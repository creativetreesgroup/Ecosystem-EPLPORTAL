use std::sync::Arc;

use api_gateway::AppState;
use axum::Router;
use dashmap::DashMap;
use uuid::Uuid;

/// `key` from the environment, or `default` if unset/empty. Every fallback
/// below matches an already-established convention elsewhere in the
/// workspace (`store`'s `test_database_url()`, `executor`/`poller`'s
/// `REDIS_URL` test helpers, `auth-sidecar`'s `SPX_BASE_URL`) so a bare
/// `cargo run` against the dev `docker-compose` stack (Postgres on 15432,
/// Redis on 16379) boots with zero required env vars.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Task 1 (Fase 6a) scope: prove `api-gateway` wires into `reactor-core`
/// end-to-end with a REAL (not stubbed) `AppState`/`PollerShared` — a live
/// Postgres pool, a real `ExecutorHandle`/`SpxClient`/`SidecarClient` — but
/// otherwise IDLE: zero accounts spawned, no notifier, no ws-hub Redis
/// bridge, and a placeholder `tenant_id` (parsed from `TENANT_ID` if set,
/// else `Uuid::nil()`). The real account-bootstrap sequence — resolving the
/// single deployment tenant from `TENANT_SLUG`/`TENANT_ID`, spawning one
/// poller task per `agency_credentials` row via
/// `poller::schedule::ensure_restored_then_spawn`, wiring the ws-hub Redis
/// bridge and the notifier — is Task 9's job later in this same plan, not
/// this one.
async fn build_state() -> AppState {
    let database_url = env_or(
        "DATABASE_URL",
        "postgres://tower:tower_dev_only@127.0.0.1:15432/tower",
    );
    let redis_url = env_or("REDIS_URL", "redis://127.0.0.1:16379");
    let spx_base_url = env_or("SPX_BASE_URL", "https://logistics.myagencyservice.id");
    let sidecar_url = env_or("AUTH_SIDECAR_URL", "http://127.0.0.1:8082");
    let tenant_id = std::env::var("TENANT_ID")
        .ok()
        .and_then(|s| Uuid::parse_str(&s).ok())
        .unwrap_or_else(Uuid::nil);

    let pool = store::connect(&database_url)
        .await
        .expect("reactor-core: connect to Postgres");

    // `ExecutorHandle::connect` only PARSES `redis_url` + best-effort loads
    // the claim script; it does not require Redis to be reachable right now
    // to succeed (see `executor::gate::ExecutorHandle::connect`'s doc
    // comment) — genuinely "freshly-connected but otherwise idle".
    let executor = executor::ExecutorHandle::connect(&redis_url)
        .await
        .expect("reactor-core: init executor redis pool");

    let client = spx_client::SpxClient::new(spx_base_url).expect("reactor-core: build SpxClient");
    let sidecar = poller::SidecarClient::new(sidecar_url);

    let poller_shared = poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool,
        config: poller::PollerConfig::from_env(),
        // Idle: no accounts spawned yet (Task 9).
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        // No fire-and-forget WAHA/n8n notifications yet (Task 9/6b).
        notifier: None,
        // No ws-hub Redis bridge yet (Task 9/6a's ws-hub mount work).
        redis: None,
    };

    AppState {
        poller: Arc::new(poller_shared),
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        // Populated for real once CORS lands (Task 7).
        cors_origins: Arc::new(Vec::new()),
        session_cookie_name: Arc::from(env_or("SESSION_COOKIE_NAME", "spx_session").as_str()),
        // Default true (production-safe); set `COOKIE_SECURE=false` only for
        // local dev where reactor-core is reached directly over plain HTTP
        // (no TLS-terminating edge proxy in front of it) — see `state.rs`'s
        // field doc comment.
        cookie_secure: env_or("COOKIE_SECURE", "true").parse().unwrap_or(true),
    }
}

fn app(state: AppState) -> Router {
    api_gateway::build_router(state)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("reactor-core starting");

    let state = build_state().await;

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081")
        .await
        .expect("bind 0.0.0.0:8081");

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl_c handler");
    tracing::info!("reactor-core shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let state = build_state().await;

        let response = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "api-gateway");
    }
}
