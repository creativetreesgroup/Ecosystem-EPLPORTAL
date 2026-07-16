// Backend/crates/api-gateway/tests/rules_routes.rs
//! `GET`/`PUT /bookings/settings` — rules editor, automation toggle, OTP-gated arm.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use redis::AsyncCommands;
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
        .bind("Rules Test Tenant")
        .bind(format!("rules-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
/// NOTE: the task brief's transcription of this helper passed
/// `(username, "Test User", &hash, is_main)` as the trailing args of
/// `store::portal_users::create`, but that fn's real signature (see
/// `crates/store/src/portal_users.rs::create` and the already-passing
/// `crates/api-gateway/tests/bookings_routes.rs`/`portal_users_routes.rs`
/// helpers) is `(pool, tenant_id, username, password_hash, display_name,
/// is_main_account)` — hash BEFORE display_name. Fixed here to match reality.
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
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
/// NOTE: the task brief's transcription used `reqwest::Client::builder().cookie_store(false)` +
/// `resp.cookies()`, which needs reqwest's `cookies` cargo feature — not enabled for this crate
/// (see `crates/api-gateway/Cargo.toml`'s `reqwest` line: `features = ["json"]` only). Every
/// other test file in this crate (e.g. `tests/bookings_routes.rs`'s `login_cookie`) reads the
/// raw `Set-Cookie` response header instead; matched here.
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
async fn get_settings_defaults_to_disabled_and_empty() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["auto_accept_enabled"], false);
    assert_eq!(body["rules"].as_array().unwrap().len(), 0);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn put_settings_sanitizes_dedupes_and_round_trips_via_get() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    // Two rules that target the SAME route (same origin/destination/mode) — dedupe_rules must
    // collapse them to one.
    let put_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({
            "auto_accept_enabled": false,
            "rules": [
                {"name": "A", "enabled": true, "mode": "route", "origin": "Padang DC", "destinations": ["Cileungsi DC"]},
                {"name": "B", "enabled": true, "mode": "route", "origin": "Padang DC", "destinations": ["Cileungsi DC"]}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 200);
    let put_body: serde_json::Value = put_resp.json().await.unwrap();
    assert_eq!(
        put_body["rules"].as_array().unwrap().len(),
        1,
        "two rules targeting the same lane must collapse to one via dedupe_rules"
    );

    let get_resp = http
        .get(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["rules"].as_array().unwrap().len(), 1);
    assert_eq!(get_body["rules"][0]["origin"], "Padang DC");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn sub_user_cannot_write_settings_but_can_read_them() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "helper").await;

    let get_resp = http
        .get(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);

    let put_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": false, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 403);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn arming_auto_accept_without_a_pwverify_proof_is_unauthorized_but_disarming_never_needs_one() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    // Disarming (staying/going to false) never needs a proof.
    let disarm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": false, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(disarm_resp.status(), 200);

    // Arming WITHOUT a proof must be rejected.
    let arm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": true, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(arm_resp.status(), 401);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn arming_auto_accept_consumes_the_pwverify_proof_exactly_once_and_broadcasts_new_rules() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let mut rules_watcher = state.poller.rules_tx.subscribe();
    let mut redis = test_redis_manager().await;
    // Seed the proof directly — this test exercises THIS route's consumption contract, not
    // Fase 6b's OTP generation/verification (already covered by that sub-phase's own tests).
    let key = format!("spx:pwverify:{tenant_id}:{user_id}");
    let _: () = redis.set_ex(&key, "1", 120).await.expect("seed pwverify proof");

    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let arm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({
            "auto_accept_enabled": true,
            "rules": [{"name": "R1", "enabled": true, "mode": "filter"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(arm_resp.status(), 200);
    let arm_body: serde_json::Value = arm_resp.json().await.unwrap();
    assert_eq!(arm_body["auto_accept_enabled"], true);

    let proof_after: Option<String> = redis.get(&key).await.expect("read proof after arm");
    assert!(proof_after.is_none(), "the proof must be consumed (deleted) after a successful arm");

    assert!(
        rules_watcher.has_changed().unwrap_or(false),
        "a running account's rules_rx subscriber must see the freshly saved rule set"
    );
    let broadcast = rules_watcher.borrow_and_update().clone();
    assert_eq!(broadcast.rules.len(), 1, "the broadcast RuleSet must reflect the just-saved rule");

    // A SECOND arm attempt (already armed — true→true, not a transition) must NOT need a proof.
    let second_arm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": true, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        second_arm_resp.status(),
        200,
        "staying armed (true\u{2192}true) must not require a fresh OTP proof"
    );

    cleanup(&pool, tenant_id).await;
}
