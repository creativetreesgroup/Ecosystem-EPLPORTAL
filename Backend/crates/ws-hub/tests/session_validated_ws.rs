// Backend/crates/ws-hub/tests/session_validated_ws.rs
//! Task 10: `ws_router_with_auth`'s real session validation, following
//! `local_broadcast.rs`'s established real-`axum::serve` + real
//! `tokio_tungstenite::connect_async` client pattern exactly. A real WS
//! client connecting with a VALID, unexpired `?session=` (backed by a real
//! seeded Postgres `portal_sessions` row) succeeds and receives the
//! `connected` greeting; a bogus/nonexistent token is rejected BEFORE the WS
//! handshake completes — asserted via the exact HTTP status
//! `tokio_tungstenite::connect_async` surfaces for a non-101 response
//! (`tungstenite::Error::Http`, carrying the real response), not just "the
//! call failed somehow".
//!
//! Real Postgres (127.0.0.1:15432), same seeding pattern as
//! `api-gateway/tests/session_auth.rs`/Task 2-3's tests: a throwaway
//! `tenants` row, a `portal_users` row, and a real `portal_sessions` row via
//! `store::portal_sessions::create` — never a hand-crafted token/hash;
//! `spx_client::crypto::session_token::generate_session_token` is the real
//! token-issuance path, same one `reactor-core`'s login route uses.
use std::sync::Arc;

use chrono::Duration;
use futures::StreamExt;
use spx_client::crypto::secret::ExposeSecret;
use spx_client::crypto::session_token::{generate_session_token, hash_session_token};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::COOKIE;
use tokio_tungstenite::tungstenite::Message as CM;
use uuid::Uuid;
use ws_hub::{ws_router_with_auth, Hub, SessionValidator};

/// The `HttpOnly` cookie name `ws_router_with_auth` falls back to when
/// `?session=` is empty (Fase 7a). Matches `reactor-core`'s own
/// `SESSION_COOKIE_NAME` default (`main.rs`'s `env_or("SESSION_COOKIE_NAME",
/// "spx_session")`) for realism, though any name would do here since this
/// file wires it explicitly rather than through env vars.
const COOKIE_NAME: &str = "spx_session";

fn cookie_name() -> Arc<str> {
    Arc::from(COOKIE_NAME)
}

fn database_url() -> String {
    // 15432, not 5432 — see `store`'s own `test_database_url()` comment:
    // this project's Docker/docker-compose.yml publishes tower-postgres on
    // 15432 to avoid colliding with a pre-existing native Postgres on the
    // dev host.
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

async fn insert_tenant(pool: &PgPool) -> Uuid {
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("WS Session Validation Test Tenant")
        .bind(format!("ws-session-validated-{tenant_id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    tenant_id
}

async fn insert_portal_user(pool: &PgPool, tenant_id: Uuid, username: &str) -> Uuid {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO portal_users (tenant_id, username, password_hash, display_name) \
         VALUES ($1, $2, 'test-hash', $3) RETURNING id",
    )
    .bind(tenant_id)
    .bind(username)
    .bind(format!("Display {username}"))
    .fetch_one(pool)
    .await
    .expect("insert portal_user");
    row.0
}

async fn cleanup_tenant(pool: &PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

/// Same shape `reactor-core`'s real `session_validator` (Task 10, `main.rs`)
/// builds: hash the plaintext token, look it up via `find_valid_by_hash`
/// (already filters `expires_at > now()` — migration 0018's `SECURITY
/// DEFINER` function), `Ok(Some(_))` means valid-and-unexpired.
fn test_validator(pool: PgPool) -> SessionValidator {
    Arc::new(move |token: String| {
        let pool = pool.clone();
        Box::pin(async move {
            let hash = hash_session_token(&token);
            matches!(
                store::portal_sessions::find_valid_by_hash(&pool, hash).await,
                Ok(Some(_))
            )
        })
    })
}

/// Case 1: a valid, unexpired session token as `?session=` succeeds — the
/// upgrade completes (HTTP 101) and the client receives the `connected`
/// greeting, same established behavior the no-auth `ws_router` already has.
#[tokio::test]
async fn valid_session_upgrade_succeeds_and_greets() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "ws-agent-valid").await;

    let (token, hash) = generate_session_token().expect("generate token");
    store::portal_sessions::create(&pool, tenant_id, user_id, hash, None, None, Duration::hours(2))
        .await
        .expect("create session");

    let hub = Hub::new();
    let app = ws_router_with_auth(hub.clone(), test_validator(pool.clone()), cookie_name());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let url = format!("ws://{addr}/ws?session={}", token.expose_secret());
    let (mut ws, resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("a valid session must complete the WS handshake");
    assert_eq!(resp.status().as_u16(), 101, "handshake must succeed with 101 Switching Protocols");

    // First frame is the `connected` greeting.
    let first = ws.next().await.unwrap().unwrap();
    assert!(matches!(first, CM::Text(ref t) if t.contains("connected")));

    cleanup_tenant(&pool, tenant_id).await;
}

/// Case 2: a bogus, never-persisted token is rejected BEFORE the WS
/// handshake completes — `connect_async` itself errors with
/// `tungstenite::Error::Http`, carrying the real `401 Unauthorized`
/// `ws_handler_with_auth` returns, never a socket that opens and then
/// immediately closes.
#[tokio::test]
async fn bogus_session_rejected_before_handshake() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let hub = Hub::new();
    let app = ws_router_with_auth(hub.clone(), test_validator(pool.clone()), cookie_name());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // A genuine 256-bit token shape, but its hash was never persisted to
    // `portal_sessions` — proves a clean "not found" rejection, not a
    // malformed-input special case.
    let (bogus_token, _hash) = generate_session_token().expect("generate token");
    let url = format!("ws://{addr}/ws?session={}", bogus_token.expose_secret());

    let err = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("a bogus session must never complete the WS handshake");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(
                resp.status().as_u16(),
                401,
                "must be rejected with exactly 401 Unauthorized, not any other status"
            );
        }
        other => panic!("expected Error::Http(401 Unauthorized), got: {other:?}"),
    }

    assert!(
        !hub.has_channel(bogus_token.expose_secret()),
        "a rejected upgrade must never register a channel in the hub"
    );
}

/// Case 3 (Fase 7a): a real browser stores the session token in an
/// `HttpOnly` cookie and has no way to read it back to construct a
/// `?session=` query string itself — it can only rely on the browser
/// automatically attaching the `Cookie:` header on the WS handshake request.
/// A valid, unexpired session token supplied ONLY via a `Cookie:` header
/// (no `?session=` at all) must still succeed and greet, proving the
/// cookie-fallback path closes that gap.
#[tokio::test]
async fn cookie_only_session_with_no_query_param_upgrades_successfully() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "ws-agent-cookie-valid").await;

    let (token, hash) = generate_session_token().expect("generate token");
    store::portal_sessions::create(&pool, tenant_id, user_id, hash, None, None, Duration::hours(2))
        .await
        .expect("create session");

    let hub = Hub::new();
    let app = ws_router_with_auth(hub.clone(), test_validator(pool.clone()), cookie_name());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // No `?session=` query string at all — only a `Cookie:` header, exactly
    // what a real browser sends automatically for an `HttpOnly` cookie.
    let url = format!("ws://{addr}/ws");
    let mut request = url.into_client_request().expect("build ws client request");
    request.headers_mut().insert(
        COOKIE,
        format!("{COOKIE_NAME}={}", token.expose_secret())
            .parse()
            .expect("valid Cookie header value"),
    );

    let (mut ws, resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("a valid session cookie alone must complete the WS handshake");
    assert_eq!(resp.status().as_u16(), 101, "handshake must succeed with 101 Switching Protocols");

    // First frame is the `connected` greeting.
    let first = ws.next().await.unwrap().unwrap();
    assert!(matches!(first, CM::Text(ref t) if t.contains("connected")));

    cleanup_tenant(&pool, tenant_id).await;
}

/// Case 4 (Fase 7a): mirrors `bogus_session_rejected_before_handshake` above,
/// but the bogus token is supplied via a `Cookie:` header instead of
/// `?session=` — the cookie fallback must reject a bogus/never-persisted
/// token exactly the same way (a clean pre-upgrade 401, not a 101 that then
/// immediately closes).
#[tokio::test]
async fn bogus_cookie_with_no_query_param_is_rejected_before_upgrade() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");

    let hub = Hub::new();
    let app = ws_router_with_auth(hub.clone(), test_validator(pool.clone()), cookie_name());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // A genuine 256-bit token shape, but its hash was never persisted to
    // `portal_sessions` — proves a clean "not found" rejection, not a
    // malformed-input special case.
    let (bogus_token, _hash) = generate_session_token().expect("generate token");
    let url = format!("ws://{addr}/ws");
    let mut request = url.into_client_request().expect("build ws client request");
    request.headers_mut().insert(
        COOKIE,
        format!("{COOKIE_NAME}={}", bogus_token.expose_secret())
            .parse()
            .expect("valid Cookie header value"),
    );

    let err = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("a bogus session cookie must never complete the WS handshake");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(
                resp.status().as_u16(),
                401,
                "must be rejected with exactly 401 Unauthorized, not any other status"
            );
        }
        other => panic!("expected Error::Http(401 Unauthorized), got: {other:?}"),
    }

    assert!(
        !hub.has_channel(bogus_token.expose_secret()),
        "a rejected upgrade must never register a channel in the hub"
    );
}
