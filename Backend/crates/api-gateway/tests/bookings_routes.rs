// Backend/crates/api-gateway/tests/bookings_routes.rs
//! `GET /bookings/live`, `/history`, `/:id/detail`, `/spx-log` — session-auth-only read routes,
//! and (Task 10) `POST /bookings/:id/accept` — manual accept.
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use api_gateway::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes(
        [7u8; 32],
    ))
}

async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis")
}

async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("Bookings Test Tenant")
        .bind(format!("bookings-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

/// NOTE: the task brief's transcription of this helper passed
/// `("Test User", &hash, true)` as the trailing three args of
/// `store::portal_users::create`, but that fn's real signature (see
/// `crates/store/src/portal_users.rs::create` and the already-passing
/// `crates/api-gateway/tests/portal_users_routes.rs::insert_portal_user`) is
/// `(pool, tenant_id, username, password_hash, display_name,
/// is_main_account)` — hash BEFORE display_name. Fixed here to match reality.
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", true)
        .await
        .expect("create portal user")
        .id
}

async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(vec![]),
        session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// NOTE: the task brief's transcription used `resp.cookies()`, which needs
/// reqwest's `cookies` cargo feature — not enabled for this crate (see
/// `crates/api-gateway/Cargo.toml`'s `reqwest` line: `features = ["json"]`
/// only). Every other test file in this crate (e.g.
/// `tests/portal_users_routes.rs`'s `set_cookie_header`/`cookie_pair`) reads
/// the raw `Set-Cookie` response header instead; matched here.
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(resp.status(), 200, "login must succeed");
    let set_cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("Set-Cookie present on successful login")
        .to_str()
        .expect("Set-Cookie is valid UTF-8")
        .to_string();
    set_cookie
        .split(';')
        .next()
        .expect("Set-Cookie header has at least one ';'-delimited segment")
        .to_string()
}

async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

/// Unlike `cleanup` (Postgres — always a freshly random `tenant_id`, so no cross-run collision
/// is possible), the manual-accept tests use FIXED `account_id`/`spx_id` strings, and Layer 2
/// (`spx:claim:*`)/Layer 3 (`spx:accepted:*`) live in the SAME real Redis this whole test suite
/// shares, outliving any one test run. Without this, a happy-path run's own
/// `record_durable_accept` would make every LATER run of the same test see the ticket as already
/// accepted (`try_claim_manual`'s Layer 3 check) and 409 instead of 200. Called BEFORE the test
/// body (self-healing against a prior run's leftovers, including a panicked one) as well as
/// after.
async fn cleanup_redis_claim(account_id: &str, spx_id: &str) {
    use redis::AsyncCommands;
    let mut con = test_redis_manager().await;
    let _: redis::RedisResult<()> = con.del(format!("spx:claim:{account_id}:{spx_id}")).await;
    let _: redis::RedisResult<()> = con.zrem(format!("spx:accepted:{account_id}"), spx_id).await;
}

#[tokio::test]
async fn live_and_history_split_by_status_and_require_session() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "live-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            // route_detail_list-shaped raw_data (real SPX shape) — see
            // `spx_client::normalize_booking`'s `parse_route_detail_list` (core_domain's
            // `route_parse.rs`), which reads `node_info_list[].name` under each
            // `route_detail_list[]` entry. This is priority #2 in `parse_route_stops`'s
            // fallback chain (an empty `{}` raw_data, as used before this task, yields an
            // empty `route_stops` — insufficient to prove the field is actually populated).
            raw_data: serde_json::json!({
                "route_detail_list": [{
                    "node_info_list": [
                        {"name": "Aceh DC", "address_info": {"l1": "Aceh", "l2": "Banda Aceh"}},
                        {"name": "Cileungsi DC", "address_info": {"l1": "Jabar", "l2": "Bogor"}},
                    ]
                }]
            }),
        },
    )
    .await
    .expect("seed pending booking");
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "hist-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking to mark accepted");
    store::update_booking_status(
        &pool,
        tenant_id,
        "hist-1",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: Some(5),
            auto_accepted: true,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark accepted");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // No session cookie → 401.
    let unauth = http
        .get(format!("{base}/bookings/live"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    let cookie = login_cookie(&http, &base, "owner").await;
    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(live_resp.status(), 200);
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    assert_eq!(live_body.len(), 1);
    assert_eq!(live_body[0]["spx_id"], "live-1");
    let route = live_body[0]["route"].as_array().expect("route field present");
    assert_eq!(
        route,
        &vec![
            serde_json::json!("Aceh DC"),
            serde_json::json!("Cileungsi DC")
        ],
        "route must be populated from raw_data via normalize_booking, not an empty default"
    );

    let hist_resp = http
        .get(format!("{base}/bookings/history"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(hist_resp.status(), 200);
    let hist_body: Vec<serde_json::Value> = hist_resp.json().await.unwrap();
    assert_eq!(hist_body.len(), 1);
    assert_eq!(hist_body[0]["spx_id"], "hist-1");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn detail_returns_full_raw_data_and_404s_for_unknown_id() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "detail-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"booking_id": "999", "note": "full payload"}),
        },
    )
    .await
    .expect("seed booking");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    let id = live_body[0]["id"].as_str().unwrap();

    let detail_resp = http
        .get(format!("{base}/bookings/{id}/detail"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(detail_resp.status(), 200);
    let detail_body: serde_json::Value = detail_resp.json().await.unwrap();
    assert_eq!(detail_body["raw_data"]["note"], "full payload");

    let missing_resp = http
        .get(format!("{base}/bookings/{}/detail", Uuid::new_v4()))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_resp.status(), 404);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn spx_log_lists_accept_events_newest_first() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    for outcome in ["accepted", "failed"] {
        store::insert_accept_event(
            &pool,
            tenant_id,
            &store::NewAcceptEvent {
                booking_id: None,
                rule_id: None,
                outcome: outcome.to_string(),
                local_dispatch_us: None,
                accept_e2e_ms: None,
                detail: serde_json::json!({}),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert {outcome}: {e}"));
    }

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/spx-log"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 2);
    assert_eq!(body[0]["outcome"], "failed", "newest (last-inserted) first");

    cleanup(&pool, tenant_id).await;
}

/// Builds `AppState` the same way `build_state` does, but spawns ONE real poller account
/// (`account_id`) pointed at `mock`'s URI, so `POST /:id/accept` has a real `AccountHandle` to
/// find in `state.poller.accounts`.
async fn build_state_with_account(
    pool: sqlx::PgPool,
    tenant_id: Uuid,
    account_id: &str,
    spx_base_url: &str,
) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = spx_client::SpxClient::new(spx_base_url).expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig {
            poll_interval_ms: 3_600_000,
            ..poller::PollerConfig::default()
        },
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    let mut state = poller::PollerState::new(
        account_id.to_string(),
        tenant_id,
        555, // nonzero agency_id — this test exercises the REAL accept_booking call, not the
             // agency_id<=0 short-circuit Task 6's note discloses for production today
        spx_client::SpxCookies::default(),
        "u".into(),
        "p".into(),
    );
    state.agency_id = 555;
    let handle = poller::ensure_restored_then_spawn(poller_shared.clone(), state).await;
    poller_shared.accounts.insert(account_id.to_string(), handle);

    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(vec![]),
        session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

#[tokio::test]
async fn manual_accept_happy_path_claims_dispatches_and_records() {
    // Self-healing: a prior successful run of THIS test (fixed account_id/spx_id) durably
    // recorded the accept in the shared Redis instance — see `cleanup_redis_claim`'s doc.
    cleanup_redis_claim("manual-acct", "manual-1").await;

    let mock = MockServer::start().await;
    // Real spx-client endpoint paths (see `spx-client/src/client.rs`'s `PATH_BIDDING_LIST`/
    // `PATH_ACCEPT` consts, and `crates/poller/tests/manual_accept_channel.rs`'s own deviation
    // note) — NOT the `/api/marketplace/dc/acceptBooking` placeholder this task's brief used.
    // The account's poll loop fires a sweep immediately on spawn (`spawn_account_loop` calls
    // `poll_once` before its first `select!`), so the bidding-list endpoint must also be mocked
    // (empty page) or every one of its rotating-window pages 404s — harmless (best-effort page
    // failures) but noisy, so it's mocked the same way `manual_accept_channel.rs` already does.
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
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "manual-acct".to_string(),
            spx_id: "manual-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"booking_id": "778899", "request_id": "1"}),
        },
    )
    .await
    .expect("seed booking");

    let state = build_state_with_account(pool.clone(), tenant_id, "manual-acct", &mock.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    let id = live_body[0]["id"].as_str().unwrap().to_string();

    let accept_resp = http
        .post(format!("{base}/bookings/{id}/accept"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(accept_resp.status(), 200);
    let accept_body: serde_json::Value = accept_resp.json().await.unwrap();
    assert_eq!(accept_body["ok"], true);
    assert_eq!(accept_body["reason"], "accepted");

    // DB status must now be 'accepted', not 'pending'.
    let after = store::bookings::get_detail(&pool, tenant_id, Uuid::parse_str(&id).unwrap())
        .await
        .expect("get_detail")
        .expect("row must still exist");
    assert_eq!(after.status, "accepted");
    assert!(!after.auto_accepted, "manual accept must record auto_accepted=false");

    // A SECOND accept attempt on the same (now non-pending) booking must be rejected.
    let second_resp = http
        .post(format!("{base}/bookings/{id}/accept"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(second_resp.status(), 409, "a non-pending booking must not be re-acceptable");

    cleanup(&pool, tenant_id).await;
    cleanup_redis_claim("manual-acct", "manual-1").await;
}

#[tokio::test]
async fn detail_includes_route_derived_from_raw_data() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "detail-route-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({
                "route_detail_list": [{
                    "node_info_list": [
                        {"name": "Cikarang DC", "address_info": {"l1": "Jabar", "l2": "Bekasi"}},
                    ]
                }]
            }),
        },
    )
    .await
    .expect("seed booking");

    let id = {
        let live = store::bookings::list_live(
            &pool,
            tenant_id,
            10,
            0,
            &store::bookings::BookingFilter::default(),
        )
        .await
        .expect("list");
        live.iter()
            .find(|b| b.spx_id == "detail-route-1")
            .expect("seeded row")
            .id
    };
    let row = store::bookings::get_detail(&pool, tenant_id, id)
        .await
        .expect("get_detail")
        .expect("row exists");
    assert_eq!(row.status, "pending"); // sanity check on the row itself, not the route below

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/{}/detail", row.id))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["route"], serde_json::json!(["Cikarang DC"]));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn audit_trail_returns_only_this_bookings_events_tenant_scoped() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "audit-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    let booking = {
        let live = store::bookings::list_live(
            &pool,
            tenant_id,
            10,
            0,
            &store::bookings::BookingFilter::default(),
        )
        .await
        .expect("list");
        live.into_iter()
            .find(|b| b.spx_id == "audit-1")
            .expect("seeded row")
    };

    store::insert_accept_event(
        &pool,
        tenant_id,
        &store::NewAcceptEvent {
            booking_id: Some(booking.id),
            rule_id: None,
            outcome: "accepted".to_string(),
            local_dispatch_us: Some(850),
            accept_e2e_ms: Some(312),
            detail: serde_json::json!({"manual": false}),
        },
    )
    .await
    .expect("seed accept_event for this booking");
    // A second, unrelated booking's event — must NOT show up in booking's own audit trail.
    store::insert_accept_event(
        &pool,
        tenant_id,
        &store::NewAcceptEvent {
            booking_id: None,
            rule_id: None,
            outcome: "failed".to_string(),
            local_dispatch_us: None,
            accept_e2e_ms: None,
            detail: serde_json::json!({}),
        },
    )
    .await
    .expect("seed unrelated accept_event");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/{}/audit-trail", booking.id))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(
        body.len(),
        1,
        "must only return this booking's own event, not the unrelated one"
    );
    assert_eq!(body[0]["outcome"], "accepted");
    assert_eq!(body[0]["local_dispatch_us"].as_i64(), Some(850));

    cleanup(&pool, tenant_id).await;
}

/// Unlike `audit_trail_returns_only_this_bookings_events_tenant_scoped` above (a SINGLE tenant —
/// it only proves the `booking_id` predicate works), this uses TWO real tenants and asserts
/// tenant B's session can never see tenant A's `accept_events` row for tenant A's booking, even
/// when tenant B's request names tenant A's `booking_id` directly. This is the pattern
/// `store::bookings_get_detail_returns_none_for_wrong_tenant` (`crates/store/src/lib.rs`) already
/// established for `/bookings/:id/detail`; `audit_trail` never had an equivalent.
///
/// `POST /auth/portal-login` looks up the username scoped by `state.tenant_id`
/// (`routes/auth.rs::portal_login`), i.e. one spawned server logs into ONE tenant — so tenant B's
/// server is built with `tenant_b` as `AppState.tenant_id` and tenant B's user logs in against
/// it. The resulting session's `tenant_id` (stored server-side, read back by `session_auth` into
/// `CurrentUser.tenant_id`) is what the `audit_trail` handler actually uses per-request — NOT
/// `AppState.tenant_id` again — so this exercises the same tenant-scoping path a real cross-tenant
/// request would.
///
/// Expected result confirmed by reading `audit_trail`'s handler (`src/routes/bookings.rs`): it
/// calls `store::accept_events::list_for_booking` directly with no prior `get_detail`-style
/// existence check on the booking, so a cross-tenant `booking_id` is not a 404 — it is RLS
/// (`list_for_booking` runs inside `begin_tenant_tx`, `WHERE tenant_id = $1 AND booking_id = $2`)
/// silently returning zero rows. So the correct assertion is `200` with an empty array, not `404`.
#[tokio::test]
async fn audit_trail_is_tenant_isolated_across_real_tenants() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_a = insert_tenant(&pool).await;
    let tenant_b = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_b, "owner-b").await;

    store::upsert_booking(
        &pool,
        tenant_a,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "audit-cross-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking under tenant A");
    let booking_a = {
        let live = store::bookings::list_live(
            &pool,
            tenant_a,
            10,
            0,
            &store::bookings::BookingFilter::default(),
        )
        .await
        .expect("list");
        live.into_iter()
            .find(|b| b.spx_id == "audit-cross-1")
            .expect("seeded row")
    };

    store::insert_accept_event(
        &pool,
        tenant_a,
        &store::NewAcceptEvent {
            booking_id: Some(booking_a.id),
            rule_id: None,
            outcome: "accepted".to_string(),
            local_dispatch_us: Some(111),
            accept_e2e_ms: Some(222),
            detail: serde_json::json!({"manual": false}),
        },
    )
    .await
    .expect("seed accept_event under tenant A");

    // Server spawned for tenant B (see doc comment above for why `state.tenant_id = tenant_b`
    // matters for login), then queried for tenant A's booking id.
    let state = build_state(pool.clone(), tenant_b).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner-b").await;

    let resp = http
        .get(format!("{base}/bookings/{}/audit-trail", booking_a.id))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        body.is_empty(),
        "tenant B must NEVER see tenant A's accept_event, even when it names tenant A's booking_id directly"
    );

    sqlx::query("DELETE FROM tenants WHERE id = ANY($1)")
        .bind(vec![tenant_a, tenant_b])
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
async fn history_status_filter_rejects_invalid_value_with_400() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?status=bogus"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn history_spx_id_filter_narrows_to_matching_prefix() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    for spx_id in ["filt-100", "filt-200", "other-300"] {
        store::upsert_booking(
            &pool,
            tenant_id,
            &store::BookingUpsert {
                account_id: "acct-1".to_string(),
                spx_id: spx_id.to_string(),
                status: "pending".to_string(),
                is_coc: false,
                raw_data: serde_json::json!({}),
            },
        )
        .await
        .expect("seed booking");
        store::update_booking_status(
            &pool,
            tenant_id,
            spx_id,
            store::BookingStatusUpdate {
                status: "accepted",
                latency_ms: Some(1),
                auto_accepted: true,
                rule_matched: None,
                accept_reason: None,
            },
        )
        .await
        .expect("mark accepted");
    }

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?spx_id=filt-"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let spx_ids: Vec<&str> = body.iter().map(|b| b["spx_id"].as_str().unwrap()).collect();
    assert_eq!(spx_ids.len(), 2, "expected only the two filt-* rows, got {spx_ids:?}");
    assert!(spx_ids.contains(&"filt-100"));
    assert!(spx_ids.contains(&"filt-200"));
    assert!(!spx_ids.contains(&"other-300"));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn history_status_filter_narrows_to_single_status() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "status-accepted-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    store::update_booking_status(
        &pool,
        tenant_id,
        "status-accepted-1",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: Some(1),
            auto_accepted: true,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark accepted");

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "status-failed-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    store::update_booking_status(
        &pool,
        tenant_id,
        "status-failed-1",
        store::BookingStatusUpdate {
            status: "failed",
            latency_ms: None,
            auto_accepted: false,
            rule_matched: None,
            accept_reason: Some("manual_accept_failed"),
        },
    )
    .await
    .expect("mark failed");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?status=failed"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["spx_id"], "status-failed-1");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn manual_accept_404s_for_unknown_booking_and_409s_for_disconnected_account() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    // A booking whose account was never spawned in THIS process — `state.poller.accounts` is
    // empty for it.
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "never-spawned-acct".to_string(),
            spx_id: "orphan-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");

    let state = build_state(pool.clone(), tenant_id).await; // no account spawned
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let missing_resp = http
        .post(format!("{base}/bookings/{}/accept", Uuid::new_v4()))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_resp.status(), 404);

    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    let id = live_body[0]["id"].as_str().unwrap();

    let disconnected_resp = http
        .post(format!("{base}/bookings/{id}/accept"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        disconnected_resp.status(),
        409,
        "a booking whose account has no running AccountHandle must not silently 500"
    );

    cleanup(&pool, tenant_id).await;
}

/// Whole-branch review Finding 1: the `to` date-range filter's SQL is `created_at <= $to`
/// (an inclusive upper bound) — `TicketFilterBar.svelte`'s `updateTo` used to convert the
/// picked date to START-of-day UTC midnight, which only matches a booking created at exactly
/// that instant, silently excluding every booking created LATER that same day. The fix makes
/// `updateTo` send END-of-day (`23:59:59.999Z`) instead. This test seeds a booking backdated to
/// mid-afternoon on a fixed day and proves: (a) `to` = that day's END-of-day (what the FIXED
/// frontend now sends) includes it, and (b) `to` = that day's START-of-day (the OLD, buggy
/// value) excludes it — i.e. this test would have caught the original bug, not just confirmed
/// the fix in isolation.
#[tokio::test]
async fn history_to_filter_end_of_day_includes_bookings_created_that_day() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "to-filter-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    store::update_booking_status(
        &pool,
        tenant_id,
        "to-filter-1",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: Some(1),
            auto_accepted: true,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark accepted");

    // `upsert_booking` has no way to pass an explicit `created_at` (it always writes `now()`),
    // so backdate the row directly — mid-afternoon on a fixed day, well away from either
    // boundary being tested.
    let created_at = chrono::DateTime::parse_from_rfc3339("2026-07-18T15:30:00Z")
        .expect("valid fixed timestamp")
        .with_timezone(&chrono::Utc);
    sqlx::query("UPDATE bookings SET created_at = $1 WHERE tenant_id = $2 AND spx_id = $3")
        .bind(created_at)
        .bind(tenant_id)
        .bind("to-filter-1")
        .execute(&pool)
        .await
        .expect("backdate created_at");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    // `to` = exactly what the FIXED TicketFilterBar.svelte's `updateTo` now sends for "include
    // 2026-07-18 as the end date": end-of-day UTC, not midnight-start. Proves the frontend fix
    // and the backend's `created_at <= $to` semantics compose correctly.
    let end_of_day_resp = http
        .get(format!("{base}/bookings/history?to=2026-07-18T23:59:59.999Z"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(end_of_day_resp.status(), 200);
    let end_of_day_body: Vec<serde_json::Value> = end_of_day_resp.json().await.unwrap();
    let end_of_day_ids: Vec<&str> = end_of_day_body
        .iter()
        .map(|b| b["spx_id"].as_str().unwrap())
        .collect();
    assert!(
        end_of_day_ids.contains(&"to-filter-1"),
        "a booking created during the selected end day must be included when `to` is end-of-day, got {end_of_day_ids:?}"
    );

    // Sanity check on the OLD, buggy `to` value (midnight start-of-day) this fix replaces — this
    // booking was created at 15:30, AFTER midnight, so it must be excluded. Proves this test
    // actually distinguishes the two behaviors instead of trivially passing either way.
    let start_of_day_resp = http
        .get(format!("{base}/bookings/history?to=2026-07-18T00:00:00.000Z"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(start_of_day_resp.status(), 200);
    let start_of_day_body: Vec<serde_json::Value> = start_of_day_resp.json().await.unwrap();
    let start_of_day_ids: Vec<&str> = start_of_day_body
        .iter()
        .map(|b| b["spx_id"].as_str().unwrap())
        .collect();
    assert!(
        !start_of_day_ids.contains(&"to-filter-1"),
        "sanity check: the OLD midnight-start `to` value must exclude a booking created later that day, got {start_of_day_ids:?}"
    );

    cleanup(&pool, tenant_id).await;
}

/// Task 4: `/bookings/live` must expose the SPX-derived generated columns (Task 1/3) on
/// `BookingListItem` — `request_id`/`onsite_id`/`booking_number`/`vehicle_type`/`trip_type`, and
/// `booking_type` must reuse the existing `is_coc` signal (`booking_name` matching `^SPXID`
/// makes this row COC), not invent a new derivation.
#[tokio::test]
async fn live_endpoint_returns_spx_derived_fields() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "derived-1".to_string(),
            status: "pending".to_string(),
            is_coc: false, // ignored — `is_coc` is a generated column, computed from `booking_name` below
            raw_data: serde_json::json!({
                "request_id": "R123",
                "onsite_id": "O456",
                "booking_name": "SPXID_DERIVED_1",
                "vehicle_type_name": "TRONTON",
                "deadline_at": 1_800_000_000,
                "trip_type": 1
            }),
        },
    )
    .await
    .expect("seed booking with spx-derived raw_data fields");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    let item = &body[0];
    assert_eq!(item["request_id"], "R123");
    assert_eq!(item["onsite_id"], "O456");
    assert_eq!(item["booking_number"], "SPXID_DERIVED_1");
    assert_eq!(item["vehicle_type"], "TRONTON");
    assert_eq!(item["trip_type"], 1);
    assert_eq!(item["booking_type"], "coc");

    cleanup(&pool, tenant_id).await;
}

/// Task 4: `/bookings/history` must apply the new `auto_accepted`/`vehicle_type` query params
/// together (AND, not OR) — proves `build_filter` actually wires both dimensions into the same
/// `BookingFilter`, not just whichever one a naive implementation might wire up first.
#[tokio::test]
async fn history_endpoint_filters_by_auto_accepted_and_vehicle_type() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "f1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"vehicle_type_name": "CDD"}),
        },
    )
    .await
    .expect("seed f1");
    store::update_booking_status(
        &pool,
        tenant_id,
        "f1",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: None,
            auto_accepted: true,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark f1 accepted, auto_accepted=true");

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "f2".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"vehicle_type_name": "TRONTON"}),
        },
    )
    .await
    .expect("seed f2");
    store::update_booking_status(
        &pool,
        tenant_id,
        "f2",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: None,
            auto_accepted: false,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark f2 accepted, auto_accepted=false");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?auto_accepted=true&vehicle_type=CDD"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1, "expected only f1 (auto_accepted=true AND vehicle_type=CDD), got {body:?}");
    assert_eq!(body[0]["spx_id"], "f1");

    cleanup(&pool, tenant_id).await;
}

/// Task 5: `/bookings/summary` endpoint requires session.
#[tokio::test]
async fn summary_endpoint_requires_session() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // No session cookie → 401.
    let res = http
        .get(format!("{base}/bookings/summary"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    cleanup(&pool, tenant_id).await;
}

/// Task 5: `/bookings/summary` endpoint returns today's counts.
#[tokio::test]
async fn summary_endpoint_returns_todays_counts() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "s1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("insert booking");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let res = http
        .get(format!("{base}/bookings/summary"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["incoming_today"], 1);

    cleanup(&pool, tenant_id).await;
}

/// Task 5: `/bookings/vehicle-types` endpoint returns distinct list.
#[tokio::test]
async fn vehicle_types_endpoint_returns_distinct_list() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "vt1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"vehicle_type_name": "CDD"}),
        },
    )
    .await
    .expect("insert booking");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let res = http
        .get(format!("{base}/bookings/vehicle-types"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body, serde_json::json!(["CDD"]));

    cleanup(&pool, tenant_id).await;
}
