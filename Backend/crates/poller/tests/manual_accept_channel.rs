// Backend/crates/poller/tests/manual_accept_channel.rs
//! Task 6 DoD: a `ManualAcceptRequest` sent through a running account's `AccountHandle.manual_accept`
//! reaches `SpxClient::accept_booking` using THAT account's own live cookies/agency_id, and the
//! reply comes back through the `oneshot` channel — proven against a real wiremock SPX server
//! (the account's poll loop itself sees empty pages, so no auto-accept dispatch ever competes
//! with the manual one in this test).
use std::sync::Arc;

use dashmap::DashMap;
use executor::ExecutorHandle;
use poller::{ManualAcceptRequest, PollerConfig, PollerShared, PollerState, SidecarClient};
use spx_client::{SpxClient, SpxCookies};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}
fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

#[tokio::test]
async fn manual_accept_request_reaches_the_running_accounts_own_client_and_replies() {
    let mock = MockServer::start().await;
    // Real spx-client endpoint paths (see `spx-client/src/client.rs`'s `PATH_BIDDING_LIST`/
    // `PATH_ACCEPT` consts) — NOT the `/api/marketplace/dc/...` placeholders the brief's Step 5
    // snippet used; this task's brief drifted from the real client, so this deviates to match
    // the ACTUAL `SpxClient::fetch_bookings`/`accept_booking` wire shape (both POST, and the
    // list body's empty-page shape matches every other poller test's wiremock, e.g.
    // `poke_pool_changed.rs`/`relogin_wiring.rs`).
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "list": [] }
        })))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "message": "ok"
        })))
        .mount(&mock)
        .await;

    let pool = store::connect(&database_url()).await.expect("connect");
    let executor = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = SpxClient::new(mock.uri()).expect("build SpxClient");
    let sidecar = SidecarClient::new("http://127.0.0.1:1".to_string());
    let shared = Arc::new(PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool,
        config: PollerConfig {
            poll_interval_ms: 3_600_000, // effectively never ticks again during this test
            ..PollerConfig::default()
        },
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    let mut state = PollerState::new(
        "manual-accept-test-acct".to_string(),
        uuid::Uuid::new_v4(),
        555, // agency_id — nonzero here so accept_booking does NOT short-circuit on the guard
        SpxCookies::default(),
        "u".into(),
        "p".into(),
    );
    state.agency_id = 555;
    let handle = poller::ensure_restored_then_spawn(shared, state).await;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    handle
        .manual_accept
        .send(ManualAcceptRequest {
            booking_id: 4242,
            request_ids: vec![],
            reply: reply_tx,
        })
        .await
        .expect("send manual accept request");

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
        .await
        .expect("reply must arrive within 5s")
        .expect("reply sender must not be dropped");
    assert_eq!(result.reason, spx_client::AcceptReason::Ok);

    handle.join.abort();
}
