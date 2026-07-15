// Backend/crates/api-gateway/src/routes/otp.rs
//! POST /auth/request-aa-otp, POST /auth/verify-aa-otp — the OTP gate that
//! (once 6c ships) authorizes the autoAccept:false->true transition. This
//! task only PRODUCES the spx:pwverify:<tenant>:<user> proof on success;
//! nothing in this plan consumes it yet.
//!
//! `crate::otp` (Task 4, top-level module) is the pure Redis logic; THIS
//! module (`crate::routes::otp`) is just the HTTP handlers wrapping it —
//! deliberately named the same leaf segment (`otp`) at two different module
//! paths, matching the task brief's own plan, so the `use crate::otp::{...}`
//! import below is unambiguous (it always resolves to the top-level module;
//! a module can never `use` itself under its own crate-relative path).
use axum::extract::{Extension, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::otp::{self, OtpRequestError, OtpVerifyError};
use crate::state::AppState;
use notifier::waha::send_to_waha_many;
use spx_client::crypto::secret::ExposeSecret;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};

/// The exact, shared "OTP delivery isn't wired up yet" message — returned
/// both when the `site_settings` row is entirely missing (no 6d write route
/// exists yet in this sub-phase) AND when the row exists but its `wa_number`
/// is blank. One message, one string literal, so the two checks can never
/// silently drift apart.
const OTP_NOT_CONFIGURED_MSG: &str = "OTP delivery is not configured for this tenant";

#[derive(Debug, Serialize)]
pub struct OtpOk {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct VerifyOtpRequest {
    pub code: String,
}

async fn request_otp(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<OtpOk>, ApiError> {
    require_permission(&user, Permission::ArmAutoAccept)?;

    let code = otp::request(&mut state.redis, user.tenant_id, user.portal_user_id)
        .await
        .map_err(|e| match e {
            // 429, not 409: this is a resend cooldown ("come back in a bit"),
            // the same rate-limit-shaped rejection family as
            // `OtpVerifyError::TooManyAttempts` below and as
            // `middleware::rate_limit`'s `tower_governor` login limiter
            // elsewhere in this crate — not a resource-state conflict, which
            // `Conflict`/409 is reserved for elsewhere in this crate (e.g.
            // `agency_credentials`'s unique-label violation).
            OtpRequestError::TooSoon => {
                ApiError::TooManyRequests("otp already requested, try again shortly".to_string())
            }
            OtpRequestError::Redis(e) => ApiError::Internal(e.to_string()),
        })?;

    let bot = load_bot_settings(&state, user.tenant_id).await?;
    if bot.wa_number.trim().is_empty() {
        return Err(ApiError::BadRequest(OTP_NOT_CONFIGURED_MSG.to_string()));
    }
    let text = format!("Kode verifikasi TOWER Anda: {code} (berlaku 3 menit)");
    let (sent, _failed) = send_to_waha_many(&bot, &bot.wa_number, &text).await;
    if sent == 0 {
        tracing::warn!(tenant_id = %user.tenant_id, "OTP WAHA send reported zero delivered");
    }
    Ok(Json(OtpOk { ok: true }))
}

async fn verify_otp(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<VerifyOtpRequest>,
) -> Result<Json<OtpOk>, ApiError> {
    require_permission(&user, Permission::ArmAutoAccept)?;
    otp::verify(&mut state.redis, user.tenant_id, user.portal_user_id, &body.code)
        .await
        .map_err(|e| match e {
            // Uniform 401 for both — don't help an attacker distinguish "no
            // code was ever requested" from "a code exists but this one's
            // wrong", same caution this crate's password-login route already
            // applies to bad-username-vs-bad-password. The threat model here
            // is admittedly thinner than a password's (a 6-digit OTP capped
            // at 5 attempts, 180s TTL — see `crate::otp::verify`'s own doc
            // comment on why a timing side-channel barely matters here), but
            // there is no upside to leaking the distinction either, so the
            // same uniform-401 discipline is applied anyway.
            OtpVerifyError::NoActiveCode | OtpVerifyError::WrongCode => ApiError::Unauthorized,
            // 429, matching `OtpRequestError::TooSoon` above: this is the
            // attempt-cap half of the same rate-limit-shaped error family,
            // not a resource conflict.
            OtpVerifyError::TooManyAttempts => {
                ApiError::TooManyRequests("too many attempts, request a new code".to_string())
            }
            OtpVerifyError::Redis(e) => ApiError::Internal(e.to_string()),
        })?;
    Ok(Json(OtpOk { ok: true }))
}

/// Builds a `notifier::BotSettings` from the `site_settings` row Fase 3's
/// `WahaSettings` already owns (`key = SITE_SETTINGS_KEY = "waha_settings"`,
/// verified by grepping `spx-client/tests/waha_settings_pg.rs` — that test's
/// own INSERT uses this exact constant, not a string literal, so there is no
/// ambiguity about the established key convention).
///
/// `WahaSettings` (Fase 3) only carries WAHA connection info (URL, session,
/// encrypted API key) plus this task's own `wa_number` addition — it has no
/// `wa_group`/`webhook_url`/`portal_label`/`enabled` fields, because those
/// remain unmanaged by any route until 6d's `site_settings` CRUD ships. This
/// fn defaults all four to their zero value on the constructed `BotSettings`
/// rather than guessing at a shape 6d hasn't defined yet — this route only
/// ever sends a personal-number OTP text (`bot.wa_number`), never touches
/// `wa_group`/`webhook_url`/`enabled`/`portal_label` (`send_to_waha_many` is
/// called directly here, not `notifier::notify_*`, so the `enabled` kill
/// switch those wrapper fns gate on is irrelevant to this call path).
async fn load_bot_settings(
    state: &AppState,
    tenant_id: uuid::Uuid,
) -> Result<notifier::BotSettings, ApiError> {
    let value = store::site_settings::get(&state.poller.pool, tenant_id, SITE_SETTINGS_KEY)
        .await?
        .ok_or_else(|| ApiError::BadRequest(OTP_NOT_CONFIGURED_MSG.to_string()))?;

    let waha = WahaSettings::from_json_value(&value)
        .map_err(|e| ApiError::Internal(format!("corrupt waha_settings site_settings row: {e}")))?;

    let api_key = waha
        .decrypt_api_key(&state.master_key, tenant_id)
        .map_err(|e| ApiError::Internal(format!("failed to decrypt waha api key: {e:?}")))?;

    Ok(notifier::BotSettings {
        enabled: false,
        webhook_url: String::new(),
        wa_group: String::new(),
        wa_number: waha.wa_number,
        waha_url: waha.waha_url,
        waha_api_key: api_key.expose_secret().to_string(),
        waha_session: waha.waha_session,
        portal_label: String::new(),
    })
}

pub fn otp_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/request-aa-otp", post(request_otp))
        .route("/verify-aa-otp", post(verify_otp))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
