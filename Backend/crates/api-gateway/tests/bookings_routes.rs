// Backend/crates/api-gateway/tests/bookings_routes.rs
//! `GET /bookings/live`, `/history`, `/:id/detail`, `/spx-log` — session-auth-only read routes.
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

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
            raw_data: serde_json::json!({}),
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
