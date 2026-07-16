// Backend/crates/poller/tests/notifier_wiring.rs
//! Task 10b: proves `dispatch_booking` actually reaches
//! `notifier::notify_accepted`/`notify_agency_loss` THROUGH `PollerShared.notifier`
//! — not just that the standalone `notifier` crate works in isolation (Task 10's own
//! `notifier/tests/waha_mock.rs` already proved that). Drives the REAL
//! `dispatch_booking` pipeline (same style as `dispatch_pipeline.rs`: real Redis @
//! 16379, real PG @ 15432, a wiremock SPX server) with a SECOND wiremock server
//! standing in for WAHA (`POST /api/sendText`, `notifier::waha`'s exact shape).
//!
//! `notify_accepted`/`notify_agency_loss` are `tokio::spawn`'d fire-and-forget (Task
//! 10's own design note) — not join-able from the test — so each case polls the WAHA
//! mock's `received_requests()` until the expected count lands or a generous budget
//! expires, mirroring `poke_pool_changed.rs`'s `wait_for_request_count` helper rather
//! than a blind fixed sleep.
use std::sync::Arc;
use std::time::Duration;

use core_domain::{CompiledRule, RuleBookingType, RuleConditions, RuleMode};
use dashmap::DashMap;
use executor::ExecutorHandle;
use notifier::BotSettings;
use poller::{dispatch_booking, DispatchResult, PollerShared, PollerState, RedisPublisher, RuleMeta, SidecarClient};
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
        .bind("Notifier Wiring Tenant")
        .bind(format!("notifier-wiring-{tenant_id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    tenant_id
}

async fn insert_rule(pool: &sqlx::PgPool, tenant_id: Uuid, rule_id: Uuid) {
    // Same coc_only catch-all shape as dispatch_pipeline.rs's insert_rule.
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

fn rule_meta_for(rule_uuid: Uuid) -> RuleMeta {
    RuleMeta {
        uuid: rule_uuid,
        cap: 0,
        accepted_count: 0,
        name: "COC catch-all".into(),
    }
}

fn compiled_coc_rule(rule_uuid: Uuid) -> CompiledRule {
    CompiledRule::compile(&core_domain::AcceptRule {
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
    })
}

fn waha_settings(waha_url: String) -> Arc<BotSettings> {
    Arc::new(BotSettings {
        enabled: true,
        waha_url,
        waha_api_key: "K".into(),
        waha_session: "default".into(),
        wa_group: "12036@g.us".into(),
        portal_label: "EPL".into(),
        ..Default::default()
    })
}

async fn new_state(account_id: String, tenant_id: Uuid, rule_uuid: Uuid) -> PollerState {
    let (username, password) = creds();
    let mut st = PollerState::new(
        account_id,
        tenant_id,
        42,
        SpxCookies::default(),
        username,
        password,
    );
    st.rules = Arc::new(vec![compiled_coc_rule(rule_uuid)]);
    st.rule_meta = Arc::new(vec![rule_meta_for(rule_uuid)]);
    st
}

/// Poll the WAHA mock's `received_requests()` until at least `min` have landed,
/// or fail after a generous real-time budget — the spawned `notify_*` task is
/// fire-and-forget and not join-able from the test, so this is the reliable
/// alternative to a blind fixed sleep (mirrors `poke_pool_changed.rs`'s
/// `wait_for_request_count`).
async fn wait_for_waha_count(server: &MockServer, min: usize, budget: Duration) -> Vec<wiremock::Request> {
    let start = std::time::Instant::now();
    loop {
        let reqs = server.received_requests().await.expect("recorder enabled");
        if reqs.len() >= min {
            return reqs;
        }
        if start.elapsed() > budget {
            panic!("timed out waiting for {min} WAHA requests, only saw {}", reqs.len());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn win_then_agency_loss_then_none_drive_waha_calls_correctly() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let rule_uuid = Uuid::new_v4();
    insert_rule(&pool, tenant_id, rule_uuid).await;

    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    // Task 7: a real `RedisPublisher` so `finalize_win`'s `if let Some(pub_) = &shared.redis`
    // block actually runs (proving the bot_log wiring, not just the WAHA notify wiring).
    let redis_publisher = RedisPublisher::connect(&redis_url())
        .await
        .expect("connect redis publisher");

    // ONE WAHA mock server, shared across all three cases below, so the
    // cumulative request count directly proves each case's effect (1, then a
    // 2nd, then unchanged at 2 for the None no-op case).
    let waha_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .mount(&waha_server)
        .await;
    let bot_settings = waha_settings(waha_server.uri());

    // ── Case 1: a win spawns exactly one WAHA call. ─────────────────────────
    let spx_server_win = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0 })))
        .mount(&spx_server_win)
        .await;
    let client_win = Arc::new(SpxClient::new(spx_server_win.uri()).expect("client"));
    let shared_win = PollerShared {
        executor: executor.clone(),
        client: client_win,
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        notifier: Some(bot_settings.clone()),
        redis: Some(redis_publisher.clone()),
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    };

    let spx_id_1 = format!("SPXID-NOTIFYWIN-{}", Uuid::new_v4().simple());
    let raw1 = serde_json::json!({ "booking_id": spx_id_1, "booking_name": spx_id_1 });
    let normalized1 = normalize_booking(&raw1);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "test-account".to_string(),
            spx_id: spx_id_1.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw1.clone(),
        },
    )
    .await
    .expect("seed booking row 1");

    let account_1 = format!("t{}", Uuid::new_v4().simple());
    let mut st1 = new_state(account_1, tenant_id, rule_uuid).await;
    let outcome1 = dispatch_booking(&shared_win, &mut st1, &normalized1).await;
    assert_eq!(outcome1, DispatchResult::Accepted, "a matched, unclaimed booking must be accepted");

    let reqs_after_win = wait_for_waha_count(&waha_server, 1, Duration::from_secs(3)).await;
    assert_eq!(
        reqs_after_win.len(),
        1,
        "a win must spawn exactly one WAHA sendText call through PollerShared.notifier"
    );

    // Task 7: the SAME finalize_win call must also have written a bot_log entry.
    let mut redis = redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis");
    let logs = notifier::bot_log::list(&mut redis, tenant_id, 10).await;
    assert_eq!(logs.len(), 1, "finalize_win must record exactly one bot_log entry");
    assert_eq!(logs[0].log_type, "success");
    assert_eq!(logs[0].kind.as_deref(), Some("accept"));
    notifier::bot_log::clear(&mut redis, tenant_id).await;

    // ── Case 2: a SEPARATE booking driven to AgencyDup -> LostToAgency spawns
    // exactly one (new, cumulative 2nd) WAHA call, with the rival's email in
    // the message body — proving the RIGHT data reached the notifier, not
    // just that "some" notify fired. Same wiremock-agencydup shape
    // `dispatch_pipeline.rs`'s `ensure_self_email_does_not_permanently_cache_a_transient_fetch_failure`
    // test already uses: retcode 150399 on accept, an `@`-bearing rival on the
    // bidding op-log's FIRST probe attempt (avoids verify_agency_dup's
    // 500/1500ms inconclusive-retry sleeps).
    let spx_server_loss = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 150399,
            "message": "Operation failed. Your agency already accepted this request before."
        })))
        .mount(&spx_server_loss)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/line_haul/agency/booking/bidding/log/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0,
            "data": { "list": [
                { "booking_operation_type": 4, "operator": "rival@otheragency.com", "create_time": 1000 }
            ]}
        })))
        .mount(&spx_server_loss)
        .await;
    let client_loss = Arc::new(SpxClient::new(spx_server_loss.uri()).expect("client"));
    let shared_loss = PollerShared {
        executor: executor.clone(),
        client: client_loss,
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        notifier: Some(bot_settings.clone()),
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    };

    let spx_id_2 = format!("SPXID-NOTIFYLOSS-{}", Uuid::new_v4().simple());
    let raw2 = serde_json::json!({ "booking_id": spx_id_2, "booking_name": spx_id_2 });
    let normalized2 = normalize_booking(&raw2);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "test-account".to_string(),
            spx_id: spx_id_2.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw2.clone(),
        },
    )
    .await
    .expect("seed booking row 2");

    let account_2 = format!("t{}", Uuid::new_v4().simple());
    let mut st2 = new_state(account_2, tenant_id, rule_uuid).await;
    let outcome2 = dispatch_booking(&shared_loss, &mut st2, &normalized2).await;
    assert_eq!(
        outcome2,
        DispatchResult::LostToAgency { rival: "rival@otheragency.com".to_string() },
        "the rival op-log entry never matches self_email, so this must classify as a loss"
    );

    let reqs_after_loss = wait_for_waha_count(&waha_server, 2, Duration::from_secs(3)).await;
    assert_eq!(
        reqs_after_loss.len(),
        2,
        "a LostToAgency outcome must spawn exactly one NEW (cumulative 2nd) WAHA sendText call"
    );
    let last_body: serde_json::Value =
        serde_json::from_slice(&reqs_after_loss[1].body).expect("2nd WAHA request body is JSON");
    let last_text = last_body.get("text").and_then(|v| v.as_str()).unwrap_or_default();
    assert!(
        last_text.contains("rival@otheragency.com"),
        "the agency-loss WAHA message must contain the rival's email, proving the right data \
         (not just SOME notify) reached the notifier: {last_text:?}"
    );

    // ── Case 3: `shared.notifier = None` (the default in every other test)
    // drives a win through `dispatch_booking` and must NOT spawn a WAHA call —
    // the cumulative count must stay at 2, proving the `None` no-op path (which
    // the other 11 unmodified `PollerShared` construction sites all silently
    // rely on) still genuinely works, and this task didn't accidentally make
    // notification delivery non-optional.
    let spx_server_none = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/accept"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0 })))
        .mount(&spx_server_none)
        .await;
    let client_none = Arc::new(SpxClient::new(spx_server_none.uri()).expect("client"));
    let shared_none = PollerShared {
        executor: executor.clone(),
        client: client_none,
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:0")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    };

    let spx_id_3 = format!("SPXID-NOTIFYNONE-{}", Uuid::new_v4().simple());
    let raw3 = serde_json::json!({ "booking_id": spx_id_3, "booking_name": spx_id_3 });
    let normalized3 = normalize_booking(&raw3);
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "test-account".to_string(),
            spx_id: spx_id_3.clone(),
            status: "pending".into(),
            is_coc: true,
            raw_data: raw3.clone(),
        },
    )
    .await
    .expect("seed booking row 3");

    let account_3 = format!("t{}", Uuid::new_v4().simple());
    let mut st3 = new_state(account_3, tenant_id, rule_uuid).await;
    let outcome3 = dispatch_booking(&shared_none, &mut st3, &normalized3).await;
    assert_eq!(outcome3, DispatchResult::Accepted, "a matched, unclaimed booking must still be accepted");

    // Give any (incorrectly) spawned task a chance to land before asserting
    // its absence.
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let reqs_final = waha_server.received_requests().await.expect("recorder enabled");
    assert_eq!(
        reqs_final.len(),
        2,
        "notifier: None must be a genuine no-op — no new WAHA call may be spawned"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
