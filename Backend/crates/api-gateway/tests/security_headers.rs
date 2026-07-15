// Backend/crates/api-gateway/tests/security_headers.rs
//! Route-level test for Task 6's global security-headers middleware
//! (`api_gateway::middleware::security_headers`). Same convention as
//! `tests/session_auth.rs` and `tests/auth_routes.rs`: a real `axum::serve`
//! instance built from `api_gateway::build_router` (the exact router
//! `reactor-core` mounts) + a real HTTP client (`reqwest`) — never calling
//! the middleware function directly.
//!
//! Exercised on TWO different routes to prove the layer is genuinely global
//! (applied once, outermost, in `build_router`) rather than something one
//! handler happens to set on its own response:
//! - `GET /healthz` — a plain, unauthenticated 200 with no middleware of its
//!   own at all.
//! - `GET /auth/me` with NO cookie — a 401 produced by `ApiError`'s
//!   `IntoResponse` impl via `session_auth` short-circuiting before the
//!   handler ever runs. This never touches Postgres (the missing-cookie
//!   check in `session_auth` returns before any `store::` call), but a real
//!   `AppState` is still built the same way the other route tests in this
//!   crate do, for consistency and because `build_router` requires one.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use reqwest::header::HeaderMap;
use spx_client::SpxClient;
use sqlx::PgPool;

const SESSION_COOKIE_NAME: &str = "spx_session";

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

/// Same construction shape as `tests/session_auth.rs`'s `build_state`.
async fn build_state(pool: PgPool) -> AppState {
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
        tenant_id: uuid::Uuid::nil(),
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
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, api_gateway::build_router(state))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

/// Asserts all 7 headers Task 6 mandates are present with the EXACT values
/// `security_headers.rs` sets — not just "present", since a subtly wrong CSP
/// or a stale `X-XSS-Protection: 1; mode=block` value would defeat the
/// point of this test.
fn assert_security_headers(headers: &HeaderMap, route: &str) {
    let get = |name: &str| {
        headers
            .get(name)
            .unwrap_or_else(|| panic!("{route}: missing {name} header"))
            .to_str()
            .unwrap()
    };

    assert_eq!(
        get("strict-transport-security"),
        "max-age=31536000; includeSubDomains",
        "{route}: Strict-Transport-Security"
    );
    assert_eq!(get("x-frame-options"), "DENY", "{route}: X-Frame-Options");
    assert_eq!(
        get("x-content-type-options"),
        "nosniff",
        "{route}: X-Content-Type-Options"
    );
    assert_eq!(
        get("x-xss-protection"),
        "0",
        "{route}: X-XSS-Protection must be the modern '0' (disable legacy filter), not the stale \
         reference value"
    );
    assert_eq!(
        get("referrer-policy"),
        "strict-origin-when-cross-origin",
        "{route}: Referrer-Policy"
    );
    assert_eq!(
        get("permissions-policy"),
        "geolocation=(), camera=(), microphone=()",
        "{route}: Permissions-Policy"
    );
    assert_eq!(
        get("content-security-policy"),
        "default-src 'none'; frame-ancestors 'none'; base-uri 'none'",
        "{route}: Content-Security-Policy (the reference has none at all — this is the genuine \
         improvement Task 6 adds)"
    );
}

/// Case 1: `GET /healthz` — a plain 200 with no per-handler header logic of
/// its own — still carries all 7 security headers.
#[tokio::test]
async fn healthz_200_carries_all_security_headers() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let state = build_state(pool).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/healthz"))
        .send()
        .await
        .expect("request /healthz");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_security_headers(resp.headers(), "/healthz");
}

/// Case 2: `GET /auth/me` with NO cookie -> a 401 produced entirely by
/// `session_auth`'s `ApiError::Unauthorized` short-circuit + `ApiError`'s
/// `IntoResponse` impl, neither of which know anything about security
/// headers. If these headers still show up here, they can only have been
/// added by the OUTERMOST layer in `build_router`, proving genuine global
/// application rather than something wired into one handler's own response
/// construction.
#[tokio::test]
async fn unauthenticated_401_still_carries_all_security_headers() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let state = build_state(pool).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/auth/me"))
        .send()
        .await
        .expect("request /auth/me");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_security_headers(resp.headers(), "/auth/me (401, no cookie)");
}
