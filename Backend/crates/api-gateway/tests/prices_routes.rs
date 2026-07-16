// Backend/crates/api-gateway/tests/prices_routes.rs
//! `GET /prices` (public) + `POST/PUT/DELETE /prices` (`ManagePrices`-gated).
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
        .bind("Prices Test Tenant")
        .bind(format!("prices-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
/// `store::portal_users::create`'s real signature (see
/// `crates/store/src/portal_users.rs::create` and the already-passing
/// `crates/api-gateway/tests/bookings_routes.rs`/`rules_routes.rs` helpers) is
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
/// compile (no reqwest `cookies` feature required for raw header reads), but every other
/// already-passing test file in this crate (`bookings_routes.rs`, `rules_routes.rs`) reads the
/// single `Set-Cookie` header via `reqwest::header::SET_COOKIE` instead; matched here for
/// consistency with the verified-working shape.
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
async fn get_prices_is_public_and_lists_seeded_rows() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    store::create_route_price(
        &pool,
        tenant_id,
        &store::NewRoutePrice {
            route_code: "AAA".to_string(),
            region: "".to_string(),
            origin: "Padang DC".to_string(),
            destinations: serde_json::json!(["Cileungsi DC"]),
            price: 100,
            vehicle_type: "TRONTON".to_string(),
        },
    )
    .await
    .expect("seed price");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // NO cookie at all — must still succeed (public route).
    let resp = http.get(format!("{base}/prices")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["route_code"], "AAA");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn create_price_requires_main_account_and_validates_destinations() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let helper_cookie = login_cookie(&http, &base, "helper").await;
    let sub_user_resp = http
        .post(format!("{base}/prices"))
        .header("Cookie", &helper_cookie)
        .json(&serde_json::json!({
            "route_code": "BBB", "origin": "X", "destinations": ["Y"], "price": 1, "vehicle_type": "V"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sub_user_resp.status(), 403);

    let owner_cookie = login_cookie(&http, &base, "owner").await;
    let bad_dest_resp = http
        .post(format!("{base}/prices"))
        .header("Cookie", &owner_cookie)
        .json(&serde_json::json!({
            "route_code": "CCC", "origin": "X", "destinations": [], "price": 1, "vehicle_type": "V"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_dest_resp.status(), 400, "empty destinations array must be rejected");

    let good_resp = http
        .post(format!("{base}/prices"))
        .header("Cookie", &owner_cookie)
        .json(&serde_json::json!({
            "route_code": "DDD", "origin": "X", "destinations": ["Y"], "price": 500, "vehicle_type": "V"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(good_resp.status(), 200);

    cleanup(&pool, tenant_id).await;
}
