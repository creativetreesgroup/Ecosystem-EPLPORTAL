// Backend/crates/api-gateway/tests/session_auth.rs
//! Route-level test for `api_gateway::auth::session_auth`. Per this crate's
//! testing convention: a real `axum::serve` instance + a real HTTP client
//! (`reqwest`), never calling the middleware/handler functions directly —
//! middleware ordering must be genuinely exercised over the wire. The test
//! router has two routes: `/control` with NO middleware, and `/protected`
//! with `session_auth` applied via `axum::middleware::from_fn_with_state`.
//!
//! Real Postgres (127.0.0.1:15432) for tenant/portal_user/portal_session
//! seeding. Real Redis (127.0.0.1:16379) is also touched indirectly:
//! `AppState` wraps a full `poller::PollerShared`, and `PollerShared.executor`
//! is an `executor::ExecutorHandle`, which `ExecutorHandle::connect` builds
//! against `REDIS_URL` — unused by `session_auth` itself, but required to
//! construct a real (not stubbed) `AppState`, same as `reactor-core`'s own
//! `build_state`.
use std::sync::Arc;

use api_gateway::auth::{session_auth, CurrentUser};
use api_gateway::AppState;
use axum::extract::Extension;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Duration;
use dashmap::DashMap;
use serde_json::{json, Value};
use spx_client::crypto::secret::ExposeSecret;
use spx_client::crypto::session_token::generate_session_token;
use spx_client::SpxClient;
use sqlx::PgPool;
use uuid::Uuid;

const SESSION_COOKIE_NAME: &str = "spx_session";

fn database_url() -> String {
    // 15432, not 5432 — see store's own test_database_url() comment: this
    // project's Docker/docker-compose.yml publishes tower-postgres on 15432
    // to avoid colliding with a pre-existing native Postgres on the dev host.
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

async fn insert_tenant(pool: &PgPool) -> Uuid {
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Session Auth Test Tenant")
        .bind(format!("session-auth-{tenant_id}"))
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

/// Builds a full `AppState` around the given (already-migrated) pool — same
/// construction shape as `reactor-core`'s `build_state` (Task 1), but left
/// idle (no accounts spawned, no notifier, no ws-hub Redis bridge) since this
/// test only exercises `session_auth`, never any poller/executor behavior.
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
        tenant_id: Uuid::nil(),
        cors_origins: Arc::new(Vec::new()),
        session_cookie_name: Arc::from(SESSION_COOKIE_NAME),
    }
}

async fn protected_handler(Extension(user): Extension<CurrentUser>) -> Json<Value> {
    Json(json!({ "username": user.username }))
}

async fn control_handler() -> Json<Value> {
    Json(json!({ "reached": "control" }))
}

/// `/control` has no `session_auth` layer at all (the control case);
/// `/protected` has it applied via `from_fn_with_state`, exactly the shape
/// Task 5+ route wiring will use.
fn test_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/protected", get(protected_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session_auth,
        ));

    Router::new()
        .route("/control", get(control_handler))
        .merge(protected)
        .with_state(state)
}

/// Spawns a real `axum::serve` instance on an ephemeral loopback port and
/// returns its base URL.
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, test_router(state)).await.unwrap();
    });
    format!("http://{addr}")
}

/// Case 1: no cookie at all -> `/protected` 401s (short-circuited by the
/// middleware before the handler runs), `/control` (no middleware) 200s.
#[tokio::test]
async fn no_cookie_protected_401_control_200() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    let state = build_state(pool.clone()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let protected = http
        .get(format!("{base}/protected"))
        .send()
        .await
        .expect("request protected");
    assert_eq!(protected.status(), reqwest::StatusCode::UNAUTHORIZED);

    let control = http
        .get(format!("{base}/control"))
        .send()
        .await
        .expect("request control");
    assert_eq!(control.status(), reqwest::StatusCode::OK);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// Case 2: a valid, unexpired session cookie -> `/protected` 200s, and the
/// handler can read `CurrentUser` via the `Extension` extractor and see the
/// right `username`.
#[tokio::test]
async fn valid_session_reaches_protected_route_with_current_user() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "agent-valid").await;

    let (token, hash) = generate_session_token().expect("generate token");
    store::portal_sessions::create(&pool, tenant_id, user_id, hash, None, None, Duration::hours(2))
        .await
        .expect("create session");

    let state = build_state(pool.clone()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/protected"))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE_NAME}={}", token.expose_secret()),
        )
        .send()
        .await
        .expect("request protected");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("json body");
    assert_eq!(body["username"], "agent-valid");

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// Case 3: an expired session (`expires_at` in the past) -> 401. Built via
/// `store::portal_sessions::create`'s own `ttl` param with a NEGATIVE
/// `Duration`, landing `expires_at` in the past — exercising the real store
/// function rather than hand-crafting the row, while still giving the test
/// full control over expiry.
#[tokio::test]
async fn expired_session_401() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "agent-expired").await;

    let (token, hash) = generate_session_token().expect("generate token");
    store::portal_sessions::create(
        &pool,
        tenant_id,
        user_id,
        hash,
        None,
        None,
        Duration::hours(-2),
    )
    .await
    .expect("create expired session");

    let state = build_state(pool.clone()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/protected"))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE_NAME}={}", token.expose_secret()),
        )
        .send()
        .await
        .expect("request protected");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// Case 4: a well-formed but nonexistent token (a genuine 256-bit token,
/// never persisted anywhere) -> 401, NOT a 500. Hashing a bogus token and
/// finding no matching row must be a clean "not found", never an error path
/// (`store::portal_sessions::find_valid_by_hash`'s `Ok(None)`, not `Err`).
#[tokio::test]
async fn well_formed_but_unknown_token_401_not_500() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;

    // Generated, but its hash is never stored — no INSERT into
    // portal_sessions happens for this token at all.
    let (bogus_token, _hash) = generate_session_token().expect("generate token");

    let state = build_state(pool.clone()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/protected"))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE_NAME}={}", bogus_token.expose_secret()),
        )
        .send()
        .await
        .expect("request protected");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a well-formed but nonexistent token must be a clean 401, never a 500"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

/// Case 5: a session that is otherwise entirely valid (unexpired, correct
/// hash, real user) but whose owning `portal_users` row has `enabled =
/// false` -> 401. Exercises `session_auth`'s `if !user.enabled { return
/// Err(ApiError::Unauthorized); }` branch specifically (middleware.rs
/// ~line 65), which none of cases 1-4 above touch: the user is disabled
/// AFTER the session is created, via a raw `UPDATE portal_users` against the
/// same test pool, so the session row itself is never invalid — only the
/// user lookup's `enabled` flag is.
#[tokio::test]
async fn disabled_user_session_returns_401() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "agent-disabled").await;

    sqlx::query("UPDATE portal_users SET enabled = false WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("disable portal_user");

    let (token, hash) = generate_session_token().expect("generate token");
    store::portal_sessions::create(&pool, tenant_id, user_id, hash, None, None, Duration::hours(2))
        .await
        .expect("create session");

    let state = build_state(pool.clone()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/protected"))
        .header(
            reqwest::header::COOKIE,
            format!("{SESSION_COOKIE_NAME}={}", token.expose_secret()),
        )
        .send()
        .await
        .expect("request protected");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a valid, unexpired session for a disabled portal user must be rejected"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
