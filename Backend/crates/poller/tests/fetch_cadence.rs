// Backend/crates/poller/tests/fetch_cadence.rs
//! DoD #3: over N cycles, full-sweep happens exactly on `poll_count % 3 == 0`.
//! DoD #9 half: a full sweep with a failing page is NOT fetch_complete. Uses a
//! wiremock SPX so `fetch_bookings` really runs; asserts fetch_complete gating.
//! Also proves fast-detect is genuinely OFF by default (no HTTP at all) and
//! works when explicitly enabled (DoD #1).
use poller::{fast_detect, should_full_sweep, sweep, FetchOutcome, PollerConfig};
use spx_client::{SpxClient, SpxCookies};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { csrftoken: "C".into(), ..Default::default() }
}

fn page_body(ids: &[&str]) -> serde_json::Value {
    let list: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({ "booking_id": id, "booking_name": id }))
        .collect();
    serde_json::json!({ "data": { "list": list } })
}

#[tokio::test]
async fn full_sweep_zero_failures_is_fetch_complete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&["A", "B"])))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig { max_pages: 2, ..Default::default() };
    let out: FetchOutcome = sweep(&client, &cookies(), &cfg, 3, true).await;
    assert!(out.fetch_complete, "full sweep, all pages ok → fetch_complete");
    assert!(out.was_full_sweep);
    assert!(out.spx_id_set.contains("A"));
}

#[tokio::test]
async fn rotating_window_is_never_fetch_complete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&["A"])))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig::default();
    let out = sweep(&client, &cookies(), &cfg, 1, false).await;
    assert!(!out.fetch_complete, "a rotating window never gates anti-drift");
}

#[tokio::test]
async fn full_sweep_with_a_failing_page_is_not_complete() {
    let server = MockServer::start().await;
    // Page 1 (pageno=1) → 500, others → 200. fetch_bookings sends pageno in body;
    // match all POSTs and fail with 500 so at least one page fails.
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig { max_pages: 3, ..Default::default() };
    let out = sweep(&client, &cookies(), &cfg, 3, true).await;
    assert!(out.page_failures >= 1);
    assert!(!out.fetch_complete, "any page failure forces fetch_complete=false (REG-500 guard)");
}

#[tokio::test]
async fn full_sweep_cadence_over_nine_cycles_hits_exactly_three_times() {
    // Drives should_full_sweep across poll_count 1..=9 with full_sync_every=3 and
    // no pool change; asserts exactly 3 full sweeps (3, 6, 9) — matching the
    // real caller-side decision poll_once will make each cycle.
    let full_sync_every = 3u64;
    let mut full_sweeps = 0u32;
    for poll_count in 1..=9u64 {
        if should_full_sweep(poll_count, full_sync_every, false) {
            full_sweeps += 1;
        }
    }
    assert_eq!(full_sweeps, 3, "exactly poll_count % 3 == 0 hits over 9 cycles");
}

#[tokio::test]
async fn fast_detect_default_off_makes_zero_http_calls() {
    // No mock registered at all. fast_detect_pages defaults to 0, so this must
    // return empty without ever touching the network; asserted directly via
    // `received_requests()` below (an unmounted MockServer 404s any stray
    // request rather than panicking, so we check the request count, not just
    // the return value — that's the real proof "no HTTP at all" happened).
    let server = MockServer::start().await;
    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig::default();
    assert_eq!(cfg.fast_detect_pages, 0, "fast-detect must default to OFF");

    let bookings = fast_detect(&client, &cookies(), &cfg).await;
    assert!(bookings.is_empty(), "fast-detect OFF must return empty with no HTTP");
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn fast_detect_enabled_fetches_page_one_through_configured_pages() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&["Z"])))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig { fast_detect_pages: 2, ..Default::default() }; // explicitly enabled

    let bookings = fast_detect(&client, &cookies(), &cfg).await;
    // 2 pages fetched (1 and 2), each returning one booking with id "Z".
    assert_eq!(bookings.len(), 2);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
