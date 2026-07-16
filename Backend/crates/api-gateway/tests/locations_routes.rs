// Backend/crates/api-gateway/tests/locations_routes.rs
//! `GET/POST/DELETE /locations` — session-auth-gated read, `ManageLocations`-gated writes.
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
        .bind("Locations Test Tenant")
        .bind(format!("locations-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
/// `store::portal_users::create`'s real signature (see
/// `crates/store/src/portal_users.rs::create` and the already-passing
/// `crates/api-gateway/tests/prices_routes.rs` helper) is
/// `(pool, tenant_id, username, password_hash, display_name, is_main_account)`
/// — hash BEFORE display_name. Matched here (not re-guessed).
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await
        .expect("create portal user")
        .id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
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
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}
/// The task brief's transcription used `resp.headers().get_all("set-cookie")`, which would also
/// compile (no reqwest `cookies` feature required for raw header reads), but the merged
/// `prices_routes.rs` (Task 4) reads the single `Set-Cookie` header via
/// `reqwest::header::SET_COOKIE` instead; matched here for consistency with the verified-working
/// shape.
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
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

#[tokio::test]
async fn locations_require_session_and_gate_writes_on_main_account() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let unauth = http.get(format!("{base}/locations")).send().await.unwrap();
    assert_eq!(unauth.status(), 401);

    let helper_cookie = login_cookie(&http, &base, "helper").await;
    let read_resp = http.get(format!("{base}/locations")).header("Cookie", &helper_cookie).send().await.unwrap();
    assert_eq!(read_resp.status(), 200);

    let write_resp = http.post(format!("{base}/locations")).header("Cookie", &helper_cookie)
        .json(&serde_json::json!({"name": "Padang DC"})).send().await.unwrap();
    assert_eq!(write_resp.status(), 403, "sub-user must not create locations");

    let owner_cookie = login_cookie(&http, &base, "owner").await;
    let create_resp = http.post(format!("{base}/locations")).header("Cookie", &owner_cookie)
        .json(&serde_json::json!({"name": "Padang DC"})).send().await.unwrap();
    assert_eq!(create_resp.status(), 200);
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let delete_resp = http.delete(format!("{base}/locations/{id}")).header("Cookie", &owner_cookie)
        .send().await.unwrap();
    assert_eq!(delete_resp.status(), 204);

    let listed = http.get(format!("{base}/locations")).header("Cookie", &owner_cookie).send().await.unwrap();
    let listed_body: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed_body.len(), 0);

    cleanup(&pool, tenant_id).await;
}
