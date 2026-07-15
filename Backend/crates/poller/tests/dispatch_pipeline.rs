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
use poller::{
    dispatch_booking, DispatchResult, PollerShared, PollerState, RuleMeta, SidecarClient,
};
use secrecy::SecretString;
use spx_client::{normalize_booking, SpxClient, SpxCookies};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> (SecretString, SecretString) {
    (SecretString::from("u"), SecretString::from("p"))
}

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
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
    };

    let account_id = format!("t{}", Uuid::new_v4().simple());
    let (username, password) = creds();
    let mut st = PollerState::new(
        account_id,
        tenant_id,
        42,
        SpxCookies::default(),
        username,
        password,
    );

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
    let (username2, password2) = creds();
    let mut st2 = PollerState::new(
        st.account_id.clone(),
        tenant_id,
        42,
        SpxCookies::default(),
        username2,
        password2,
    );
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
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
    };
    let account_id = format!("t{}", Uuid::new_v4().simple());
    let (username, password) = creds();
    let mut st = PollerState::new(
        account_id,
        tenant_id,
        42,
        SpxCookies::default(),
        username,
        password,
    );
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

/// `Auth` outcome: the Layer-2 claim (and, for a capped rule, the inflight
/// quota slot) must be released — same as `Transient` — because a 401/403
/// means the accept never fired server-side, so there is no double-accept to
/// protect against, and holding the slot would spuriously block retries and
/// inflate the quota count against unrelated tickets on the same rule. Proven
/// by attempting a fresh `try_claim_auto` for the same account/spx_id/rule
/// immediately afterward and asserting it succeeds (`Proceed`), not
/// `AlreadyClaimed`/`QuotaFull` — a bare "release.await'd" check wouldn't
/// prove the key was actually gone from Redis.
#[tokio::test]
async fn auth_outcome_releases_claim_and_leaves_booking_pending() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let rule_uuid = Uuid::new_v4();
    // Capped at 1 so a leftover inflight slot would be observable via
    // QuotaFull on the follow-up claim attempt, not just AlreadyClaimed.
    sqlx::query(
        "INSERT INTO accept_rules (id, tenant_id, name, mode, coc_only, max_accept_count, accepted_count) \
         VALUES ($1, $2, 'COC catch-all capped', 'filter', true, 1, 0)",
    )
    .bind(rule_uuid)
    .bind(tenant_id)
    .execute(&pool)
    .await
    .expect("insert accept_rule");

    let spx_id = format!("SPXID-AUTH-{}", Uuid::new_v4().simple());
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
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    let client = Arc::new(SpxClient::new(server.uri()).expect("client"));
    let shared = PollerShared {
        executor: executor.clone(),
        client,
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
    };
    let account_id = format!("t{}", Uuid::new_v4().simple());
    let (username, password) = creds();
    let mut st = PollerState::new(
        account_id.clone(),
        tenant_id,
        42,
        SpxCookies::default(),
        username,
        password,
    );
    let compiled = CompiledRule::compile(&core_domain::AcceptRule {
        id: rule_uuid.to_string(),
        name: "COC catch-all capped".into(),
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
        cap: 1,
        accepted_count: 0,
        name: "COC catch-all capped".into(),
    }]);

    let outcome = dispatch_booking(&shared, &mut st, &normalized).await;
    assert_eq!(outcome, DispatchResult::Auth);
    assert!(
        st.consecutive_401s >= 3,
        "Auth outcome must jump consecutive_401s to the relogin threshold"
    );

    let (status, rule_matched): (String, Option<Uuid>) = sqlx::query_as(
        "SELECT status, rule_matched FROM bookings WHERE tenant_id = $1 AND spx_id = $2",
    )
    .bind(tenant_id)
    .bind(&spx_id)
    .fetch_one(&pool)
    .await
    .expect("fetch booking row");
    assert_eq!(status, "pending", "Auth must not write a terminal status");
    assert_eq!(rule_matched, None, "Auth must not bind a rule uuid");

    // Proof, via DIRECT (non-claiming) Redis reads — deliberately NOT via a
    // `try_claim_auto` reclaim of the SAME spx_id: that call is not a
    // read-only check, it IS a claim, and under cap=1 it would legitimately
    // re-populate the inflight set as a side effect of successfully claiming
    // — which would silently sabotage a later "different ticket" check by
    // consuming the very capacity being tested (this is exactly what an
    // earlier version of this test got wrong: it chained a same-id reclaim
    // before the different-id check, so the different-id check's `QuotaFull`
    // was caused by the test's OWN reclaim, not a real leak). Reading the
    // raw key/set state instead has no side effects.
    let mut raw_con = redis::Client::open(redis_url())
        .expect("redis client")
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection");
    {
        use redis::AsyncCommands;
        let claim_exists: bool = raw_con
            .exists(format!("spx:claim:{account_id}:{spx_id}"))
            .await
            .expect("EXISTS claim key");
        assert!(
            !claim_exists,
            "the claim key must be deleted after an Auth outcome, proving the SAME ticket \
             can be retried once Task 7's relogin recovers the session"
        );
        let inflight_members: Vec<String> = raw_con
            .smembers(format!("spx:inflight:{account_id}:{rule_uuid}"))
            .await
            .expect("SMEMBERS inflight set");
        assert!(
            inflight_members.is_empty(),
            "the capped-rule inflight quota slot must also be released after an Auth outcome \
             (found leaked members: {inflight_members:?}) — a leaked slot would spuriously \
             block OTHER, unrelated tickets on the same rule with QuotaFull even though \
             nothing was ever actually accepted against this rule's cap"
        );
    }

    // End-to-end confirmation: with the slot genuinely free, a real claim for
    // a DIFFERENT, unrelated ticket on the same capped(=1) rule must succeed.
    let other_spx_id = format!("SPXID-AUTH-OTHER-{}", Uuid::new_v4().simple());
    let other_claim = executor
        .try_claim_auto(&account_id, &other_spx_id, Some(rule_uuid), 1, 0)
        .await;
    assert_eq!(
        other_claim,
        executor::ClaimOutcome::Proceed,
        "a different, unrelated ticket on the same capped rule must not see spurious QuotaFull \
         after an unrelated Auth outcome"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// Final review finding: `dispatch::ensure_self_email` must never permanently
/// cache an empty self-email off a transient fetch failure. `ensure_self_email`
/// is private to `dispatch.rs`, so this drives it indirectly through the public
/// `dispatch_booking` -> `AcceptReason::AgencyDup` path (`self_email` is `pub`
/// on `PollerState`, so its caching state is directly observable). The accept
/// endpoint always returns the "your agency already accepted" AgencyDup
/// trigger, and the bidding op-log always names an unrelated `@`-bearing
/// rival on the FIRST probe attempt (avoiding `verify_agency_dup`'s
/// 500/1500ms inconclusive-retry sleeps) so classification is decided solely
/// by the SPX-side data, independent of `self_email` — the fix under test is
/// purely about the CACHING behavior, observed by reusing the SAME
/// `PollerState` across two dispatches.
#[tokio::test]
async fn ensure_self_email_does_not_permanently_cache_a_transient_fetch_failure() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let rule_uuid = Uuid::new_v4();
    insert_rule(&pool, tenant_id, rule_uuid).await;

    let server = MockServer::start().await;
    // Accept endpoint: always the AgencyDup trigger (retcode 150399, matches
    // spx-client's RE_AGENCY_DUP — see accept.rs's `eight_real_cases`).
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 150399,
            "message": "Operation failed. Your agency already accepted this request before."
        })))
        .mount(&server)
        .await;
    // Bidding op-log: a definite `@`-bearing rival acceptor on the very first
    // probe attempt, so `verify_agency_dup` resolves immediately every time
    // (no retry sleeps), and — crucially — resolves the SAME way (a loss to
    // this unrelated rival) regardless of what `self_email` happens to be on
    // either call below.
    Mock::given(method("GET"))
        .and(path("/api/line_haul/agency/booking/bidding/log/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0,
            "data": { "list": [
                { "booking_operation_type": 4, "operator": "rival@otheragency.com", "create_time": 1000 }
            ]}
        })))
        .mount(&server)
        .await;
    // Deliberately NO mock yet for any of the 6 SPX profile-fallback
    // candidates `fetch_profile` tries — every one 404s (wiremock's default
    // for an unmatched route), so `fetch_self_email` fails and
    // `ensure_self_email` must return "" WITHOUT caching it.

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
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
    };

    let account_id = format!("t{}", Uuid::new_v4().simple());
    let (username, password) = creds();
    let mut st = PollerState::new(
        account_id,
        tenant_id,
        42,
        SpxCookies::default(),
        username,
        password,
    );
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

    assert_eq!(st.self_email, None, "sanity: starts uncached");

    // (1) First dispatch: the self-email fetch fails (nothing mocked yet for
    // the 6 profile candidates) -> the AgencyDup path runs with self_email = "".
    let spx_id_1 = format!("SPXID-SELFMAIL-1-{}", Uuid::new_v4().simple());
    let raw1 = serde_json::json!({ "booking_id": spx_id_1, "booking_name": spx_id_1 });
    let normalized1 = normalize_booking(&raw1);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            spx_id: spx_id_1.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw1.clone(),
        },
    )
    .await
    .expect("seed booking row 1");

    let first = dispatch_booking(&shared, &mut st, &normalized1).await;
    assert_eq!(
        first,
        DispatchResult::LostToAgency {
            rival: "rival@otheragency.com".to_string()
        },
        "the rival op-log entry never matches self_email (empty or otherwise), so this must \
         classify as a loss to the unrelated rival"
    );
    assert_eq!(
        st.self_email, None,
        "THE FIX: a transient self-email fetch failure must NOT be cached as Some(\"\"). The \
         pre-fix code left this as Some(String::new()) after exactly this call, permanently \
         disabling agency-dup detection (every future call short-circuits to \"\") for the rest \
         of this account's poller task lifetime — this assertion is what the pre-fix code fails."
    );

    // (2) Mount the primary profile endpoint so the NEXT fetch attempt
    // succeeds (proving the fix actually retries instead of ever
    // short-circuiting on a cached empty value).
    Mock::given(method("GET"))
        .and(path("/api/basicserver/agency/account/current_user/basic_info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "email": "  Us@OurCompany.COM " }
        })))
        .mount(&server)
        .await;

    // (3) Second dispatch, reusing the SAME `st` (so any cache persists
    // across calls) -> the fetch now succeeds and must both return AND cache
    // the real, normalized (trimmed + lowercased) email.
    let spx_id_2 = format!("SPXID-SELFMAIL-2-{}", Uuid::new_v4().simple());
    let raw2 = serde_json::json!({ "booking_id": spx_id_2, "booking_name": spx_id_2 });
    let normalized2 = normalize_booking(&raw2);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            spx_id: spx_id_2.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw2.clone(),
        },
    )
    .await
    .expect("seed booking row 2");

    let second = dispatch_booking(&shared, &mut st, &normalized2).await;
    assert_eq!(
        second,
        DispatchResult::LostToAgency {
            rival: "rival@otheragency.com".to_string()
        },
        "still a loss to the same unrelated rival, now with a REAL self_email available — the \
         op-log rival never matched either way, so only the caching behavior below differs"
    );
    assert_eq!(
        st.self_email,
        Some("us@ourcompany.com".to_string()),
        "once a genuine fetch succeeds, the real (trimmed+lowercased) email must be cached — \
         proving RECOVERY is possible after a prior transient failure, which the pre-fix \
         permanent-\"\" cache could never do (it would have returned \"\" forever)"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
