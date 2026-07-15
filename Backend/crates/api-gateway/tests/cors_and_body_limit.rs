// Backend/crates/api-gateway/tests/cors_and_body_limit.rs
//! Route-level tests for Task 7's two global layers wired in
//! `api_gateway::build_router`: `middleware::cors_layer` (exact-match CORS
//! allowlist) and `tower_http::limit::RequestBodyLimitLayer` (1.5MB global
//! body-limit). Same convention as `tests/security_headers.rs`: a real
//! `axum::serve` instance built from `api_gateway::build_router` (the exact
//! router `reactor-core` mounts) + a real HTTP client (`reqwest`) — CORS and
//! body-limit are `tower_http` `Layer`s that only take effect over the wire
//! through the actual `Service` stack, never by calling any function
//! directly.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use spx_client::SpxClient;
use sqlx::PgPool;

const SESSION_COOKIE_NAME: &str = "spx_session";
const ALLOWED_ORIGIN: &str = "https://allowed.example";

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

/// Same construction shape as `tests/security_headers.rs`'s `build_state`,
/// with a real (non-empty) `cors_origins` allowlist so the CORS layer has a
/// configured origin to match against.
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
        cors_origins: Arc::new(vec![ALLOWED_ORIGIN.to_string()]),
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

/// Case 1: a request carrying `Origin: <a configured allowed origin>` gets
/// back `Access-Control-Allow-Origin` echoing that EXACT origin string, plus
/// `Access-Control-Allow-Credentials: true` (required for cookies to flow
/// cross-origin — session auth in this crate is cookie-based).
#[tokio::test]
async fn allowed_origin_gets_echoed_back_with_credentials() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let state = build_state(pool).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/healthz"))
        .header(reqwest::header::ORIGIN, ALLOWED_ORIGIN)
        .send()
        .await
        .expect("request /healthz with an allowed Origin");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .expect("Access-Control-Allow-Origin must be present for a configured origin")
            .to_str()
            .unwrap(),
        ALLOWED_ORIGIN,
        "must echo back the EXACT configured origin string, never a wildcard"
    );
    assert_eq!(
        resp.headers()
            .get("access-control-allow-credentials")
            .expect("Access-Control-Allow-Credentials must be present")
            .to_str()
            .unwrap(),
        "true"
    );
}

/// Case 2: a request carrying an UNLISTED `Origin` gets NO
/// `Access-Control-Allow-Origin` header at all — proving rejection via
/// omission, not a permissive fallback that echoes every origin back.
///
/// Empirically confirmed by reading `tower_http` 0.7.0's
/// `cors::allow_origin::AllowOrigin::to_future` source (list variant:
/// `origin.filter(|o| l.contains(o)).map(...)`) AND this test together: an
/// unmatched origin makes the header be OMITTED entirely — tower_http does
/// NOT turn a CORS mismatch into any kind of 4xx/5xx error response; the
/// underlying request is still handled normally (still `200 OK` here), it
/// just never receives the opt-in header a browser's Fetch spec requires
/// before exposing the response to the requesting page's JS. Server-side CORS
/// headers are advisory to browsers, not an access-control mechanism a
/// non-browser HTTP client is bound by — which is exactly why this test
/// still gets a normal `200 OK` body back over the wire and only asserts on
/// the header's absence.
#[tokio::test]
async fn unlisted_origin_gets_no_allow_origin_header() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let state = build_state(pool).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/healthz"))
        .header(reqwest::header::ORIGIN, "https://evil.example")
        .send()
        .await
        .expect("request /healthz with an unlisted Origin");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(
        resp.headers().get("access-control-allow-origin").is_none(),
        "an unlisted Origin must NOT get Access-Control-Allow-Origin back — a permissive \
         fallback here would defeat the entire allowlist"
    );
}

/// Case 3: a request with a body over 1.5MB gets `413 Payload Too Large`.
/// Constructs a REAL oversized body (1,500,001 bytes — exactly 1 over the
/// `1_500_000`-byte limit) rather than merely asserting the layer is present
/// in code. Posted to `/auth/portal-login` (a route that genuinely accepts a
/// request body) with a plain `Vec<u8>` body — reqwest sets `Content-Length`
/// from it automatically, and `tower_http::limit::RequestBodyLimitLayer`
/// rejects purely from that header, before the request ever reaches
/// routing/JSON-deserialization — so the oversized bytes don't need to be
/// valid JSON for this to prove the limit is enforced.
#[tokio::test]
async fn oversized_body_gets_413() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let state = build_state(pool).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let oversized_body = vec![0u8; 1_500_001];
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .body(oversized_body)
        .send()
        .await
        .expect("request /auth/portal-login with an oversized body");

    assert_eq!(resp.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
}
