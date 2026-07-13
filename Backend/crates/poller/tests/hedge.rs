// Backend/crates/poller/tests/hedge.rs
//! DoD #2: hedged fetch is OFF by default (no backup ever fires, behavior ==
//! plain fetch), and when ENABLED it fires a backup for a slow page. These
//! end-to-end tests go through a real wiremock HTTP socket (so they cannot use
//! `tokio::time::pause` — see `hedge::race_tests` in `src/hedge.rs` for the
//! paused-virtual-time proof of the delay-gating itself, which uses in-memory
//! futures instead of a socket). Also proves hedging is gated to the
//! forced-full-sweep path only, never the rotating window, via `poller::sweep`.
use poller::{hedge_fires_since_reset, hedged_page, sweep, PollerConfig};
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { csrftoken: "C".into(), ..Default::default() }
}
fn ok_body() -> serde_json::Value {
    serde_json::json!({ "data": { "list": [{ "booking_id": "A", "booking_name": "A" }] } })
}

// `hedge_fires_since_reset` reads a single process-global static, and cargo
// runs the tests in this binary concurrently by default. Serialize them so
// exact-count assertions on that shared counter can't race against each
// other (see the identical rationale in `src/hedge.rs::race_tests`). An
// async-aware `tokio::sync::Mutex` avoids `clippy::await_holding_lock`.
static HEDGE_FIRES_TEST_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn default_off_never_hedges() {
    let _guard = HEDGE_FIRES_TEST_LOCK.lock().await;
    let _ = hedge_fires_since_reset(); // reset
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let page = hedged_page(&client, &cookies(), 1, 50, 0).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(hedge_fires_since_reset(), 0, "hedge OFF must never fire a backup");
    // Direct proof, not just the counter: exactly one request reached the
    // server (a real backup would have produced a second request).
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn enabled_fires_backup_on_slow_page() {
    let _guard = HEDGE_FIRES_TEST_LOCK.lock().await;
    let _ = hedge_fires_since_reset();
    let server = MockServer::start().await;
    // Respond after 300ms so a 50ms hedge window elapses → backup fires.
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_body())
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let page = hedged_page(&client, &cookies(), 1, 50, 50).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(hedge_fires_since_reset(), 1, "a page slower than hedge_ms must fire exactly one backup");
    // Direct proof: the primary AND exactly one backup both reached the
    // server (both were slow, so both are still in flight / completing).
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn full_sweep_hedges_but_rotating_window_never_does() {
    // DoD: hedging is gated to the FORCED FULL-SWEEP path only (correction
    // #1 / design note — `forceFullSweep ? SPX_SWEEP_HEDGE_MS : 0`), never the
    // steady-state rotating window, even when `sweep_hedge_ms` is configured
    // ON. A slow page during a rotating-window poll must NEVER trigger a
    // backup; the same slow page during a full sweep must.
    let _guard = HEDGE_FIRES_TEST_LOCK.lock().await;
    let _ = hedge_fires_since_reset();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_body())
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig {
        max_pages: 1,
        sweep_hedge_ms: 50,
        ..Default::default()
    };

    // Rotating window (full=false): hedge_ms configured but must be ignored.
    let _ = hedge_fires_since_reset();
    let out = sweep(&client, &cookies(), &cfg, 1, false).await;
    assert!(!out.was_full_sweep);
    assert_eq!(
        hedge_fires_since_reset(),
        0,
        "a rotating window must never hedge, even with sweep_hedge_ms configured"
    );

    // Full sweep (full=true): the SAME slow page must now hedge.
    let _ = hedge_fires_since_reset();
    let out = sweep(&client, &cookies(), &cfg, 3, true).await;
    assert!(out.was_full_sweep);
    assert_eq!(
        hedge_fires_since_reset(),
        1,
        "a forced full sweep must hedge a page slower than sweep_hedge_ms"
    );
}
