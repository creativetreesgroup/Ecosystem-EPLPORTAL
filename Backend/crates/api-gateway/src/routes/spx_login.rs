// Backend/crates/api-gateway/src/routes/spx_login.rs
//! POST /auth/spx-login/:label — a CONNECTIVITY TEST for a stored SPX
//! credential (tiers 2/3 only, no browser/tier-1, no cookie persistence —
//! see this task's design note in the plan for why). Never returns the
//! password or the resulting session cookies.
use axum::extract::{Extension, Path, State};
use axum::routing::post;
use axum::{Json, Router};
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::decrypt_agency_password;
use spx_client::crypto::secret::ExposeSecret;

/// Seconds a `(tenant, label)` must wait between connectivity tests. Also the
/// TTL of the in-flight lock: a second click while the first (up to ~80s)
/// login is still running fails the `NX` claim and is rejected immediately.
const TEST_COOLDOWN_SECS: u64 = 60;

fn cooldown_key(tenant_id: Uuid, label: &str) -> String {
    format!("spx:spx_login_rl:{tenant_id}:{label}")
}

#[derive(Debug, Serialize)]
pub struct SpxLoginResult {
    pub ok: bool,
    pub tier: Option<&'static str>,
}

async fn test_login(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
) -> Result<Json<SpxLoginResult>, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    let cred = store::agency_credentials::find_by_label(&state.poller.pool, user.tenant_id, &label)
        .await?
        .ok_or(ApiError::NotFound)?;
    let nonce: [u8; 12] = cred
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::Internal("stored nonce is not 12 bytes".to_string()))?;
    let password = decrypt_agency_password(&state.master_key, user.tenant_id, &cred.ciphertext, &nonce)
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?;

    // Rate-limit + in-flight guard. Claimed AFTER the 403/404/decrypt checks
    // so a request that never reaches SPX doesn't burn the window. `SET NX EX`
    // is atomic (no read-then-write race) — mirrors `otp.rs`'s cooldown idiom.
    // The `NX` failure means either a test is currently running (the key is
    // held for the whole login) or one finished within the last 60s.
    let key = cooldown_key(user.tenant_id, &label);
    let mut redis = state.redis.clone();
    let claim_opts = SetOptions::default()
        .with_expiration(SetExpiry::EX(TEST_COOLDOWN_SECS))
        .conditional_set(ExistenceCheck::NX);
    let acquired: bool = redis
        .set_options(&key, "1", claim_opts)
        .await
        .map_err(|e| ApiError::Internal(format!("redis cooldown claim: {e}")))?;
    if !acquired {
        return Err(ApiError::TooManyRequests(
            "test koneksi sedang berjalan atau baru saja dijalankan, coba lagi sebentar".to_string(),
        ));
    }

    // Tiers 2/3 only (no tier 1 in a synchronous HTTP route).
    let result = run_login(&state, &cred.username, password.expose_secret()).await;

    // Best-effort: reset the window to 60s from COMPLETION, not from the start
    // of a login that may have taken ~80s. Ignore errors — the login already
    // ran and its outcome is what the caller wants; a failed refresh at worst
    // lets the window lapse slightly early. The `NX` claim above already
    // provided the in-flight guarantee.
    let refresh_opts = SetOptions::default().with_expiration(SetExpiry::EX(TEST_COOLDOWN_SECS));
    let _: Result<(), redis::RedisError> = redis.set_options(&key, "1", refresh_opts).await;

    Ok(Json(result))
}

async fn run_login(state: &AppState, username: &str, password: &str) -> SpxLoginResult {
    if let Some(mut jar) = state.poller.client.api_login(username, password).await {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return SpxLoginResult {
            ok: true,
            tier: Some("api"),
        };
    }
    if let Some(mut jar) = state.poller.client.form_login(username, password).await {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return SpxLoginResult {
            ok: true,
            tier: Some("form"),
        };
    }
    SpxLoginResult {
        ok: false,
        tier: None,
    }
}

pub fn spx_login_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{label}", post(test_login))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
