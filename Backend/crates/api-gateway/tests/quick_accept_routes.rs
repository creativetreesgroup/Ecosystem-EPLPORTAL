// Backend/crates/api-gateway/tests/quick_accept_routes.rs
//! `GET/POST /q/:token` — the HMAC quick-accept flow, reachable with NO session cookie at all.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
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
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes([7u8; 32]))
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
        .bind("Quick Accept Test Tenant")
        .bind(format!("qa-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
async fn insert_booking(pool: &sqlx::PgPool, tenant_id: Uuid, spx_id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO bookings (tenant_id, account_id, spx_id, status, raw_data) \
         VALUES ($1, 'acct-qa', $2, $3, '{}'::jsonb)",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert booking");
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
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

#[tokio::test]
async fn get_quick_token_is_reachable_with_no_session_at_all() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPX-QA-1", "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-QA-1",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "must be reachable with zero Cookie header");
    let body = resp.text().await.unwrap();
    assert!(body.contains("SPX-QA-1"));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn expired_token_returns_410() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let long_ago = chrono::Utc::now().timestamp_millis() - 999_999_999;
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-QA-2",
        1,
        long_ago,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 410);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn malformed_token_returns_400() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/q/a")).send().await.unwrap(); // too short
    assert_eq!(resp.status(), 400);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn post_quick_accept_on_a_nonexistent_booking_returns_404() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-DOES-NOT-EXIST",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http
        .post(format!("{base}/q/accept"))
        .json(&serde_json::json!({"token": token}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn wrong_tenant_token_is_rejected_by_this_deployments_state_tenant_id() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // Signed for a DIFFERENT tenant than this deployment's own state.tenant_id.
    let other_tenant = Uuid::new_v4();
    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        other_tenant,
        "SPX-QA-3",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(
        resp.status(),
        410,
        "a token signed for a different tenant must verify as invalid here"
    );

    cleanup(&pool, tenant_id).await;
}
