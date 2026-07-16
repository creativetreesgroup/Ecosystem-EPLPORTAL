// Backend/crates/api-gateway/src/routes/bot.rs
//! `GET/PUT /bot/settings` — WAHA/n8n bot configuration. `Permission::ManageBotSettings`-gated
//! on BOTH verbs (this crate's usual convention is "GET = any session, mutation = gated" — this
//! route is a deliberate exception, matching the reference's own behavior: WAHA connection info
//! is sensitive enough that even reading it requires main-account, even with the API key masked).
use axum::extract::{Extension, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};

#[derive(Debug, Serialize)]
pub struct BotSettingsResponse {
    pub enabled: bool,
    pub webhook_url: String,
    pub wa_number: String,
    pub wa_group: String,
    pub waha_url: String,
    pub waha_session: String,
    /// The API key is NEVER echoed back — only whether one is currently configured, matching the
    /// reference's own `{...s, wahaApiKey: '', wahaApiKeySet: !!s.wahaApiKey}` masking.
    pub waha_api_key_set: bool,
}

#[derive(Debug, Deserialize)]
pub struct BotSettingsRequest {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default)]
    pub wa_number: String,
    #[serde(default)]
    pub wa_group: String,
    #[serde(default)]
    pub waha_url: String,
    #[serde(default)]
    pub waha_session: String,
    /// Blank = "keep the previously configured key" — never wipes a configured value with an
    /// empty PUT body field, matching the reference's own `bot.ts` semantic exactly.
    #[serde(default)]
    pub waha_api_key: String,
}

/// Ports the reference's `isSafeOutboundUrl` SSRF guard, applied to BOTH `waha_url` and
/// `webhook_url` before storing. No `url`-crate dependency — plain string parsing (scheme prefix
/// plus host substring up to the first `/`/`:`/`?`/`#`) is sufficient for this narrow check and
/// adds no new Cargo.toml entry. Empty string is considered safe ("disabled"/"keep previous").
fn is_safe_outbound_url(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return true;
    }
    let after_scheme = if let Some(rest) = s.strip_prefix("https://") {
        rest
    } else if let Some(rest) = s.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    let host_end = after_scheme.find(['/', ':', '?', '#']).unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host == "::1" || host.ends_with(".local") || host == "0.0.0.0" {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let o = ip.octets();
        if o[0] == 127
            || o[0] == 10
            || (o[0] == 192 && o[1] == 168)
            || (o[0] == 169 && o[1] == 254)
            || (o[0] == 172 && (16..=31).contains(&o[1]))
        {
            return false;
        }
    }
    true
}

fn empty_waha_settings() -> WahaSettings {
    WahaSettings {
        waha_url: String::new(),
        waha_session: String::new(),
        wa_number: String::new(),
        enabled: false,
        webhook_url: String::new(),
        wa_group: String::new(),
        portal_label: String::new(),
        api_key_ciphertext_b64: String::new(),
        api_key_nonce_b64: String::new(),
        key_version: 0,
    }
}

fn to_response(waha: &WahaSettings) -> BotSettingsResponse {
    BotSettingsResponse {
        enabled: waha.enabled,
        webhook_url: waha.webhook_url.clone(),
        wa_number: waha.wa_number.clone(),
        wa_group: waha.wa_group.clone(),
        waha_url: waha.waha_url.clone(),
        waha_session: waha.waha_session.clone(),
        waha_api_key_set: !waha.api_key_ciphertext_b64.is_empty(),
    }
}

async fn get_settings(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<BotSettingsResponse>, ApiError> {
    require_permission(&user, Permission::ManageBotSettings)?;
    let value = store::site_settings::get(&state.poller.pool, user.tenant_id, SITE_SETTINGS_KEY).await?;
    let waha = match value {
        Some(v) => WahaSettings::from_json_value(&v)
            .map_err(|e| ApiError::Internal(format!("corrupt waha_settings row: {e}")))?,
        None => empty_waha_settings(),
    };
    Ok(Json(to_response(&waha)))
}

async fn put_settings(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<BotSettingsRequest>,
) -> Result<Json<BotSettingsResponse>, ApiError> {
    require_permission(&user, Permission::ManageBotSettings)?;

    if !is_safe_outbound_url(&body.waha_url) {
        return Err(ApiError::BadRequest("waha_url points to a disallowed host".to_string()));
    }
    if !is_safe_outbound_url(&body.webhook_url) {
        return Err(ApiError::BadRequest("webhook_url points to a disallowed host".to_string()));
    }

    let existing = store::site_settings::get(&state.poller.pool, user.tenant_id, SITE_SETTINGS_KEY)
        .await?
        .map(|v| WahaSettings::from_json_value(&v))
        .transpose()
        .map_err(|e| ApiError::Internal(format!("corrupt waha_settings row: {e}")))?;

    let mut waha = if body.waha_api_key.trim().is_empty() {
        existing.ok_or_else(|| {
            ApiError::BadRequest("waha_api_key is required on first setup".to_string())
        })?
    } else {
        WahaSettings::encrypt_new(
            &state.master_key,
            user.tenant_id,
            &body.waha_url,
            &body.waha_session,
            &body.waha_api_key,
        )
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?
    };

    waha.waha_url = body.waha_url.trim().to_string();
    waha.waha_session = body.waha_session.trim().to_string();
    waha.wa_number = body.wa_number.trim().to_string();
    waha.enabled = body.enabled;
    waha.webhook_url = body.webhook_url.trim().to_string();
    waha.wa_group = body.wa_group.trim().to_string();

    store::site_settings::put(
        &state.poller.pool,
        user.tenant_id,
        SITE_SETTINGS_KEY,
        &waha.to_json_value(),
    )
    .await?;

    Ok(Json(to_response(&waha)))
}

pub fn bot_settings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
