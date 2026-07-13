// Backend/crates/poller/tests/restore_before_first_poll.rs
//! Fase 4's CP-7 contract, made a hard, tested guarantee by
//! `ensure_restored_then_spawn`: `executor.restore_accepted_ids` MUST complete
//! BEFORE the account's first poll ever dispatches — otherwise a booking
//! genuinely accepted in a PREVIOUS process lifetime (Layer 1 starts empty on
//! every restart; Layer 2's Redis claim key may have expired past its 600s
//! TTL) would be re-accepted on the very first cycle after a restart.
//!
//! Simulates the restart race directly: seed the durable Layer-3 ZSET
//! (`spx:accepted:<acct>`) with booking "9001" — exactly what a REAL restart
//! would leave behind from a previous process lifetime — then build a BRAND
//! NEW `PollerState` (empty in-proc dedup, as a freshly-started process would
//! have) and drive it through the real `ensure_restored_then_spawn` →
//! `spawn_account_loop` → `poll_once` → `dispatch_booking` chain against a
//! wiremock SPX that returns BOTH the already-accepted booking ("9001") and a
//! genuinely new one ("9002") as still "pending" in SPX's own bidding pool
//! (exactly what SPX would show while its own UI hasn't caught up yet, or if
//! the previous accept's claim already expired).
//!
//! Real PG @ 15432 + real Redis @ 16379 + wiremock SPX.
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

/// Poll the DB (real `.await`, no ad hoc nested executor) until `spx_id`'s
/// status equals `want`, or the budget is exhausted.
async fn wait_for_status(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    spx_id: &str,
    want: &str,
    budget: Duration,
) -> bool {
    let start = std::time::Instant::now();
    loop {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM bookings WHERE tenant_id = $1 AND spx_id = $2")
                .bind(tenant_id)
                .bind(spx_id)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
        if row.map(|(s,)| s == want).unwrap_or(false) {
            return true;
        }
        if start.elapsed() > budget {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_booking_accepted_before_restart_is_never_re_accepted_after_restore() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Restore Contract Tenant")
        .bind(format!("restore-{tenant_id}"))
        .execute(&pool)
        .await
        .expect("insert tenant");

    let rule_uuid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accept_rules (id, tenant_id, name, mode, coc_only, max_accept_count, accepted_count) \
         VALUES ($1, $2, 'COC catch-all', 'filter', true, 0, 0)",
    )
    .bind(rule_uuid)
    .bind(tenant_id)
    .execute(&pool)
    .await
    .expect("insert accept_rule");

    // The "previous process lifetime": SPX id 9001 was already won. Only the
    // Layer-3 durable ZSET is seeded here — Layer 1 (in-proc) is NOT, because
    // that is exactly what a real process restart wipes.
    let account_id = format!("t{}", Uuid::new_v4().simple());
    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    executor
        .record_durable_accept(&account_id, "9001")
        .await
        .expect("seed durable accept (simulates a pre-restart win)");

    // wiremock SPX: both 9001 (already ours, per Layer 3) and 9002 (genuinely
    // new) are still visible as pending in SPX's own bidding pool.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "list": [
                { "booking_id": "9001", "booking_name": "SPXID-RESTORED-A" },
                { "booking_id": "9002", "booking_name": "SPXID-NEW-B" }
            ] }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0 })))
        .mount(&server)
        .await;

    let client = Arc::new(SpxClient::new(server.uri()).expect("client"));
    let config = PollerConfig {
        poll_interval_ms: 5000, // long enough that only ONE cycle runs in this test's budget
        page_size: 10,
        max_pages: 1,       // one page covers both seeded bookings
        full_sync_every: 1, // poll_count=1 is always a full sweep regardless of pool_changed
        fast_detect_pages: 0,
        sweep_hedge_ms: 0,
        notif_watch_ms: 0,
        notif_watch_concurrency: 1,
        primary_account_id: String::new(),
    };
    let shared = Arc::new(PollerShared {
        executor: executor.clone(),
        client,
        pool: pool.clone(),
        config,
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
    });

    // A BRAND NEW PollerState: empty in-proc dedup (`AccountDedupState::new()`
    // inside `PollerState::new`), the coc_only rule wired, agency_id > 0 (so
    // `accept_booking` actually issues HTTP rather than short-circuiting to
    // Auth on a zero agency id).
    let mut st = PollerState::new(
        account_id.clone(),
        tenant_id,
        1,
        SpxCookies::default(),
        SecretString::from("u"),
        SecretString::from("p"),
    );
    // This test proves the restore-before-first-poll contract, not Task 7b's
    // relogin trigger — seed `last_daily_relogin_day` to TODAY so the
    // empty-sentinel daily trigger doesn't add an unrelated relogin attempt
    // (against an unreachable sidecar + this test's SPX mock, which only has
    // the list/accept endpoints mounted) on cycle 1.
    st.last_daily_relogin_day = poller::wib_day(chrono::Utc::now());
    let compiled = core_domain::CompiledRule::compile(&core_domain::AcceptRule {
        id: rule_uuid.to_string(),
        name: "COC catch-all".into(),
        enabled: true,
        priority: 0,
        mode: core_domain::RuleMode::Filter,
        conditions: core_domain::RuleConditions {
            coc_only: true,
            ..Default::default()
        },
    });
    st.rules = Arc::new(vec![compiled]);
    st.rule_meta = Arc::new(vec![poller::RuleMeta {
        uuid: rule_uuid,
        cap: 0,
        accepted_count: 0,
        name: "COC catch-all".into(),
    }]);
    assert!(
        !st.dedup.is_known("9001"),
        "sanity: the FRESH PollerState's Layer 1 must start with NO knowledge of 9001 — \
         the restore contract's whole job is to fix that BEFORE the first poll"
    );

    // THE call under test: restore MUST complete before the loop's first
    // cycle can dispatch anything.
    let handle = ensure_restored_then_spawn(shared.clone(), st).await;

    // Booking 9002 (genuinely new) must be accepted — proves the pipeline
    // genuinely ran and isn't just silently skipping everything.
    let accepted_b =
        wait_for_status(&pool, tenant_id, "9002", "accepted", Duration::from_secs(5)).await;
    assert!(
        accepted_b,
        "9002 (genuinely new) must be accepted within the budget — the pipeline must actually run"
    );

    // THE assertion: 9001 (restored) must NEVER have been dispatched — its row
    // stays 'pending' (poll_once's `st.dedup.is_known` pre-check short-circuits
    // it before `dispatch_booking` is ever called), and the accept endpoint
    // must have received exactly ONE request (for 9002), never for 9001.
    let (status_a,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id = $1 AND spx_id = '9001'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("9001 row must exist (upserted by the sweep)");
    assert_eq!(
        status_a, "pending",
        "9001 must NEVER be dispatched (still 'pending', not 'accepted') — the restore contract \
         must have made Layer 1 already know it BEFORE the first poll ran"
    );

    let accept_requests = server.received_requests().await.expect("recorder enabled");
    let accept_calls: Vec<serde_json::Value> = accept_requests
        .iter()
        .filter(|r| r.url.path() == "/api/line_haul/agency/booking/bidding/accept")
        .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
        .collect();
    assert_eq!(
        accept_calls.len(),
        1,
        "exactly one real accept HTTP call may ever be made (for 9002 only): saw {accept_calls:?}"
    );
    assert_eq!(
        accept_calls[0].get("booking_id").and_then(|v| v.as_i64()),
        Some(9002),
        "the one accept call must be for 9002, NEVER for the already-restored 9001: {accept_calls:?}"
    );

    handle.join.abort();
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
