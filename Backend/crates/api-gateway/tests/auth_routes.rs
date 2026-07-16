// Backend/crates/api-gateway/tests/auth_routes.rs
//! Route-level tests for `POST /auth/portal-login`, `GET /auth/me`, and
//! `POST /auth/logout` (Task 5). Same convention as `tests/session_auth.rs`:
//! a real `axum::serve` instance + a real HTTP client (`reqwest`) — the
//! router built here is `api_gateway::build_router`, the exact one
//! `reactor-core` mounts, not a hand-rolled test-only router.
//!
//! Real Postgres (127.0.0.1:15432) is used to seed a `portal_users` row with
//! a KNOWN password (via `spx_client::crypto::password::hash_password`, the
//! same function `portal_login` verifies against) and to inspect
//! `portal_sessions` directly. Real Redis (127.0.0.1:16379) is touched only
//! indirectly to construct a genuine (not stubbed) `AppState`, same as
//! `tests/session_auth.rs`.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use serde_json::Value;
use spx_client::crypto::password::hash_password;
use spx_client::SpxClient;
use sqlx::PgPool;
use uuid::Uuid;

const SESSION_COOKIE_NAME: &str = "spx_session";
const KNOWN_PASSWORD: &str = "correct horse battery staple 42";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

/// A throwaway 32-byte master key for `AppState.master_key` (Task 1) — this
/// test file never exercises envelope-encryption itself, so a fixed key is
/// enough to satisfy the field's type.
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes(
        [7u8; 32],
    ))
}

/// Real Redis connection for `AppState.redis` (Task 1's OTP-gate field) —
/// not `Option`, so a real, live `ConnectionManager` is required to
/// construct any `AppState` at all, even a test one that never touches the
/// OTP routes.
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
        .bind("Auth Routes Test Tenant")
        .bind(format!("auth-routes-{tenant_id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    tenant_id
}

/// Inserts a portal user with a REAL argon2id hash of `KNOWN_PASSWORD` (not
/// the `'test-hash'` placeholder `tests/session_auth.rs` uses, since these
/// tests exercise `verify_password` for real, not just session-cookie
/// lookup).
async fn insert_portal_user(pool: &PgPool, tenant_id: Uuid, username: &str) -> Uuid {
    let hash = hash_password(KNOWN_PASSWORD).expect("hash known password");
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO portal_users (tenant_id, username, password_hash, display_name, is_main_account) \
         VALUES ($1, $2, $3, $4, true) RETURNING id",
    )
    .bind(tenant_id)
    .bind(username)
    .bind(&hash)
    .bind(format!("Display {username}"))
    .fetch_one(pool)
    .await
    .expect("insert portal_user");
    row.0
}

/// Same construction shape as `tests/session_auth.rs`'s `build_state` (Task
/// 3), extended with Task 5's `cookie_secure` field. Left idle (no accounts
/// spawned, no notifier, no ws-hub Redis bridge) — these tests only exercise
/// the auth routes.
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

/// Spawns a real `axum::serve` instance (the SAME `build_router` as
/// `reactor-core`'s `app()`) on an ephemeral loopback port and returns its
/// base URL.
///
/// `.into_make_service_with_connect_info::<SocketAddr>()` (Task 8): every
/// test in this file sends at least one `POST /auth/portal-login`, which is
/// now behind `middleware::login_rate_limit_layer`'s `SmartIpKeyExtractor` —
/// it needs SOME way to identify the caller. This test's `reqwest::Client`
/// never sends an `X-Forwarded-For` header, so the extractor falls back to
/// `ConnectInfo<SocketAddr>`, which (per `axum-0.8.9`'s `routing/mod.rs` and
/// `tower_governor`'s own README "Common pitfalls" #2) is only populated
/// with this connect-info wiring, never by the plain `Router` `axum::serve`
/// used pre-Task-8. Without it every `/auth/portal-login` request in this
/// file would fail key extraction and 500, not the status each test asserts.
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

/// Extracts the raw `Set-Cookie` header value (if any) so a test can both
/// assert on its attributes (Task 5's `HttpOnly`/`SameSite=Strict` security
/// requirement) and forward the cookie manually on a follow-up request
/// (`reqwest::Client` doesn't auto-manage cookies unless built with a jar,
/// and manual forwarding makes exactly which cookie is being reused explicit
/// in each test).
fn set_cookie_header(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_string())
}

/// From a raw `Set-Cookie: name=value; Path=/; ...` header, extract just the
/// `name=value` pair suitable for a `Cookie:` request header.
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("Set-Cookie header has at least one ';'-delimited segment")
        .to_string()
}

/// Case 1: correct username + password -> 200, a `Set-Cookie` header is
/// present with the required security attributes (`HttpOnly`,
/// `SameSite=Strict`, `Secure`), and the JSON body has the right
/// `username`/`display_name`/`is_main_account` — and critically, the
/// response body does NOT itself contain the session token value (the token
/// only appears in the `Set-Cookie` header).
#[tokio::test]
async fn correct_login_returns_200_with_cookie_and_no_token_in_body() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "agent-correct").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": "agent-correct",
            "password": KNOWN_PASSWORD,
        }))
        .send()
        .await
        .expect("request portal-login");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let set_cookie = set_cookie_header(&resp).expect("Set-Cookie header present on success");
    assert!(
        set_cookie.contains(SESSION_COOKIE_NAME),
        "Set-Cookie must carry the session cookie: {set_cookie}"
    );
    assert!(
        set_cookie.to_lowercase().contains("httponly"),
        "session cookie must be HttpOnly: {set_cookie}"
    );
    assert!(
        set_cookie.contains("SameSite=Strict"),
        "session cookie must be SameSite=Strict: {set_cookie}"
    );
    assert!(
        set_cookie.to_lowercase().contains("secure"),
        "session cookie must be Secure (state.cookie_secure=true in this test): {set_cookie}"
    );

    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["username"], "agent-correct");
    assert_eq!(body["display_name"], "Display agent-correct");
    assert_eq!(body["is_main_account"], true);

    // The plaintext session token must NEVER appear in the JSON body — only
    // in the Set-Cookie header. The cookie value is the part after `=` and
    // before the first `;` in the Set-Cookie header.
    let token_value = cookie_pair(&set_cookie)
        .split_once('=')
        .expect("cookie pair has a value")
        .1
        .to_string();
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(
        !body_str.contains(&token_value),
        "session token must not leak into the JSON response body"
    );

    cleanup(&pool, tenant_id).await;
}

/// Case 2: known username, WRONG password -> 401.
#[tokio::test]
async fn wrong_password_returns_401_no_cookie() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "agent-wrongpw").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": "agent-wrongpw",
            "password": "definitely-not-the-right-password",
        }))
        .send()
        .await
        .expect("request portal-login");

    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        set_cookie_header(&resp).is_none(),
        "no Set-Cookie must be issued on a failed login"
    );
    let body: Value = resp.json().await.expect("json body");

    cleanup(&pool, tenant_id).await;

    // Stash for the enumeration-parity comparison below (re-fetched by the
    // next test independently — see that test's own comment for why the
    // comparison is done within a single test rather than across two).
    let _ = body;
}

/// Case 3: an UNKNOWN username -> 401, with the EXACT SAME response shape
/// (status + JSON body) as case 2's wrong-password response. This is the
/// core username-enumeration-prevention assertion: the caller must not be
/// able to tell "no such user" apart from "wrong password" by inspecting the
/// HTTP response. Both sub-cases are exercised in the SAME test (rather than
/// comparing across two independent `#[tokio::test]` functions) so the
/// comparison is a real, direct assertion instead of an implicit "both
/// happen to say 401" coincidence.
#[tokio::test]
async fn unknown_username_and_wrong_password_are_indistinguishable() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "agent-real").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let wrong_password_resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": "agent-real",
            "password": "definitely-wrong",
        }))
        .send()
        .await
        .expect("request portal-login (wrong password)");
    let wrong_password_status = wrong_password_resp.status();
    let wrong_password_cookie = set_cookie_header(&wrong_password_resp);
    let wrong_password_body: Value = wrong_password_resp.json().await.expect("json body");

    let unknown_user_resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": "agent-does-not-exist-at-all",
            "password": "whatever",
        }))
        .send()
        .await
        .expect("request portal-login (unknown username)");
    let unknown_user_status = unknown_user_resp.status();
    let unknown_user_cookie = set_cookie_header(&unknown_user_resp);
    let unknown_user_body: Value = unknown_user_resp.json().await.expect("json body");

    assert_eq!(wrong_password_status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong_password_status, unknown_user_status,
        "wrong-password and unknown-username must return the identical status code"
    );
    assert_eq!(
        wrong_password_body, unknown_user_body,
        "wrong-password and unknown-username must return the identical JSON body shape/content \
         (a username-enumeration leak would show a different error string here)"
    );
    assert!(wrong_password_cookie.is_none());
    assert!(unknown_user_cookie.is_none());

    cleanup(&pool, tenant_id).await;
}

/// Case 4a: `GET /auth/me` WITH the cookie from a successful login -> 200,
/// body matches the logged-in user. Case 4b: `GET /auth/me` with NO cookie
/// at all -> 401.
#[tokio::test]
async fn me_requires_valid_session_cookie() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "agent-me").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let login_resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": "agent-me",
            "password": KNOWN_PASSWORD,
        }))
        .send()
        .await
        .expect("request portal-login");
    assert_eq!(login_resp.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(&set_cookie_header(&login_resp).expect("Set-Cookie present"));

    // 4a: with the cookie.
    let me_resp = http
        .get(format!("{base}/auth/me"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request /auth/me with cookie");
    assert_eq!(me_resp.status(), reqwest::StatusCode::OK);
    let body: Value = me_resp.json().await.expect("json body");
    assert_eq!(body["username"], "agent-me");
    assert_eq!(body["display_name"], "Display agent-me");
    assert_eq!(body["is_main_account"], true);

    // 4b: no cookie at all.
    let me_resp_no_cookie = http
        .get(format!("{base}/auth/me"))
        .send()
        .await
        .expect("request /auth/me without cookie");
    assert_eq!(
        me_resp_no_cookie.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    cleanup(&pool, tenant_id).await;
}

/// Case 5: `POST /auth/logout` then a SUBSEQUENT `GET /auth/me` with the
/// SAME (now-deleted) cookie -> 401. Proves the session row was actually
/// deleted server-side (`store::portal_sessions::delete`), not merely that
/// the client's local cookie was cleared — reusing the exact same cookie
/// value after logout must fail.
#[tokio::test]
async fn logout_deletes_session_so_subsequent_me_fails() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "agent-logout").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let login_resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": "agent-logout",
            "password": KNOWN_PASSWORD,
        }))
        .send()
        .await
        .expect("request portal-login");
    assert_eq!(login_resp.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(&set_cookie_header(&login_resp).expect("Set-Cookie present"));

    // Sanity: the session is valid before logout.
    let me_before = http
        .get(format!("{base}/auth/me"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request /auth/me before logout");
    assert_eq!(me_before.status(), reqwest::StatusCode::OK);

    let logout_resp = http
        .post(format!("{base}/auth/logout"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request /auth/logout");
    assert_eq!(logout_resp.status(), reqwest::StatusCode::OK);

    // Reusing the EXACT SAME cookie value after logout must now fail — this
    // is only possible if the server-side `portal_sessions` row was deleted,
    // since nothing about the client's request changed between this call and
    // the "before logout" call above.
    let me_after = http
        .get(format!("{base}/auth/me"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request /auth/me after logout");
    assert_eq!(
        me_after.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "reusing a logged-out session's cookie must fail — proves server-side deletion, not just \
         a client-side cookie clear"
    );

    cleanup(&pool, tenant_id).await;
}
