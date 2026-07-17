// Backend/crates/api-gateway/tests/branding_routes.rs
//! `GET /branding` (public) + `PUT /branding` (`ManageBranding`) + the 15MB body-limit carve-out
//! itself — proves BOTH (a) branding accepts a body between 1.5MB and 15MB and (b) every OTHER
//! route still correctly 413s above 1.5MB, per Task 8's own risk note.
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
    redis::Client::open(redis_url()).expect("open redis client").get_connection_manager().await.expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id).bind("Branding Test Tenant").bind(format!("branding-test-{id}"))
        .execute(pool).await.expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await.expect("create portal user").id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor), client: Arc::new(client), pool: pool.clone(),
        config: poller::PollerConfig::default(), accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar), notifier: None, redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared, ws_hub: ws_hub::Hub::new(), tenant_id,
        cors_origins: Arc::new(vec![]), session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false, master_key: test_master_key(), redis: test_redis_manager().await,
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
/// compile, but every other already-merged test file in this crate (`locations_routes.rs`,
/// `prices_routes.rs`, etc) reads the single `Set-Cookie` header via
/// `reqwest::header::SET_COOKIE` instead; matched here for consistency with the verified-working
/// shape rather than re-deriving a second way to do the same thing.
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http.post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send().await.expect("login request");
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

/// A real, base64-encoded ~4MB PNG-shaped payload (starts with a real PNG magic-byte-adjacent
/// prefix so it passes `validate_data_uri`'s prefix check; content past that is irrelevant filler
/// for THIS test's purpose — proving the body-limit carve-out, not image correctness).
fn big_valid_logo_data_uri(approx_decoded_bytes: usize) -> String {
    let b64_len = (approx_decoded_bytes / 3) * 4;
    format!("data:image/png;base64,{}", "A".repeat(b64_len))
}

#[tokio::test]
async fn get_branding_is_public_and_returns_defaults_when_unconfigured() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/branding")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "GET /branding must be reachable with no session cookie at all");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["site_name"], "SPX Agency Portal");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn put_branding_accepts_a_4mb_body_but_prices_still_rejects_it() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    // ~4MB decoded logo — well over the GLOBAL 1.5MB limit, well under branding's 15MB one.
    let logo = big_valid_logo_data_uri(4_000_000);
    let put_resp = http
        .put(format!("{base}/branding"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"title": "Test", "logo_data_uri": logo}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        put_resp.status(),
        200,
        "a ~4MB branding PUT must succeed — this is the carve-out's whole point"
    );

    let get_resp = http.get(format!("{base}/branding")).send().await.unwrap();
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["title"], "Test");
    assert!(get_body["logo_data_uri"].as_str().unwrap().len() > 1_500_000, "the stored logo must be the full oversized payload, not truncated");

    // A DIFFERENT route (not branding) must STILL reject a body over 1.5MB — proves the global
    // layer wasn't accidentally widened for the whole app instead of scoped to just branding.
    let oversized_json = serde_json::json!({"name": "A".repeat(2_000_000)});
    let other_route_resp = http
        .post(format!("{base}/locations"))
        .header("Cookie", &cookie)
        .json(&oversized_json)
        .send()
        .await
        .unwrap();
    assert_eq!(
        other_route_resp.status(),
        413,
        "a >1.5MB body on ANY non-branding route must still be rejected"
    );

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn sub_user_cannot_write_branding_but_can_read_it() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let get_resp = http.get(format!("{base}/branding")).send().await.unwrap();
    assert_eq!(get_resp.status(), 200);

    let helper_cookie = login_cookie(&http, &base, "helper").await;
    let put_resp = http
        .put(format!("{base}/branding"))
        .header("Cookie", &helper_cookie)
        .json(&serde_json::json!({"title": "Hacked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 403);

    cleanup(&pool, tenant_id).await;
}
