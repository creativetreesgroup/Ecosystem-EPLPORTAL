// Backend/crates/poller/tests/relogin_wiring.rs
//! Task 7b: proves `poll_once` actually WIRES Task 7's already-tested `login`
//! module in, rather than merely having it available unused (the gap Task 7's
//! own review found: `consecutive_401s` was written by Task 6's dispatch
//! pipeline but never read, and `should_daily_relogin` was never called from
//! anywhere in production).
//!
//! Deliberately Real-Redis/Postgres-FREE: every scenario here drives
//! `poll_once` with an EMPTY booking pool (the wiremock SPX `bidding/list`
//! endpoint always returns `{ "data": { "list": [] } }`), so the
//! upsert/dispatch loop body never executes and `run_anti_drift` no-ops
//! (never touches the DB) because the sweep here is a rotating window
//! (`fetch_complete=false` — see `antidrift.rs`'s type-gated no-op). That
//! means `store::PgPool` and `executor::ExecutorHandle` only need to be
//! CONSTRUCTIBLE (lazy pool / lazy redis client, matching how the rest of
//! this crate's non-DB tests work), not backed by live services — the
//! relogin check itself is the only thing actually exercised end to end,
//! against real wiremock SPX + sidecar servers.
use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use dashmap::DashMap;
use executor::ExecutorHandle;
use poller::{poll_once, wib_day, PollerConfig, PollerShared, PollerState, SidecarClient};
use secrecy::SecretString;
use spx_client::{SpxClient, SpxCookies};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Lazy pool: parses the URL and is immediately usable as a VALUE, but never
/// opens a real connection until a query actually runs against it. Every
/// scenario below keeps `fetch_complete=false` (rotating window, not a full
/// sweep), so `run_anti_drift` returns before ever touching this pool —
/// proven safe by these tests passing with no live Postgres reachable at
/// this bogus address.
fn lazy_pg_pool() -> store::PgPool {
    store::PgPool::connect_lazy("postgres://nouser:nopass@127.0.0.1:1/nodb")
        .expect("connect_lazy never actually dials out, so this cannot fail")
}

/// `ExecutorHandle::connect` only best-effort-tries a real Redis round trip
/// (ignored on error) and otherwise builds a lazy `redis::Client` — safe to
/// point at an address nothing listens on (port 1, refused instantly), since
/// none of these scenarios ever reach `dispatch_booking` (empty booking pool).
async fn unreachable_executor() -> Arc<ExecutorHandle> {
    Arc::new(
        ExecutorHandle::connect("redis://127.0.0.1:1/")
            .await
            .expect("ExecutorHandle::connect never hard-fails on an unreachable redis"),
    )
}

fn creds() -> (SecretString, SecretString) {
    (SecretString::from("theuser"), SecretString::from("thepass"))
}

fn old_cookies() -> SpxCookies {
    SpxCookies {
        fms_user_skey: "OLD-SESSION".into(),
        ..Default::default()
    }
}

/// Mount the empty-pool bidding/list handler every scenario needs so the
/// sweep step of `poll_once` completes without ever producing a booking to
/// dispatch.
async fn mount_empty_booking_pool(spx: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "list": [] } })),
        )
        .mount(spx)
        .await;
}

async fn build_shared(spx: &MockServer, sidecar: &MockServer) -> Arc<PollerShared> {
    let client = Arc::new(SpxClient::new(spx.uri()).expect("spx client"));
    let sidecar_client = Arc::new(SidecarClient::new(sidecar.uri()));
    Arc::new(PollerShared {
        executor: unreachable_executor().await,
        client,
        pool: lazy_pg_pool(),
        // Small max_pages + huge full_sync_every + fast_detect OFF: cycle 1
        // is guaranteed to be a rotating-window sweep, never a full sweep —
        // keeps `run_anti_drift` a no-op (see module doc) and keeps the
        // number of mocked list requests small.
        config: PollerConfig {
            poll_interval_ms: 100,
            page_size: 10,
            max_pages: 1,
            full_sync_every: 1_000_000,
            fast_detect_pages: 0,
            sweep_hedge_ms: 0,
            notif_watch_ms: 0,
            notif_watch_concurrency: 1,
            primary_account_id: String::new(),
        },
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: sidecar_client,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    })
}

fn sidecar_login_requests(reqs: &[wiremock::Request]) -> usize {
    reqs.iter().filter(|r| r.url.path() == "/login").count()
}

/// Step 3: the reactive trigger (consecutive_401s >= 3) must call
/// `login::auto_login`, replace `st.cookies` with the new session, and reset
/// `st.consecutive_401s` back to 0 — proving Task 6's counter is finally READ
/// and reset, not just written and ignored forever.
#[tokio::test]
async fn reactive_trigger_relogs_in_swaps_cookies_and_resets_counter() {
    let spx = MockServer::start().await;
    mount_empty_booking_pool(&spx).await;

    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "cookies": { "fms_user_skey": "NEW-REACTIVE-SESSION" }
        })))
        .mount(&sidecar)
        .await;

    let shared = build_shared(&spx, &sidecar).await;
    let (username, password) = creds();
    let mut st = PollerState::new(
        format!("t{}", Uuid::new_v4().simple()),
        Uuid::new_v4(),
        42,
        old_cookies(),
        username,
        password,
    );
    st.consecutive_401s = 3;
    // Healthy daily bookkeeping so ONLY the reactive trigger is in play —
    // proves this test isn't accidentally passing via the daily trigger too.
    st.last_daily_relogin_day = wib_day(Utc::now());

    poll_once(&shared, &mut st, false).await;

    assert_eq!(
        st.cookies.fms_user_skey, "NEW-REACTIVE-SESSION",
        "a successful relogin must replace st.cookies with the new session"
    );
    assert_eq!(
        st.consecutive_401s, 0,
        "a successful relogin must reset the 401 counter back to 0"
    );

    let reqs = sidecar.received_requests().await.expect("recorder enabled");
    assert_eq!(
        sidecar_login_requests(&reqs),
        1,
        "exactly one HTTP call must have reached the sidecar's /login"
    );
}

/// Step 4: when all 3 login tiers fail (sidecar down, tier2/tier3 in-proc
/// attempts unmatched on the SPX mock — same "nothing mounted" shape as
/// `login_chain.rs`'s `all_tiers_failing_returns_none_not_a_hard_error`),
/// `poll_once` must return normally (no panic), the OLD cookies must be left
/// completely untouched (never partially overwritten with an empty/None
/// jar), and `consecutive_401s` must stay >= 3 so the VERY NEXT cycle tries
/// again.
#[tokio::test]
async fn failed_relogin_does_not_panic_or_corrupt_state_and_stays_retryable() {
    let spx = MockServer::start().await;
    mount_empty_booking_pool(&spx).await;
    // Deliberately NOTHING else mounted on `spx` — every tier-2/tier-3
    // login attempt 404s, exactly like login_chain.rs's all-tiers-fail case.

    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&sidecar)
        .await;

    let shared = build_shared(&spx, &sidecar).await;
    let (username, password) = creds();
    let mut st = PollerState::new(
        format!("t{}", Uuid::new_v4().simple()),
        Uuid::new_v4(),
        42,
        old_cookies(),
        username,
        password,
    );
    st.consecutive_401s = 3;
    st.last_daily_relogin_day = wib_day(Utc::now());

    // No panic across the await — this line completing at all is part of
    // the proof.
    poll_once(&shared, &mut st, false).await;

    assert_eq!(
        st.cookies.fms_user_skey, "OLD-SESSION",
        "a fully-failed relogin must never touch/corrupt the existing cookies"
    );
    assert!(
        st.consecutive_401s >= 3,
        "a fully-failed relogin must leave consecutive_401s at/above the threshold so the \
         NEXT cycle retries — got {}",
        st.consecutive_401s
    );

    let reqs = sidecar.received_requests().await.expect("recorder enabled");
    assert_eq!(
        sidecar_login_requests(&reqs),
        1,
        "the sidecar must still have been TRIED once (tier 1 attempted before falling through)"
    );
}

/// Step 5: the daily (proactive) trigger must fire independent of the 401
/// count — a perfectly healthy account (`consecutive_401s == 0`) whose
/// `last_daily_relogin_day` is stale (yesterday, WIB) must still relogin and
/// advance the stored day to today.
#[tokio::test]
async fn daily_trigger_fires_regardless_of_401_count_and_advances_the_day() {
    let spx = MockServer::start().await;
    mount_empty_booking_pool(&spx).await;

    let sidecar = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "cookies": { "fms_user_skey": "NEW-DAILY-SESSION" }
        })))
        .mount(&sidecar)
        .await;

    let shared = build_shared(&spx, &sidecar).await;
    let (username, password) = creds();
    let mut st = PollerState::new(
        format!("t{}", Uuid::new_v4().simple()),
        Uuid::new_v4(),
        42,
        old_cookies(),
        username,
        password,
    );
    // Healthy 401 count — ONLY the daily trigger should be able to fire this.
    st.consecutive_401s = 0;
    let today = wib_day(Utc::now());
    let yesterday = wib_day(Utc::now() - ChronoDuration::days(1));
    assert_ne!(
        yesterday, today,
        "sanity: the fixture must actually cross a WIB day boundary"
    );
    st.last_daily_relogin_day = yesterday;

    poll_once(&shared, &mut st, false).await;

    assert_eq!(
        st.cookies.fms_user_skey, "NEW-DAILY-SESSION",
        "the daily trigger must have actually relogged in"
    );
    assert_eq!(
        st.last_daily_relogin_day, today,
        "a successful daily relogin must advance last_daily_relogin_day to today (WIB)"
    );

    let reqs = sidecar.received_requests().await.expect("recorder enabled");
    assert_eq!(
        sidecar_login_requests(&reqs),
        1,
        "the daily trigger must have hit the sidecar's /login exactly once"
    );
}
