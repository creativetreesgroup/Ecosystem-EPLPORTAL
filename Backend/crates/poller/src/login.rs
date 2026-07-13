// Backend/crates/poller/src/login.rs
//! 3-tier auto-login orchestration. Tier 1 (browser) is delegated to the
//! separate `auth-sidecar` process over internal HTTP (poller depends on NO
//! browser crate — correction #2). Tiers 2/3 are in-proc `spx-client` HTTP.
//! Order 1→2→3; a down/unreachable sidecar falls through to tier 2 (never a
//! hard failure). Reactive relogin at 3×401; proactive once-per-WIB-day.
//!
//! Scope note: this module builds the login TOOLKIT (the tier chain +
//! trigger predicates) and proves each piece in isolation. Wiring it into the
//! live `schedule::poll_once` loop (reading `st.consecutive_401s`/
//! `st.last_daily_relogin_day`, calling `auto_login`, writing the resulting
//! cookies back into `st.cookies`) needs credentials + a `SidecarClient`
//! handle that don't exist on `PollerShared`/`PollerState` yet — that
//! integration is out of this task's file list (see the plan's Task 7
//! section) and is left for the watchdog/bootstrap wiring task that already
//! owns "recreate poller: read saved cookies or trigger a full auto-login".
use serde::{Deserialize, Serialize};
use spx_client::{SpxClient, SpxCookies};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginTier {
    Browser,
    Api,
    Form,
}

#[derive(Serialize)]
struct SidecarLoginReq<'a> {
    account_id: &'a str,
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct SidecarLoginResp {
    ok: bool,
    #[serde(default)]
    cookies: Option<SidecarCookies>,
}

/// The 11 SPX cookie fields as returned by auth-sidecar's /login.
#[derive(Deserialize, Default)]
struct SidecarCookies {
    #[serde(default)]
    fms_user_skey: String,
    #[serde(default)]
    fms_user_id: String,
    #[serde(default)]
    fms_user_agency_id: String,
    #[serde(default)]
    csrftoken: String,
    #[serde(default)]
    spx_uk: String,
    #[serde(default)]
    spx_cid: String,
    #[serde(default)]
    spx_uid: String,
    #[serde(default)]
    spx_agid: String,
    #[serde(default)]
    spx_st: String,
    #[serde(default)]
    ds: String,
    #[serde(default)]
    spx_admin_device_id: String,
}

impl From<SidecarCookies> for SpxCookies {
    fn from(c: SidecarCookies) -> Self {
        SpxCookies {
            fms_user_skey: c.fms_user_skey,
            fms_user_id: c.fms_user_id,
            fms_user_agency_id: c.fms_user_agency_id,
            csrftoken: c.csrftoken,
            spx_uk: c.spx_uk,
            spx_cid: c.spx_cid,
            spx_uid: c.spx_uid,
            spx_agid: c.spx_agid,
            spx_st: c.spx_st,
            ds: c.ds,
            spx_admin_device_id: c.spx_admin_device_id,
        }
    }
}

/// HTTP client for tier 1 — `auth-sidecar`'s (Task 9's) browser-login
/// endpoint. Internal-network HTTP, so no Chrome-impersonating transport is
/// needed (unlike `SpxClient`, which talks to SPX itself).
pub struct SidecarClient {
    base_url: String,
    http: wreq::Client,
}

impl SidecarClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        // Plain client (internal HTTP, no impersonation needed). A build
        // failure is treated as "sidecar unavailable" by falling back to a
        // default-constructed client; keep it simple with a default build().
        let http = wreq::Client::builder().build().unwrap_or_default();
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    /// Tier 1: ask the sidecar to browser-login. Any error / non-2xx /
    /// ok:false / missing-skey → None (caller falls through to tier 2). This
    /// is the load-bearing fallback behavior the whole 3-tier design is built
    /// around — a down/unreachable sidecar must NEVER abort the login
    /// attempt outright (design doc: "poller fallback ke tier 2 — TIDAK
    /// boleh gagal keseluruhan hanya karena sidecar down").
    pub async fn login(
        &self,
        account_id: &str,
        username: &str,
        password: &str,
    ) -> Option<SpxCookies> {
        let url = format!("{}/login", self.base_url.trim_end_matches('/'));
        let req = SidecarLoginReq {
            account_id,
            username,
            password,
        };
        let res = self.http.post(url).json(&req).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let parsed: SidecarLoginResp = res.json().await.ok()?;
        if !parsed.ok {
            return None;
        }
        let jar: SpxCookies = parsed.cookies?.into();
        if jar.fms_user_skey.is_empty() {
            return None;
        }
        Some(jar)
    }
}

/// Try tier 1 → 2 → 3, IN ORDER. A sidecar-unreachable/None falls through to
/// tier 2 (never aborts the whole attempt). Runs `fetch_spx_cid` after
/// whichever tier wins, if `spx_cid` came back empty.
pub async fn auto_login(
    sidecar: &SidecarClient,
    client: &SpxClient,
    account_id: &str,
    username: &str,
    password: &str,
) -> Option<(SpxCookies, LoginTier)> {
    // Tier 1 — browser via sidecar (primary; port the reference's order
    // exactly, even though tier-1's implementation now lives in a separate
    // process).
    if let Some(mut jar) = sidecar.login(account_id, username, password).await {
        client.fetch_spx_cid(&mut jar).await;
        return Some((jar, LoginTier::Browser));
    }
    // Tier 2 — API login (in-proc).
    if let Some(mut jar) = client.api_login(username, password).await {
        client.fetch_spx_cid(&mut jar).await;
        return Some((jar, LoginTier::Api));
    }
    // Tier 3 — form login (in-proc).
    if let Some(mut jar) = client.form_login(username, password).await {
        client.fetch_spx_cid(&mut jar).await;
        return Some((jar, LoginTier::Form));
    }
    None
}

/// Reactive relogin fires at 3 consecutive 401s (correction #5). Reads
/// `PollerState.consecutive_401s` — the SAME counter `dispatch::dispatch_booking`
/// jumps to 3 on an `AcceptReason::Auth` outcome (Task 6); this is a pure
/// predicate over that field, not a second/parallel counter.
pub fn should_reactive_relogin(consecutive_401s: u32) -> bool {
    consecutive_401s >= 3
}

/// Proactive relogin once per WIB day: true iff `now_wib_day` differs from
/// the last day a relogin actually ran (`PollerState.last_daily_relogin_day`,
/// itself produced by `wib_day`). This makes "once per WIB day" a day-string
/// comparison rather than a fixed-interval timer — a caller that checks this
/// every poll cycle will trigger within one cycle of WIB midnight regardless
/// of how long the process has been running, and never double-fires within
/// the same WIB day even across restarts (as long as the day string was
/// persisted/reloaded).
pub fn should_daily_relogin(last_day_wib: &str, now_wib_day: &str) -> bool {
    last_day_wib != now_wib_day
}

/// YYYY-MM-DD in WIB (UTC+7, no DST — Indonesia has observed no DST since
/// 1963, so a fixed offset is exact, not an approximation).
pub fn wib_day(now: chrono::DateTime<chrono::Utc>) -> String {
    let wib = chrono::FixedOffset::east_opt(7 * 3600).expect("valid +7");
    now.with_timezone(&wib).format("%Y-%m-%d").to_string()
}
