// Backend/crates/api-gateway/tests/otp_routes.rs
//! Route-level tests for `POST /auth/request-aa-otp` + `POST
//! /auth/verify-aa-otp` (Fase 6b Task 5). Same convention as
//! `tests/spx_login_routes.rs`/`tests/spx_credentials_routes.rs`: a real
//! `axum::serve` instance + a real HTTP client (`reqwest`) against
//! `api_gateway::build_router` itself, real Postgres (127.0.0.1:15432) and
//! real Redis (127.0.0.1:16379). WAHA delivery is faked with `wiremock`,
//! same pattern `spx_login_routes.rs` already established for the SPX login
//! endpoints.
//!
//! `load_bot_settings` reads `site_settings` (key
//! `spx_client::waha_settings::SITE_SETTINGS_KEY`) — nothing in this plan
//! writes that row yet (6d's CRUD route doesn't exist), so this file seeds it
//! directly via `WahaSettings::encrypt_new` + a raw `sqlx::query` INSERT,
//! matching the brief's explicit precedent (Fase 6a Task 9 did the same for
//! `agency_credentials` before its own CRUD existed).
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use serde_json::Value;
use spx_client::crypto::password::hash_password;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};
use spx_client::SpxClient;
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION_COOKIE_NAME: &str = "spx_session";
const KNOWN_PASSWORD: &str = "correct horse battery staple 42";
const TEST_WA_NUMBER: &str = "6281234567890";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

/// The SAME fixed 32-byte master key used to build `AppState.master_key`
/// below AND to encrypt the seeded `site_settings` row's WAHA API key — the
/// route handler must decrypt with the exact key the test used to encrypt.
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes(
        [23u8; 32],
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
        .bind("Otp Routes Test Tenant")
        .bind(format!("otp-routes-{tenant_id}"))
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

/// Seeds the `site_settings` row `load_bot_settings` reads: envelope-encrypts
/// a WAHA API key with the SAME master key `build_state` hands to
/// `AppState`, sets `wa_number` (this task's own field addition to
/// `WahaSettings`) to `TEST_WA_NUMBER`, then inserts the row directly via
/// `sqlx::query` (test-side seeding — no CRUD route writes this row yet).
async fn seed_waha_settings(pool: &PgPool, tenant_id: Uuid, waha_url: &str) {
    let mut settings = WahaSettings::encrypt_new(
        &test_master_key(),
        tenant_id,
        waha_url,
        "default",
        "waha-test-api-key",
    )
    .expect("encrypt waha settings");
    settings.wa_number = TEST_WA_NUMBER.to_string();

    let mut tx = store::begin_tenant_tx(pool, tenant_id)
        .await
        .expect("begin tenant tx");
    sqlx::query("INSERT INTO site_settings (tenant_id, key, value) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(SITE_SETTINGS_KEY)
        .bind(settings.to_json_value())
        .execute(&mut *tx)
        .await
        .expect("insert site_settings row");
    tx.commit().await.expect("commit");
}

/// Same construction shape as `tests/spx_login_routes.rs`'s `build_state`,
/// except `poller.client`'s target address is irrelevant here (this route
/// never calls `SpxClient` — WAHA delivery goes through `notifier::waha`
/// directly), so it stays pointed at a dead loopback address like
/// `spx_credentials_routes.rs`'s `build_state` does.
async fn build_state(pool: PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
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

/// Direct Redis reads, independent of the HTTP layer — same "verify via
/// direct backend read" pattern `otp_module.rs` and the spx-credentials route
/// tests already use for their own backends (Postgres there, Redis here).
async fn direct_redis() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis connection manager")
}

async fn read_stored_code(redis: &mut redis::aio::ConnectionManager, tenant_id: Uuid, user_id: Uuid) -> String {
    let key = format!("spx:aa_otp:{tenant_id}:{user_id}");
    redis::AsyncCommands::get(redis, &key)
        .await
        .expect("read stored OTP code directly from Redis")
}

async fn key_exists(redis: &mut redis::aio::ConnectionManager, key: &str) -> bool {
    redis::AsyncCommands::exists(redis, key)
        .await
        .expect("EXISTS query")
}

/// Case 1: `POST /request-aa-otp` -> 200, and wiremock recorded EXACTLY one
/// `/api/sendText` call whose body's `chatId` matches the configured
/// `wa_number` (`"6281234567890@c.us"` — `parse_chat_ids`'s bare-digits ->
/// `@c.us` normalization), NOT some `wa_group` value (this `WahaSettings`-
/// backed `BotSettings` never has one — `wa_group` is always defaulted to
/// `""` by `load_bot_settings`, so there is nothing else it COULD send to).
#[tokio::test]
async fn request_otp_sends_to_wa_number_via_waha() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "main-otp-req", true).await;

    let waha_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&waha_server)
        .await;

    seed_waha_settings(&pool, tenant_id, &waha_server.uri()).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-otp-req").await;

    let resp = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request POST /auth/request-aa-otp");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body, serde_json::json!({ "ok": true }));

    // wiremock's `.expect(1)` above is verified on drop/`.verify()` at the
    // MockServer's teardown — but assert explicitly here too, checking the
    // actual chatId, not just the call count.
    let received = waha_server.received_requests().await.expect("received requests");
    assert_eq!(received.len(), 1, "exactly one /api/sendText call expected");
    let sent_body: Value = received[0].body_json().expect("sendText body is JSON");
    assert_eq!(sent_body["chatId"], "6281234567890@c.us");

    let mut redis = direct_redis().await;
    let stored_code = read_stored_code(&mut redis, tenant_id, user_id).await;
    assert_eq!(stored_code.len(), 6);

    cleanup(&pool, tenant_id).await;
}

/// Case 2: an IMMEDIATE second `POST /request-aa-otp` -> `429 Too Many
/// Requests` (this task's chosen status for `OtpRequestError::TooSoon` — see
/// `routes/otp.rs`'s doc comment for why 429, not 409, was chosen).
#[tokio::test]
async fn immediate_second_request_is_rate_limited() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-otp-cooldown", true).await;

    let waha_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&waha_server)
        .await;
    seed_waha_settings(&pool, tenant_id, &waha_server.uri()).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-otp-cooldown").await;

    let first = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("first request");
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    let second = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("second (immediate) request");
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    cleanup(&pool, tenant_id).await;
}

/// Case 3: `POST /verify-aa-otp` with the WRONG code -> uniform `401
/// Unauthorized` rejection (this task's chosen status for both
/// `WrongCode`/`NoActiveCode`, matching the login route's "don't distinguish
/// exact failure reason" caution — see `routes/otp.rs`'s doc comment).
#[tokio::test]
async fn verify_with_wrong_code_is_uniformly_unauthorized() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "main-otp-wrong", true).await;

    let waha_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&waha_server)
        .await;
    seed_waha_settings(&pool, tenant_id, &waha_server.uri()).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-otp-wrong").await;

    let req_resp = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request-aa-otp");
    assert_eq!(req_resp.status(), reqwest::StatusCode::OK);

    let mut redis = direct_redis().await;
    let real_code = read_stored_code(&mut redis, tenant_id, user_id).await;
    let wrong_code = if real_code == "999999" { "000000" } else { "999999" };

    let verify_resp = http
        .post(format!("{base}/auth/verify-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({ "code": wrong_code }))
        .send()
        .await
        .expect("verify-aa-otp with wrong code");
    assert_eq!(verify_resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    let pwverify_key = format!("spx:pwverify:{tenant_id}:{user_id}");
    assert!(
        !key_exists(&mut redis, &pwverify_key).await,
        "pwverify proof must NOT exist after a failed verify"
    );

    cleanup(&pool, tenant_id).await;
}

/// Case 4: `POST /verify-aa-otp` with the RIGHT code (read directly from
/// Redis after Case 1's request pattern — the "verify via direct backend
/// read" convention this project's tests already use) -> 200, and a direct
/// Redis read confirms `spx:pwverify:<tenant>:<user>` now exists.
#[tokio::test]
async fn verify_with_right_code_succeeds_and_writes_pwverify_proof() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "main-otp-right", true).await;

    let waha_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&waha_server)
        .await;
    seed_waha_settings(&pool, tenant_id, &waha_server.uri()).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-otp-right").await;

    let req_resp = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request-aa-otp");
    assert_eq!(req_resp.status(), reqwest::StatusCode::OK);

    let mut redis = direct_redis().await;
    let real_code = read_stored_code(&mut redis, tenant_id, user_id).await;

    let verify_resp = http
        .post(format!("{base}/auth/verify-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({ "code": real_code }))
        .send()
        .await
        .expect("verify-aa-otp with right code");
    assert_eq!(verify_resp.status(), reqwest::StatusCode::OK);
    let body: Value = verify_resp.json().await.expect("json body");
    assert_eq!(body, serde_json::json!({ "ok": true }));

    let pwverify_key = format!("spx:pwverify:{tenant_id}:{user_id}");
    assert!(
        key_exists(&mut redis, &pwverify_key).await,
        "pwverify proof must exist after a successful verify"
    );

    cleanup(&pool, tenant_id).await;
}

/// Case 5: a sub-user (non-main-account) session -> 403 on BOTH routes
/// (`require_permission(ArmAutoAccept)` rejection), before any Redis/WAHA
/// interaction — same RBAC gate as `PUT`/`DELETE /auth/spx-credentials` and
/// `POST /auth/spx-login/:label`.
#[tokio::test]
async fn sub_user_is_forbidden_on_both_routes() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-otp-rbac", true).await;
    insert_portal_user(&pool, tenant_id, "sub-otp-rbac", false).await;

    // No site_settings row seeded at all — proves the 403 fires BEFORE
    // `load_bot_settings` (and thus before any Postgres/WAHA interaction)
    // would otherwise surface a different error for a misconfigured tenant.
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let sub_cookie = login(&http, &base, "sub-otp-rbac").await;

    let req_resp = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request-aa-otp as sub-user");
    assert_eq!(req_resp.status(), reqwest::StatusCode::FORBIDDEN);

    let verify_resp = http
        .post(format!("{base}/auth/verify-aa-otp"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .json(&serde_json::json!({ "code": "123456" }))
        .send()
        .await
        .expect("verify-aa-otp as sub-user");
    assert_eq!(verify_resp.status(), reqwest::StatusCode::FORBIDDEN);

    cleanup(&pool, tenant_id).await;
}

/// Bonus coverage (not one of the 5 required cases, but cheap given the
/// fixtures above): no `site_settings` row at all -> `400 Bad Request` with
/// the disclosed "not configured" message, proving `load_bot_settings`'s
/// missing-row path actually wires up to the route, not just in isolation.
#[tokio::test]
async fn request_otp_without_site_settings_row_is_bad_request() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-otp-unconfigured", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-otp-unconfigured").await;

    let resp = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request-aa-otp with no site_settings row");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"], "OTP delivery is not configured for this tenant",
        "expected the disclosed not-configured message: {body}"
    );

    cleanup(&pool, tenant_id).await;
}

/// Whole-branch review finding regression: a SECOND `request-aa-otp` call
/// from a still-unconfigured tenant, made within what would have been the
/// 60s resend cooldown window had the first call armed it, must ALSO see
/// `400 "not configured"` — never a `429 "otp already requested"`. Before
/// this fix, `request_otp` called `otp::request` (claiming the cooldown key)
/// BEFORE checking `load_bot_settings`, so the first call would claim the
/// cooldown and then still fail with 400; but the SECOND call would find the
/// cooldown key already occupied and misleadingly return 429 instead of the
/// accurate 400. Reordering `load_bot_settings` before `otp::request` means
/// nothing is ever claimed for a misconfigured tenant, so this second call
/// gets the same accurate 400 the first one did.
#[tokio::test]
async fn second_request_from_unconfigured_tenant_is_still_bad_request_not_rate_limited() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-otp-unconfigured-retry", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-otp-unconfigured-retry").await;

    let first = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("first request-aa-otp with no site_settings row");
    assert_eq!(first.status(), reqwest::StatusCode::BAD_REQUEST);
    let first_body: Value = first.json().await.expect("json body");
    assert_eq!(
        first_body["error"], "OTP delivery is not configured for this tenant",
        "expected the disclosed not-configured message on the first attempt: {first_body}"
    );

    // Immediate retry, well within what would have been the 60s resend
    // cooldown had the first (misconfigured) attempt armed it.
    let second = http
        .post(format!("{base}/auth/request-aa-otp"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("second (immediate) request-aa-otp with no site_settings row");
    assert_eq!(
        second.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a retry from a still-unconfigured tenant must see 400, not a misleading 429"
    );
    let second_body: Value = second.json().await.expect("json body");
    assert_eq!(
        second_body["error"], "OTP delivery is not configured for this tenant",
        "expected the disclosed not-configured message on the retry too: {second_body}"
    );

    cleanup(&pool, tenant_id).await;
}
