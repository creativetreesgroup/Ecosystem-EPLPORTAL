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
    /// Envelope-encryption master key (`spx_client::crypto::envelope`, Fase
    /// 3) — Fase 6b's `agency_credentials` CRUD routes need this to
    /// encrypt/decrypt `ciphertext`/`nonce` on every read/write, same key
    /// `reactor-core`'s account-bootstrap loop already loads once at boot
    /// (see `main.rs`'s `build_state`) to decrypt every row up front. `Arc`
    /// so cloning `AppState` per request stays cheap and so the SAME loaded
    /// key is shared between the bootstrap loop and every handler — this
    /// crate must never load the key file a second time.
    pub master_key: Arc<spx_client::crypto::envelope::MasterKey>,
    /// Redis connection for Fase 6b's OTP gate (`POST /auth/request-aa-otp` /
    /// `POST /auth/verify-aa-otp`) — generate/store/verify/rate-limit state,
    /// keyed per the design doc's OTP Redis key convention. `ConnectionManager`
    /// (not a bare `redis::aio::Connection`) so it transparently reconnects
    /// across a transient Redis blip instead of every handler needing its
    /// own retry logic — same rationale `executor::ExecutorHandle`/
    /// `poller::RedisPublisher` already established elsewhere in this
    /// workspace for their own Redis connections. Distinct from
    /// `poller::PollerShared`'s own `redis: Option<RedisPublisher>` field
    /// (ws `ticket_accepted` pub/sub) — this one is the HTTP layer's own
    /// connection for OTP state, a different concern with a different
    /// (harder) availability requirement: see `reactor-core`'s
    /// `build_state()` for why this field is NOT optional at boot.
    pub redis: redis::aio::ConnectionManager,
}
