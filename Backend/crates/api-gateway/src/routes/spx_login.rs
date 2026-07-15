// Backend/crates/api-gateway/src/routes/spx_login.rs
//! POST /auth/spx-login/:label — a CONNECTIVITY TEST for a stored SPX
//! credential (tiers 2/3 only, no browser/tier-1, no cookie persistence —
//! see this task's design note in the plan for why). Never returns the
//! password or the resulting session cookies.
use axum::extract::{Extension, Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::decrypt_agency_password;
use spx_client::crypto::secret::ExposeSecret;

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

    // Tiers 2/3 only (see this task's design note — no tier 1 in a
    // synchronous HTTP route).
    if let Some(mut jar) = state
        .poller
        .client
        .api_login(&cred.username, password.expose_secret())
        .await
    {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return Ok(Json(SpxLoginResult {
            ok: true,
            tier: Some("api"),
        }));
    }
    if let Some(mut jar) = state
        .poller
        .client
        .form_login(&cred.username, password.expose_secret())
        .await
    {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return Ok(Json(SpxLoginResult {
            ok: true,
            tier: Some("form"),
        }));
    }
    Ok(Json(SpxLoginResult { ok: false, tier: None }))
}

pub fn spx_login_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{label}", post(test_login))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
