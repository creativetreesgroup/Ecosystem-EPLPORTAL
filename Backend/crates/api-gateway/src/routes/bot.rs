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

/// No `Debug` derive: this struct carries the plaintext `waha_api_key` from the request body,
/// and a `Debug`/`{:?}` impl is exactly the kind of thing a future `tracing::debug!(?body)`
/// could reach for without realizing it logs a raw credential (review finding — same footgun
/// class already fixed for `UpsertCredential`, Fase 6b Task 2, and `notifier::BotSettings`,
/// Fase 6b's whole-branch-review fix).
#[derive(Deserialize)]
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
/// `webhook_url` before storing.
///
/// **Security-review correction (post-Task-6 fix):** the original implementation of this fn used
/// hand-rolled string parsing (find the host as the substring up to the first `/`/`:`/`?`/`#`
/// after the scheme prefix) instead of a real URL parser. An automated security review correctly
/// flagged two real bypasses that hand-rolled parsing cannot catch:
/// 1. **Userinfo confusion**: `http://looks-safe.example@169.254.169.254/` — the substring up to
///    the first `/`/`:`/`?`/`#` is `looks-safe.example@169.254.169.254`, which matches none of
///    the blocklist patterns, so the OLD check passed it — but a real HTTP client parses
///    `169.254.169.254` (the part AFTER `@`) as the actual connection target, i.e. the AWS/GCP
///    metadata endpoint. The fix rejects any URL carrying userinfo outright.
/// 2. **IPv6 bracket notation**: `http://[::1]:3000/` — IPv6 addresses contain `:` themselves, so
///    splitting on the first `:` (intended to strip a port) instead truncated INSIDE the bracket
///    notation, producing a garbled host string that never matched the literal `"::1"` check.
///
/// Now uses `url::Url::parse` (already resolved transitively in this workspace's dependency tree
/// via `reqwest`/`wreq` — promoting it to a direct dependency here changes nothing about the
/// resolved dependency graph) and inspects the parsed `Host` enum directly, using `std::net`'s own
/// well-tested `is_loopback`/`is_private`/`is_link_local`/`is_unspecified` predicates instead of
/// hand-rolled octet comparisons. Empty string is considered safe ("disabled"/"keep previous").
fn is_safe_outbound_url(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return true;
    }
    let Ok(url) = url::Url::parse(s) else {
        return false;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    // Reject embedded credentials outright — `url::Url` correctly separates userinfo from host,
    // but a URL that specifies userinfo at all has no legitimate use here, and different HTTP
    // clients have historically disagreed on how ambiguous userinfo is handled — safer to reject
    // than to trust "the parsed host field looks fine so we're safe."
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host() else {
        return false;
    };
    match host {
        url::Host::Domain(d) => {
            let dl = d.to_lowercase();
            if dl == "localhost" || dl.ends_with(".local") {
                return false;
            }
        }
        url::Host::Ipv4(ip) => {
            if ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() {
                return false;
            }
        }
        url::Host::Ipv6(ip) => {
            if ip.is_loopback() || ip.is_unspecified() {
                return false;
            }
            let seg0 = ip.segments()[0];
            // Unique-local fc00::/7 (top 7 bits 1111_110) and link-local fe80::/10.
            if (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80 {
                return false;
            }
            // IPv4-mapped (::ffff:a.b.c.d) — check the embedded v4 address too.
            if let Some(v4) = ip.to_ipv4_mapped() {
                if v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
                {
                    return false;
                }
            }
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
