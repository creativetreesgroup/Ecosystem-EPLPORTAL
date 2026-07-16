// Backend/crates/poller/tests/poke_pool_changed.rs
//! Cross-task hand-off (Task 4 review → Task 6): the notif watcher only pokes
//! (`Arc<Notify>::notify_one()`) — it never sets a flag. THIS crate's job is to
//! translate "was this cycle's wake-up caused by a poke?" into
//! `pool_changed=true` for the NEXT `poll_once` call, so a poke reliably forces
//! a full sweep (DoD #4) instead of being pure motion with no effect on what
//! gets fetched.
//!
//! Drives the REAL `ensure_restored_then_spawn` → `spawn_account_loop` →
//! `poll_once` chain (not a mimic) against a wiremock SPX + real Redis/PG, and
//! proves the wiring via an unambiguous page-range signal:
//!
//! - `max_pages = 3`, `full_sync_every` huge (never fires by cadence alone).
//! - Cycle 1 (poll_count=1, no poke yet): `window_pages(1, 3) = (2, 4)` — a
//!   rotating window that NEVER includes page 1.
//! - Cycle 2 (poll_count=2): rotating window would be `window_pages(2, 3) =
//!   (3, 5)` — ALSO never includes page 1. So the only way page 1 is ever
//!   requested on cycle 2 is a FULL sweep (`1..=max_pages`), which only
//!   happens if `pool_changed` was true — which (with fast-detect off and no
//!   cadence hit) can only come from the poke.
//!
//! Real time (no `start_paused`): the interval is set generously long (3s) so
//! the test's own poke — fired the instant cycle 1's requests are observed —
//! is unambiguously "during the sleep phase", comfortably before the interval
//! would have elapsed on its own.
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use executor::ExecutorHandle;
use poller::{ensure_restored_then_spawn, PollerConfig, PollerShared, PollerState, SidecarClient};
use secrecy::SecretString;
use spx_client::{SpxClient, SpxCookies};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn cookies() -> SpxCookies {
    SpxCookies {
        csrftoken: "C".into(),
        ..Default::default()
    }
}

/// Poll `received_requests()` until at least `min` have landed, or fail after
/// a generous real-time budget. Small real sleeps (not `tokio::time::advance`
/// — this test intentionally does NOT pause the clock, since it exercises real
/// wiremock network I/O end to end).
async fn wait_for_request_count(
    server: &MockServer,
    min: usize,
    budget: Duration,
) -> Vec<wiremock::Request> {
    let start = std::time::Instant::now();
    loop {
        let reqs = server.received_requests().await.expect("recorder enabled");
        if reqs.len() >= min {
            return reqs;
        }
        if start.elapsed() > budget {
            panic!(
                "timed out waiting for {min} requests, only saw {}",
                reqs.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn pagenos(reqs: &[wiremock::Request]) -> Vec<i64> {
    reqs.iter()
        .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
        .filter_map(|v| v.get("pageno").and_then(|p| p.as_i64()))
        .collect()
}

#[tokio::test]
async fn poke_during_sleep_forces_a_full_sweep_on_the_next_cycle() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Poke Pool Changed Tenant")
        .bind(format!("poke-{tenant_id}"))
        .execute(&pool)
        .await
        .expect("insert tenant");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "list": [] } })),
        )
        .mount(&server)
        .await;

    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    let client = Arc::new(SpxClient::new(server.uri()).expect("client"));
    let config = PollerConfig {
        poll_interval_ms: 3000, // generous: the poke must win well before this
        page_size: 10,
        max_pages: 3,
        full_sync_every: 1_000_000, // cadence alone must never fire in this test
        fast_detect_pages: 0,       // fast-detect OFF — only the poke can force a full sweep
        sweep_hedge_ms: 0,
        notif_watch_ms: 0,
        notif_watch_concurrency: 1,
        primary_account_id: String::new(),
    };
    let shared = Arc::new(PollerShared {
        executor,
        client,
        pool: pool.clone(),
        config,
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    let account_id = format!("t{}", Uuid::new_v4().simple());
    let mut st = PollerState::new(
        account_id,
        tenant_id,
        42,
        cookies(),
        SecretString::from("u"),
        SecretString::from("p"),
    );
    // This test exercises the poke -> full-sweep wiring, not Task 7b's
    // relogin trigger — seed `last_daily_relogin_day` to TODAY so the
    // empty-sentinel daily trigger doesn't fire on cycle 1 and add
    // unrelated HTTP noise (a real relogin attempt against an
    // unreachable/unmounted sidecar+SPX) that would otherwise inflate the
    // raw request count this test's `wait_for_request_count` budget relies
    // on and starve cycle 2 of its window.
    st.last_daily_relogin_day = poller::wib_day(chrono::Utc::now());

    let handle = ensure_restored_then_spawn(shared.clone(), st).await;

    // Cycle 1 (poll_count=1, no poke observed yet): rotating window (2, 4) —
    // 3 requests, page 1 absent.
    let cycle1 = wait_for_request_count(&server, 3, Duration::from_secs(5)).await;
    let pages1 = pagenos(&cycle1);
    assert_eq!(
        pages1.len(),
        3,
        "cycle 1 must fetch exactly the (2,4) rotating window"
    );
    assert!(
        !pages1.contains(&1),
        "cycle 1 (no poke yet) must NEVER touch page 1: {pages1:?}"
    );

    // Poke NOW, while the loop is in its post-cycle-1 sleep (interval=3000ms,
    // and we just observed cycle 1's requests within the 5s budget above —
    // this is well inside the sleep window).
    handle.poke.notify_one();

    // Cycle 2 must appear promptly (poke cancels the 3s sleep), and — the
    // decisive signal — must include page 1, which NEITHER cadence
    // (full_sync_every huge) NOR the natural rotating window at poll_count=2
    // ((3,5)) would ever produce. Only `pool_changed=true` (from the poke)
    // forces the full-sweep branch (`1..=max_pages`).
    let all_after_cycle2 = wait_for_request_count(&server, 6, Duration::from_secs(2)).await;
    let all_pages = pagenos(&all_after_cycle2);
    assert!(
        all_pages.contains(&1),
        "cycle 2 must be a FULL sweep (page 1 present) because the poke set pool_changed=true; saw {all_pages:?}"
    );
    // And it must have completed comfortably before the 3s plain-interval
    // deadline — i.e. the poke actually cancelled the sleep, not merely
    // "eventually" happened to run.
    assert!(
        all_pages.len() >= 6,
        "cycle 2's full sweep (pages 1..=3) must have run: saw {all_pages:?}"
    );

    handle.join.abort();
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
