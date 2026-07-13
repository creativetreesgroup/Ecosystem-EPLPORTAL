// Backend/crates/poller/tests/notif_watch.rs
//! DoD #4: (a) a change in the pending signal pokes the poll loop, proven
//! against a REAL `tokio::sync::Notify` (Task 1's poke mechanism) driven by
//! `spawn_notif_watcher` talking to a real wiremock HTTP socket (so the
//! backoff-timing proofs live in `src/notif_watch.rs`'s paused-time unit
//! tests instead — wiremock's delays are real wall-clock, not governed by
//! `tokio::time::pause`); (b) the backoff math is the exact 250-floor/
//! 5000-cap ramp (pure function, no I/O).
use std::sync::Arc;

use poller::{next_backoff, spawn_notif_watcher, PollerConfig};
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { csrftoken: "C".into(), ..Default::default() }
}

#[test]
fn backoff_is_exact_reference_ramp() {
    let mut b = 0u64;
    let mut seq = Vec::new();
    for _ in 0..6 {
        b = next_backoff(b);
        seq.push(b);
    }
    assert_eq!(seq, vec![250, 500, 1000, 2000, 4000, 5000]);
}

#[tokio::test]
async fn change_in_signal_pokes_the_loop() {
    let server = MockServer::start().await;
    // notification_count returns a CHANGING pending count on each call so the
    // watcher detects a change and pokes. (Two distinct bodies via up-to.)
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/notification/pn/pending/read/count"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "count": 1 } })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/notification/pn/pending/read/count"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "count": 9 } })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/count_v2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "pending": 0 } })))
        .mount(&server)
        .await;

    let client = Arc::new(SpxClient::new(server.uri()).unwrap());
    let poke = Arc::new(Notify::new());
    let cfg = PollerConfig { notif_watch_ms: 10, notif_watch_concurrency: 1, ..Default::default() };
    let handle = spawn_notif_watcher(client, cookies(), cfg, poke.clone());

    // Wait (real, short) for at least two ticks so the signal changes 1 → 9.
    let poked = tokio::time::timeout(std::time::Duration::from_secs(2), poke.notified())
        .await
        .is_ok();
    handle.abort();
    assert!(poked, "a changed pending signal must poke the poll loop");
}

#[tokio::test]
async fn failing_counter_endpoint_never_pokes_and_the_watcher_survives() {
    // Both counter endpoints fail (404) -> `read_pending_signal` always
    // returns None -> the watcher must back off, never poke, and never
    // panic/exit (proven indirectly: the handle is still running — not
    // finished — right up until we abort it).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/notification/pn/pending/read/count"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/count_v2"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = Arc::new(SpxClient::new(server.uri()).unwrap());
    let poke = Arc::new(Notify::new());
    let cfg = PollerConfig { notif_watch_ms: 10, notif_watch_concurrency: 2, ..Default::default() };
    let handle = spawn_notif_watcher(client, cookies(), cfg, poke.clone());

    let poked = tokio::time::timeout(std::time::Duration::from_millis(500), poke.notified())
        .await
        .is_ok();
    assert!(!poked, "a fully-failed counter tick must never poke");
    assert!(!handle.is_finished(), "the watcher must keep running (backing off), not exit, on errors");
    handle.abort();
}

#[tokio::test]
async fn notif_watch_ms_zero_disables_the_watcher_and_makes_zero_http_calls() {
    // `notif_watch_ms == 0` is the documented "watcher disabled" escape
    // hatch: the spawned task must return immediately (no infinite loop) and
    // must never touch the network at all.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/notification/pn/pending/read/count"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "count": 1 } })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/count_v2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "pending": 0 } })))
        .mount(&server)
        .await;

    let client = Arc::new(SpxClient::new(server.uri()).unwrap());
    let poke = Arc::new(Notify::new());
    let cfg = PollerConfig { notif_watch_ms: 0, ..Default::default() };
    let handle = spawn_notif_watcher(client, cookies(), cfg, poke);

    tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("disabled watcher task must return promptly, not loop forever")
        .expect("disabled watcher task must not panic");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "a disabled watcher must never make an HTTP call"
    );
}
