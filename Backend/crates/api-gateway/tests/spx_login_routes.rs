// Backend/crates/api-gateway/tests/spx_login_routes.rs
//! Route-level tests for `POST /auth/spx-login/:label` (Fase 6b Task 3) — a
//! tier-2/3-only CONNECTIVITY TEST for a stored SPX credential. Same
//! convention as `tests/spx_credentials_routes.rs`: a real `axum::serve`
//! instance + a real HTTP client (`reqwest`) against `api_gateway::build_router`
//! itself, real Postgres (127.0.0.1:15432) and real Redis (127.0.0.1:16379).
//!
//! The SPX login endpoints themselves are faked with `wiremock`, reusing the
//! EXACT request/response shapes `spx-client/tests/login_mock.rs` already
//! established for `api_login`/`form_login` (same paths, same
//! `set-cookie`/status conventions) rather than inventing a new mocking
//! pattern for this crate.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use serde_json::Value;
use spx_client::crypto::envelope::{encrypt_agency_password, KEY_VERSION};
use spx_client::crypto::password::hash_password;
use spx_client::SpxClient;
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION_COOKIE_NAME: &str = "spx_session";
const KNOWN_PASSWORD: &str = "correct horse battery staple 42";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

/// The SAME fixed 32-byte master key used to build `AppState.master_key`
/// below AND to encrypt the seeded `agency_credentials` row — the route
/// handler must decrypt with the exact key the test used to encrypt.
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes(
        [11u8; 32],
    ))
}

async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client for AppState.redis")
        .get_connection_manager()
        .await
        .expect("connect AppState.redis connection manager")
}

async fn insert_tenant(pool: &PgPool) -> Uuid {
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Spx Login Routes Test Tenant")
        .bind(format!("spx-login-routes-{tenant_id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    tenant_id
}

async fn insert_portal_user(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
    is_main_account: bool,
) -> Uuid {
    let hash = hash_password(KNOWN_PASSWORD).expect("hash known password");
    let user = store::portal_users::create(
        pool,
        tenant_id,
        username,
        &hash,
        &format!("Display {username}"),
        is_main_account,
    )
    .await
    .expect("insert portal_user");
    user.id
}

/// Envelope-encrypts `password` with the SAME master key `build_state` below
/// hands to `AppState`, then inserts the row directly via
/// `store::agency_credentials::create` (test-side seeding, per this task's
/// brief — not through the `PUT /auth/spx-credentials` route).
async fn seed_credential(pool: &PgPool, tenant_id: Uuid, label: &str, username: &str, password: &str) {
    let ct = encrypt_agency_password(&test_master_key(), tenant_id, password)
        .expect("encrypt seeded credential password");
    store::agency_credentials::create(
        pool,
        tenant_id,
        label,
        username,
        &ct.bytes,
        &ct.nonce,
        KEY_VERSION,
    )
    .await
    .expect("insert seeded agency_credentials row");
}

/// Same construction shape as `tests/spx_credentials_routes.rs`'s
/// `build_state`, except `poller.client` (the `SpxClient` this route's
/// tier-2/3 login attempts actually go through) is pointed at `spx_base_url`
/// — the wiremock server's URI — instead of a dead loopback address, since
/// this route's whole test surface is exercising that client against a fake
/// SPX server.
async fn build_state(pool: PgPool, tenant_id: Uuid, spx_base_url: &str) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = SpxClient::new(spx_base_url).expect("build SpxClient pointed at wiremock");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1");

    let poller_shared = poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool,
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    };

    AppState {
        poller: Arc::new(poller_shared),
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(Vec::new()),
        session_cookie_name: Arc::from(SESSION_COOKIE_NAME),
        cookie_secure: true,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            api_gateway::build_router(state)
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

async fn cleanup(pool: &PgPool, tenant_id: Uuid) {
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await
        .ok();
}

fn set_cookie_header(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_string())
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("Set-Cookie header has at least one ';'-delimited segment")
        .to_string()
}

async fn login(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": username,
            "password": KNOWN_PASSWORD,
        }))
        .send()
        .await
        .expect("request portal-login");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "login must succeed to obtain a session cookie for this test"
    );
    cookie_pair(&set_cookie_header(&resp).expect("Set-Cookie present on successful login"))
}

/// Mounts 401s on all 4 distinct paths `api_login` tries (5 attempts, 2 of
/// which share `/api/basicserver/agency/account/login`) and a no-cookie,
/// no-redirect `/login` pair for `form_login` — the exact failure shapes
/// `spx-client/tests/login_mock.rs` already uses in
/// `api_login_falls_through_its_5_attempts_in_order` and
/// `form_login_returns_none_when_no_redirect_and_no_skey`, reused here rather
/// than invented fresh.
async fn mount_all_tiers_fail(server: &MockServer) {
    for failing_path in [
        "/api/basicserver/agency/account/login",
        "/api/basicserver/account/login",
        "/api/basicserver/agency/auth/login",
        "/api/user/login",
    ] {
        Mock::given(method("POST"))
            .and(path(failing_path))
            .respond_with(ResponseTemplate::new(401))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// Case 1: wiremock's FIRST `api_login` attempt (`/api/basicserver/agency/account/login`)
/// returns a `Set-Cookie: fms_user_skey=...` -> `200 {ok: true, tier: "api"}`,
/// and no password/cookie material anywhere in the response.
#[tokio::test]
async fn success_via_api_tier() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-login-ok", true).await;
    seed_credential(&pool, tenant_id, "agency1", "agency1-user", "s3cret-agency-pw").await;

    let spx_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/account/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=APITESTKEY; Path=/"),
        )
        .mount(&spx_server)
        .await;

    let state = build_state(pool.clone(), tenant_id, &spx_server.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-login-ok").await;

    let resp = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request POST /auth/spx-login/agency1");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body, serde_json::json!({ "ok": true, "tier": "api" }));

    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.contains("s3cret-agency-pw"));
    assert!(!body_str.contains("APITESTKEY"));

    cleanup(&pool, tenant_id).await;
}

/// Case 2: wiremock fails every tier-2/3 attempt -> `200 {ok: false, tier: null}`
/// (a failed connectivity test is still a well-formed 200 response, not an
/// error status — the route's own job is only to report the outcome).
#[tokio::test]
async fn all_tiers_fail_reports_ok_false() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-login-fail", true).await;
    seed_credential(&pool, tenant_id, "agency1", "agency1-user", "wrong-or-whatever").await;

    let spx_server = MockServer::start().await;
    mount_all_tiers_fail(&spx_server).await;

    let state = build_state(pool.clone(), tenant_id, &spx_server.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-login-fail").await;

    let resp = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request POST /auth/spx-login/agency1");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body, serde_json::json!({ "ok": false, "tier": null }));

    cleanup(&pool, tenant_id).await;
}

/// Case 3: a `label` with no matching `agency_credentials` row -> 404, before
/// any SPX login attempt is even made (no wiremock mocks are mounted, and
/// none is expected to be hit).
#[tokio::test]
async fn nonexistent_label_returns_404() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-login-404", true).await;

    let spx_server = MockServer::start().await;

    let state = build_state(pool.clone(), tenant_id, &spx_server.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-login-404").await;

    let resp = http
        .post(format!("{base}/auth/spx-login/does-not-exist"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request POST /auth/spx-login/does-not-exist");

    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    cleanup(&pool, tenant_id).await;
}

/// Case 4: a sub-user (non-main-account) session -> 403 (`require_permission`
/// rejection), before any SPX login attempt is made — same RBAC gate as
/// `PUT`/`DELETE /auth/spx-credentials`.
#[tokio::test]
async fn sub_user_is_forbidden() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-login-rbac", true).await;
    insert_portal_user(&pool, tenant_id, "sub-login-rbac", false).await;
    seed_credential(&pool, tenant_id, "agency1", "agency1-user", "irrelevant-password").await;

    let spx_server = MockServer::start().await;

    let state = build_state(pool.clone(), tenant_id, &spx_server.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let sub_cookie = login(&http, &base, "sub-login-rbac").await;

    let resp = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request POST /auth/spx-login/agency1 as sub-user");

    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    cleanup(&pool, tenant_id).await;
}

/// Case 5: a second `POST /auth/spx-login/:label` for the SAME (tenant, label)
/// within the cooldown window is rejected with 429, even though the first
/// call succeeded. The cooldown is claimed only after the 403/404/decrypt
/// checks, so it guards exactly the calls that would otherwise hit SPX.
#[tokio::test]
async fn second_call_within_cooldown_is_rate_limited() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-login-rl", true).await;
    seed_credential(&pool, tenant_id, "agency1", "agency1-user", "s3cret-agency-pw").await;

    let spx_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/account/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=APITESTKEY; Path=/"),
        )
        .mount(&spx_server)
        .await;

    let state = build_state(pool.clone(), tenant_id, &spx_server.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-login-rl").await;

    let first = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("first spx-login request");
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    let second = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("second spx-login request");
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    cleanup(&pool, tenant_id).await;
}
