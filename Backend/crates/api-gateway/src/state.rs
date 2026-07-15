// Backend/crates/api-gateway/src/state.rs
//! Shared HTTP-layer context. Wraps `poller::PollerShared` (the SAME
//! executor/client/pool/etc. account tasks use) rather than duplicating its
//! fields — `AppState` adds only what the HTTP layer needs on top: the
//! ws-hub registry and the resolved single deployment tenant (see the design
//! doc's tenant-resolution addendum — no per-request tenant resolution).
use std::sync::Arc;

use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub poller: Arc<poller::PollerShared>,
    pub ws_hub: Arc<ws_hub::Hub>,
    pub tenant_id: Uuid,
    /// Exact-match CORS allowlist (Task 7) — `Arc` so cloning `AppState` per
    /// request stays cheap. Raw origin strings as configured (e.g. via
    /// `reactor-core`'s `CORS_ALLOWED_ORIGINS` env var, comma-separated);
    /// parsing into `http::HeaderValue`s (dropping + `tracing::warn!`ing any
    /// entry that fails to parse, rather than panicking) happens in
    /// `middleware::cors_layer` at `build_router` time, not here.
    pub cors_origins: Arc<Vec<String>>,
    /// Session cookie name, configurable so a later fase/deployment can
    /// rename it without touching handler code.
    pub session_cookie_name: Arc<str>,
    /// Whether the session cookie is issued with the `Secure` attribute
    /// (Task 5). Defaults to `true` in every real deployment binary
    /// (`reactor-core`'s `build_state`, via `COOKIE_SECURE`) — browsers
    /// refuse to send a `Secure` cookie back over plain HTTP, so this only
    /// needs to be `false` for local dev setups that reach `reactor-core`
    /// directly over HTTP instead of through the TLS-terminating edge proxy
    /// (Caddy/Traefik) the master spec's architecture assumes in
    /// production.
    pub cookie_secure: bool,
}
