// Backend/crates/poller/tests/dispatch_pipeline.rs
//! End-to-end (minus real SPX): a matched pending booking is claimed, accepted
//! (wiremock SPX returns retcode 0), committed to Layer 1, recorded durably, and
//! its booking row flips to 'accepted'. A second dispatch of the same id is a
//! Duplicate (claim shared). Proves Fase 3+4+5 compose. Real Redis @ 16379 +
//! real PG @ 15432 + wiremock SPX accept endpoint.
//!
//! Because building a full `PollerState` needs compiled rules, this test
//! constructs a single `coc_only` filter rule via `core_domain` directly and
//! drives `dispatch_booking` — not the full `poll_once` cycle (that is covered
//! separately by `poke_pool_changed.rs`, which drives the real loop end to end).
use std::sync::Arc;

use core_domain::{CompiledRule, RuleBookingType, RuleConditions, RuleMode};
use dashmap::DashMap;
use executor::ExecutorHandle;
use poller::{dispatch_booking, DispatchResult, PollerShared, PollerState, RuleMeta};
use spx_client::{normalize_booking, SpxClient, SpxCookies};
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

async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Dispatch Pipeline Tenant")
        .bind(format!("dispatch-{tenant_id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    tenant_id
}

async fn insert_rule(pool: &sqlx::PgPool, tenant_id: Uuid, rule_id: Uuid) {
    // max_accept_count=0 (unlimited, per Step 8's checklist wording), coc_only
    // filter rule — matches any SPXID booking regardless of route.
    sqlx::query(
        "INSERT INTO accept_rules (id, tenant_id, name, mode, coc_only, max_accept_count, accepted_count) \
         VALUES ($1, $2, 'COC catch-all', 'filter', true, 0, 0)",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .execute(pool)
    .await
    .expect("insert accept_rule");
}

#[tokio::test]
async fn accept_then_duplicate_and_booking_row_flips_to_accepted() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let rule_uuid = Uuid::new_v4();
    insert_rule(&pool, tenant_id, rule_uuid).await;

    // Seed the booking row itself (as the upsert step of poll_once would).
    let spx_id = format!("SPXID-DISPATCH-{}", Uuid::new_v4().simple());
    let raw =
        serde_json::json!({ "booking_id": spx_id, "booking_name": spx_id, "request_id": "555" });
    let normalized = normalize_booking(&raw);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            spx_id: spx_id.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw.clone(),
        },
    )
    .await
    .expect("seed booking row");

    // wiremock SPX: accept endpoint returns retcode 0 (Ok) unconditionally.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0 })))
        .mount(&server)
        .await;

    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    let client = Arc::new(SpxClient::new(server.uri()).expect("client"));
    let shared = PollerShared {
        executor,
        client,
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
    };

    let account_id = format!("t{}", Uuid::new_v4().simple());
    let mut st = PollerState::new(account_id, tenant_id, 42, SpxCookies::default());

    let compiled = CompiledRule::compile(&core_domain::AcceptRule {
        id: rule_uuid.to_string(),
        name: "COC catch-all".into(),
        enabled: true,
        priority: 0,
        mode: RuleMode::Filter,
        conditions: RuleConditions {
            coc_only: true,
            booking_type: RuleBookingType::All,
            ..Default::default()
        },
    });
    st.rules = Arc::new(vec![compiled]);
    st.rule_meta = Arc::new(vec![RuleMeta {
        uuid: rule_uuid,
        cap: 0,
        accepted_count: 0,
        name: "COC catch-all".into(),
    }]);

    // (1) First dispatch → Accepted.
    let first = dispatch_booking(&shared, &mut st, &normalized).await;
    assert_eq!(
        first,
        DispatchResult::Accepted,
        "a matched, unclaimed booking must be accepted"
    );

    // (4) Layer 1 now knows this id.
    assert!(
        st.dedup.is_known(&normalized.id),
        "a committed accept must be known to Layer 1"
    );

    // (3) bookings.status must have flipped to 'accepted'.
    let (status, rule_matched, latency): (String, Option<Uuid>, Option<i32>) = sqlx::query_as(
        "SELECT status, rule_matched, accept_latency_ms FROM bookings WHERE tenant_id = $1 AND spx_id = $2",
    )
    .bind(tenant_id)
    .bind(&spx_id)
    .fetch_one(&pool)
    .await
    .expect("fetch booking row");
    assert_eq!(status, "accepted");
    assert_eq!(
        rule_matched,
        Some(rule_uuid),
        "rule_matched must be the rule's real Uuid, not its name"
    );
    assert!(latency.is_some(), "accept_latency_ms must be recorded");

    // (2) Second dispatch of the SAME id, from a FRESH in-proc dedup state
    // (same account_id, so it shares the same Redis keyspace) → `Duplicate`.
    // A second call reusing the SAME `st.dedup` would short-circuit at Layer 1
    // (`try_begin_accept` sees the id in `accepted_ids` → `Skipped`, per the
    // design note's step 2) without ever reaching Layer 2 — using a fresh
    // dedup here is what actually exercises "claim shared": Layer 1 permits
    // the attempt (empty), but the durable Redis claim key from the first
    // accept (never released on a win) makes Layer 2 reject it.
    let mut st2 = PollerState::new(st.account_id.clone(), tenant_id, 42, SpxCookies::default());
    st2.rules = st.rules.clone();
    st2.rule_meta = st.rule_meta.clone();
    let second = dispatch_booking(&shared, &mut st2, &normalized).await;
    assert_eq!(
        second,
        DispatchResult::Duplicate,
        "a second claimant (fresh Layer 1, same account's Layer 2 keyspace) must be rejected as a duplicate"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// `Taken` outcome: rule_matched must stay NULL (no rule "won"), and the
/// terminal-but-not-a-uuid reason must land in `raw_data->>'accept_reason'`,
/// not the `rule_matched` FK column (the bug this task fixes proactively).
#[tokio::test]
async fn taken_outcome_leaves_rule_matched_null_and_stamps_accept_reason() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let rule_uuid = Uuid::new_v4();
    insert_rule(&pool, tenant_id, rule_uuid).await;

    let spx_id = format!("SPXID-TAKEN-{}", Uuid::new_v4().simple());
    let raw = serde_json::json!({ "booking_id": spx_id, "booking_name": spx_id });
    let normalized = normalize_booking(&raw);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            spx_id: spx_id.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw.clone(),
        },
    )
    .await
    .expect("seed booking row");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 1,
            "message": "This booking has been taken by another agency"
        })))
        .mount(&server)
        .await;

    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    let client = Arc::new(SpxClient::new(server.uri()).expect("client"));
    let shared = PollerShared {
        executor,
        client,
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
    };
    let account_id = format!("t{}", Uuid::new_v4().simple());
    let mut st = PollerState::new(account_id, tenant_id, 42, SpxCookies::default());
    let compiled = CompiledRule::compile(&core_domain::AcceptRule {
        id: rule_uuid.to_string(),
        name: "COC catch-all".into(),
        enabled: true,
        priority: 0,
        mode: RuleMode::Filter,
        conditions: RuleConditions {
            coc_only: true,
            ..Default::default()
        },
    });
    st.rules = Arc::new(vec![compiled]);
    st.rule_meta = Arc::new(vec![RuleMeta {
        uuid: rule_uuid,
        cap: 0,
        accepted_count: 0,
        name: "COC catch-all".into(),
    }]);

    let outcome = dispatch_booking(&shared, &mut st, &normalized).await;
    assert_eq!(outcome, DispatchResult::Taken);

    let (status, rule_matched, raw_data): (String, Option<Uuid>, serde_json::Value) = sqlx::query_as(
        "SELECT status, rule_matched, raw_data FROM bookings WHERE tenant_id = $1 AND spx_id = $2",
    )
    .bind(tenant_id)
    .bind(&spx_id)
    .fetch_one(&pool)
    .await
    .expect("fetch booking row");
    assert_eq!(status, "failed");
    assert_eq!(
        rule_matched, None,
        "Taken must never bind a rule uuid — no rule won this booking"
    );
    assert_eq!(
        raw_data.get("accept_reason").and_then(|v| v.as_str()),
        Some("taken_by_other"),
        "the sub-classification reason must be merged into raw_data, never the rule_matched FK"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
