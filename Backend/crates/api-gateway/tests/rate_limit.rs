// Backend/crates/api-gateway/tests/rate_limit.rs
//! Route-level test for Task 8's per-IP rate limiter on
//! `POST /auth/portal-login` (`middleware::login_rate_limit_layer`, applied
//! via `.route_layer(...)` scoped to just that route in
//! `routes/auth.rs::auth_router`). Same convention as every other route test
//! in this crate: a real `axum::serve` instance built from
//! `api_gateway::build_router` (the exact router `reactor-core` mounts) + a
//! real HTTP client (`reqwest`).
//!
//! ## Why this is a real-time test, not `tokio::time::pause`/`advance`
//!
//! This project's standard timing-test convention (`tokio::time::pause`/
//! `advance`) does NOT compose with `tower_governor`'s internal clock:
//! `governor`'s `DefaultClock` is backed by the `quanta` crate, which reads
//! either the CPU's TSC or `std::time::Instant` directly — NEITHER of which
//! is affected by Tokio's simulated/paused clock (that only advances
//! `tokio::time::Instant`/timers, a completely separate clock source).
//! Verified by reading `governor-0.10.4`'s `clock.rs` (`DefaultClock`
//! resolves to a `quanta`-backed clock) rather than assuming either way.
//! Per the task brief's own documented fallback (and this project's Fase 5
//! watchdog-test precedent), this test uses REAL wall-clock time instead of
//! fighting the crate's actual clock source: it fires a batch of real HTTP
//! requests over loopback in quick succession and asserts on the observed
//! statuses, using the SAME production budget
//! (`middleware::rate_limit::LOGIN_BURST_SIZE` = 20,
//! `LOGIN_REPLENISH_PERIOD_SECS` = 3) rather than a separate test-only layer
//! construction — the task brief's specified `login_rate_limit_layer()`
//! signature takes no parameters, so production and test share one budget.
//! To stay robust against real per-request latency (each `/auth/portal-login`
//! call pays a full argon2id `verify_password` cost, per `routes/auth.rs`'s
//! timing-safety doc comment) without becoming flaky if that pushes the
//! whole batch's wall-clock duration close to the 3-second replenish period,
//! this test fires MORE requests (30) than the burst budget could plausibly
//! absorb even with a few extra tokens trickling back in mid-batch, and
//! asserts only that AT LEAST ONE `429` appears — not that a specific
//! numbered request is the first to be throttled.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
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

/// Same minimal construction shape as `tests/cors_and_body_limit.rs`'s
/// `build_state` — this test never inserts a tenant or portal user (every
/// hammered request uses a nonexistent username, so `portal_login` always
/// takes the "unknown user" branch: a real Postgres query that finds
/// nothing, then a dummy `verify_password` call — no FK-constrained rows
/// needed to exercise the rate limiter).
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
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    };

    AppState {
        poller: Arc::new(poller_shared),
        ws_hub: ws_hub::Hub::new(),
        tenant_id: uuid::Uuid::new_v4(),
        cors_origins: Arc::new(Vec::new()),
        session_cookie_name: Arc::from(SESSION_COOKIE_NAME),
        cookie_secure: true,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

/// Spawns a real `axum::serve` instance with
/// `.into_make_service_with_connect_info::<SocketAddr>()` — see this file's
/// top doc comment and `middleware::rate_limit`'s doc comment for why: the
/// `SmartIpKeyExtractor` this route's limiter uses needs SOME way to
/// identify the caller, and this test sends no `X-Forwarded-For` header, so
/// it exercises the peer-IP fallback path instead (every request in this
/// test comes from the same loopback client, so it's recognized as one IP
/// either way — exactly the "SAME client, one IP" setup the task requires).
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

/// Hammers `POST /auth/portal-login` from the same client well past the
/// configured burst (20) and asserts a `429 Too Many Requests` shows up —
/// then, in the SAME test, confirms `GET /healthz` (a different, unthrottled
/// route) still returns `200`, proving the limiter is scoped to the login
/// route specifically, not applied globally.
#[tokio::test]
async fn login_route_is_rate_limited_but_healthz_is_not() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let state = build_state(pool).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let mut saw_429 = false;
    for i in 0..30 {
        let resp = http
            .post(format!("{base}/auth/portal-login"))
            .json(&serde_json::json!({
                "username": format!("nonexistent-user-{i}"),
                "password": "whatever",
            }))
            .send()
            .await
            .expect("request portal-login");

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }

        // Every non-throttled response on this path must be 401 (unknown
        // username) — anything else means the rate limiter's key-extraction
        // fallback isn't wired the way this test expects (e.g. a
        // `500 Internal Server Error` would mean `SmartIpKeyExtractor`
        // failed to identify the caller at all — see this file's and
        // `middleware::rate_limit`'s doc comments on why
        // `into_make_service_with_connect_info` is required).
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "request {i} was neither 401 (unknown user) nor 429 (rate-limited)"
        );
    }

    assert!(
        saw_429,
        "expected a 429 Too Many Requests within 30 rapid POST /auth/portal-login requests \
         from the same client (burst budget is 20)"
    );

    // Scoping check, in the SAME test: a DIFFERENT, unthrottled route must
    // still return 200 even while the login route above is exhausted.
    let healthz_resp = http
        .get(format!("{base}/healthz"))
        .send()
        .await
        .expect("request /healthz");
    assert_eq!(
        healthz_resp.status(),
        reqwest::StatusCode::OK,
        "the rate limiter must be scoped to /auth/portal-login only, not global"
    );
}
