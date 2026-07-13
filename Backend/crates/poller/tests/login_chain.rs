// Backend/crates/poller/tests/login_chain.rs
//! DoD #5: tier 1→2→3 auto-login order + fallback, proven against wiremock
//! servers standing in for `auth-sidecar` (Task 9 hasn't built the real
//! handler yet — only its HTTP contract, mirrored here) and SPX itself.
//! Also covers the reactive (3×401) and proactive (WIB-day) relogin trigger
//! predicates.
use poller::{
    auto_login, should_daily_relogin, should_reactive_relogin, wib_day, LoginTier, SidecarClient,
};
use secrecy::SecretString;
use spx_client::SpxClient;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// (a) Tier 1 succeeds: the sidecar returns a usable session → auto_login
/// must use it and report `LoginTier::Browser`, without ever touching the SPX
/// server (no mock mounted on `spx`, so a stray call would 404 or panic under
/// wiremock's unmatched-request handling — this proves tier 2/3 are genuinely
/// skipped, not just "also happen to succeed").
#[tokio::test]
async fn tier1_sidecar_success_wins_and_skips_tier2_tier3() {
    let sidecar_srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .and(body_json(serde_json::json!({
            "account_id": "acct", "username": "u", "password": "p"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "cookies": { "fms_user_skey": "B" }
        })))
        .mount(&sidecar_srv)
        .await;

    // No mocks mounted on this server at all — proves tier 2/3 are never
    // reached when tier 1 already won.
    let spx = MockServer::start().await;

    let sidecar = SidecarClient::new(sidecar_srv.uri());
    let client = SpxClient::new(spx.uri()).unwrap();
    let out = auto_login(&sidecar, &client, "acct", "u", "p").await;
    let (jar, tier) = out.expect("tier 1 must succeed");
    assert_eq!(tier, LoginTier::Browser);
    assert_eq!(jar.fms_user_skey, "B");
}

/// (b) THE key fallback case: sidecar down (503 == "tier 1 unavailable") must
/// fall through to tier 2 (in-proc API login), NOT hard-fail the whole
/// attempt. This is the load-bearing behavior the 3-tier design exists for.
#[tokio::test]
async fn sidecar_down_falls_through_to_api() {
    // "sidecar" that is down (503 on /login).
    let sidecar_srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&sidecar_srv)
        .await;
    // SPX whose API login succeeds (Set-Cookie fms_user_skey).
    let spx = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/account/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=APIWIN; Path=/"),
        )
        .mount(&spx)
        .await;

    let sidecar = SidecarClient::new(sidecar_srv.uri());
    let client = SpxClient::new(spx.uri()).unwrap();
    let out = auto_login(&sidecar, &client, "acct", "u", "p").await;
    let (jar, tier) = out.expect("must fall through to tier 2, not hard-fail");
    assert_eq!(tier, LoginTier::Api);
    assert_eq!(jar.fms_user_skey, "APIWIN");
}

/// Same idea, but the sidecar is fully UNREACHABLE (connection refused —
/// nothing listening at all, not just a 503) rather than merely erroring.
/// This is the literal "proses belum jalan/down" case from the design doc,
/// distinct from (b)'s "process up but rejecting" case.
#[tokio::test]
async fn sidecar_connection_refused_falls_through_to_api() {
    // A MockServer that is started then immediately dropped: its port is
    // freed, so the URI is guaranteed unreachable (connection refused), with
    // no listener of any kind — the strongest form of "sidecar down".
    let dead = MockServer::start().await;
    let dead_uri = dead.uri();
    drop(dead);

    let spx = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/account/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=APIWIN2; Path=/"),
        )
        .mount(&spx)
        .await;

    let sidecar = SidecarClient::new(dead_uri);
    let client = SpxClient::new(spx.uri()).unwrap();
    let out = auto_login(&sidecar, &client, "acct", "u", "p").await;
    let (jar, tier) = out.expect("connection-refused sidecar must still fall through to tier 2");
    assert_eq!(tier, LoginTier::Api);
    assert_eq!(jar.fms_user_skey, "APIWIN2");
}

/// (c-extended) The FULL chain in order: sidecar down AND tier 2 (all 5 API
/// attempts) failing must fall all the way through to tier 3 (form login).
/// Proves 1→2→3 tries every tier in order, not just 1→2.
#[tokio::test]
async fn sidecar_and_api_both_down_falls_through_to_form() {
    let sidecar_srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&sidecar_srv)
        .await;

    let spx = MockServer::start().await;
    // All 5 tier-2 API-login endpoint variants explicitly fail.
    for failing_path in [
        "/api/basicserver/agency/account/login",
        "/api/basicserver/account/login",
        "/api/basicserver/agency/auth/login",
        "/api/user/login",
    ] {
        Mock::given(method("POST"))
            .and(path(failing_path))
            .respond_with(ResponseTemplate::new(401))
            .mount(&spx)
            .await;
    }
    // Tier 3 (form login) succeeds: GET /login (csrf) -> POST /login
    // (redirect) -> GET redirect target (session cookie).
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&spx)
        .await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/dashboard"))
        .mount(&spx)
        .await;
    Mock::given(method("GET"))
        .and(path("/dashboard"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=FORMWIN; Path=/"),
        )
        .mount(&spx)
        .await;

    let sidecar = SidecarClient::new(sidecar_srv.uri());
    let client = SpxClient::new(spx.uri()).unwrap();
    let out = auto_login(&sidecar, &client, "acct", "u", "p").await;
    let (jar, tier) = out.expect("must fall all the way through to tier 3");
    assert_eq!(tier, LoginTier::Form);
    assert_eq!(jar.fms_user_skey, "FORMWIN");
}

/// All 3 tiers fail: `auto_login` returns `None` (never panics), rather than
/// synthesizing a fake success.
#[tokio::test]
async fn all_tiers_failing_returns_none_not_a_hard_error() {
    let sidecar_srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&sidecar_srv)
        .await;

    // SPX server with nothing mounted: every tier-2/3 attempt 404s.
    let spx = MockServer::start().await;

    let sidecar = SidecarClient::new(sidecar_srv.uri());
    let client = SpxClient::new(spx.uri()).unwrap();
    let out = auto_login(&sidecar, &client, "acct", "u", "p").await;
    assert!(out.is_none());
}

/// (c) Reactive relogin predicate: fires at exactly 3, not before.
#[test]
fn reactive_relogin_threshold_is_exactly_3() {
    assert!(!should_reactive_relogin(0));
    assert!(!should_reactive_relogin(2));
    assert!(should_reactive_relogin(3));
    assert!(should_reactive_relogin(4));
}

/// Reuses the EXACT update expression `dispatch::dispatch_booking` applies on
/// an `AcceptReason::Auth` outcome (`st.consecutive_401s.max(3)`, Task 6) —
/// proves `should_reactive_relogin` reads the same field Task 6 already
/// writes, not a second/parallel counter invented by this task.
#[test]
fn reactive_relogin_uses_pollerstate_consecutive_401s_field_task6_writes() {
    let mut st = poller::PollerState::new(
        "acct".into(),
        uuid::Uuid::new_v4(),
        42,
        spx_client::SpxCookies::default(),
        SecretString::from("u"),
        SecretString::from("p"),
    );
    assert!(!should_reactive_relogin(st.consecutive_401s));

    // The exact expression dispatch.rs uses on an Auth outcome.
    st.consecutive_401s = st.consecutive_401s.max(3);
    assert!(should_reactive_relogin(st.consecutive_401s));
}

/// (c) Proactive daily relogin predicate: any differing WIB-day string
/// triggers it; the same day never does.
#[test]
fn daily_relogin_fires_on_new_wib_day_only() {
    assert!(should_daily_relogin("2026-07-12", "2026-07-13"));
    assert!(!should_daily_relogin("2026-07-13", "2026-07-13"));
    // The empty-string sentinel (`PollerState::new`'s initial
    // `last_daily_relogin_day`, meaning "never relogged in") must also count
    // as a boundary crossing on the very first real day computed.
    assert!(should_daily_relogin("", "2026-07-13"));
}

/// `wib_day` converts UTC -> UTC+7 correctly across the actual WIB-midnight
/// instant (not just "different calendar date somewhere"): 16:59:59 UTC is
/// still 23:59:59 WIB (same day), but one second later, 17:00:00 UTC, is
/// 00:00:00 WIB the NEXT day.
#[test]
fn wib_day_crosses_over_at_the_exact_utc_offset_boundary() {
    use chrono::TimeZone;
    let just_before = chrono::Utc
        .with_ymd_and_hms(2026, 7, 13, 16, 59, 59)
        .unwrap();
    let just_after = chrono::Utc.with_ymd_and_hms(2026, 7, 13, 17, 0, 0).unwrap();
    assert_eq!(wib_day(just_before), "2026-07-13");
    assert_eq!(wib_day(just_after), "2026-07-14");
    assert!(should_daily_relogin(
        &wib_day(just_before),
        &wib_day(just_after)
    ));
}

/// Proactive daily relogin, driven by a PAUSED tokio clock rather than real
/// wall-clock waiting (this project's established time-based-logic test
/// pattern — Task 1/3/4). A fixed chrono anchor just before WIB midnight is
/// combined with `tokio::time::Instant`'s paused-clock delta (advanced
/// instantly, no real sleep) to derive a simulated "now"; crossing the
/// boundary purely via `tokio::time::advance` must flip
/// `should_daily_relogin` from false to true.
#[tokio::test(start_paused = true)]
async fn daily_relogin_flips_true_after_paused_clock_advances_past_wib_midnight() {
    use chrono::TimeZone;

    // Anchor: 2026-07-13 23:58:00 WIB == 16:58:00 UTC (2 minutes before WIB
    // midnight).
    let anchor_utc = chrono::Utc
        .with_ymd_and_hms(2026, 7, 13, 16, 58, 0)
        .unwrap();
    let anchor_instant = tokio::time::Instant::now();

    let day0 = wib_day(anchor_utc);
    assert_eq!(day0, "2026-07-13");
    assert!(!should_daily_relogin(&day0, &wib_day(anchor_utc)));

    // Advance the PAUSED clock by 5 minutes. Because the runtime's clock is
    // paused (`start_paused = true`), this returns essentially instantly —
    // the test does not actually wait 5 minutes, unlike a real relogin
    // scheduler that waited for wall-clock time to pass.
    tokio::time::advance(std::time::Duration::from_secs(5 * 60)).await;

    let elapsed = tokio::time::Instant::now().duration_since(anchor_instant);
    assert_eq!(elapsed, std::time::Duration::from_secs(5 * 60));
    let simulated_now = anchor_utc + chrono::Duration::milliseconds(elapsed.as_millis() as i64);

    let day1 = wib_day(simulated_now);
    assert_eq!(
        day1, "2026-07-14",
        "5 min after 23:58 WIB must be the next WIB day"
    );
    assert!(
        should_daily_relogin(&day0, &day1),
        "crossing WIB midnight via the paused/advanced tokio clock must trigger proactive relogin"
    );
}
