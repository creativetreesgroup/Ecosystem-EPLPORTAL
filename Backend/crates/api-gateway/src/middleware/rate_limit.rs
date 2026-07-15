// Backend/crates/api-gateway/src/middleware/rate_limit.rs
//! Per-IP rate limiting via `tower_governor`, applied ONLY to
//! `POST /auth/portal-login` (see `routes/auth.rs::auth_router`'s
//! `.route_layer(...)`, scoped to just that one route — never global, never
//! `/me`/`/logout`). Login gets a stricter budget (~20/min/IP) than the
//! reference's undifferentiated public-GET limiter (120/min, which 6d's
//! `prices`/`branding` routes will use) — a login POST is a
//! credential-stuffing target, a different threat model than a public read.
//!
//! ## Verified against the ACTUALLY-resolved `tower_governor = 0.8.0`
//!
//! The task brief's snippet was flagged best-effort and IS wrong for this
//! resolved version in two ways, found by reading
//! `~/.cargo/registry/src/.../tower_governor-0.8.0/src/{governor,key_extractor,errors}.rs`
//! and its `README.md` directly (same discipline Task 7 used for
//! `tower_http`):
//!
//! 1. **`GovernorLayer` takes an `Arc`, not a `'static` reference.** The
//!    brief's `Box::leak` idiom is for an older API shape; 0.8.0's
//!    `GovernorLayer::new(config: impl Into<Arc<GovernorConfig<K, M>>>)`
//!    accepts an owned `GovernorConfig` directly (`Arc<T>: From<T>` covers
//!    the `Into` bound) — no leaking needed.
//! 2. **`GovernorConfigBuilder::finish()` returns `Option`, not `Result`.**
//!    The brief's `.expect("valid governor config")` call happens to compile
//!    either way (`Option` also has `.expect`), but the reason it can return
//!    `None` is specifically "`burst_size` or `period` is zero" — never true
//!    for the fixed literals below, so the `.expect` here can never actually
//!    panic in practice.
//!
//! ## Key extractor: `SmartIpKeyExtractor`, not the brief's `PeerIpKeyExtractor`
//!
//! This is the one place this task deviates from the brief's own sketch on
//! *behavior*, not just API shape, and it matters for correctness:
//!
//! `PeerIpKeyExtractor` (the brief's choice, and `tower_governor`'s own
//! default) keys on the raw TCP peer address. `state.rs`'s `cookie_secure`
//! field doc comment already establishes this deployment's architecture: a
//! TLS-terminating edge proxy (Caddy/Traefik) sits in front of `reactor-core`
//! in every real deployment. Behind such a proxy the TCP peer is ALWAYS the
//! proxy's own address — `PeerIpKeyExtractor`'s own doc comment calls this
//! out explicitly as a misuse ("rate limiting will be applied to all
//! incoming requests as if they were from the same user"). Using it here
//! would turn a "20/min per attacking IP" defense into "20/min for the
//! entire deployment, shared by every legitimate user" — worse than no
//! rate-limiting design at all for a multi-tenant portal.
//!
//! `SmartIpKeyExtractor` reads `X-Forwarded-For` / `X-Real-Ip` / `Forwarded`
//! (the headers a correctly-configured reverse proxy sets to the REAL client
//! IP) first, falling back to the TCP peer address
//! (`ConnectInfo<SocketAddr>`, then a raw `SocketAddr` extension) only when
//! none of those headers are present — exactly the "sane default for an app
//! running behind a reverse proxy" its own doc comment describes.
//!
//! ## The `ConnectInfo<SocketAddr>` fallback requires opt-in wiring
//!
//! Both key extractors' peer-IP fallback need `axum::extract::ConnectInfo`
//! in the request extensions, which is NOT automatic: `axum::serve(listener,
//! router)` (what this crate's existing tests and `reactor-core`'s `main.rs`
//! both used, pre-Task-8) does NOT populate it — only
//! `.into_make_service_with_connect_info::<SocketAddr>()` does (confirmed by
//! reading `axum-0.8.9`'s `routing/mod.rs`, AND the `tower_governor` 0.8.0
//! README's own "Common pitfalls" section #2, which names this exact
//! mistake). Without it, EVERY request to a peer-IP-keyed route fails key
//! extraction and 500s — silently taking down the very login route this task
//! is supposed to protect, not just rate-limit it. `reactor-core/src/main.rs`
//! and `tests/auth_routes.rs`'s `spawn_server` (the only other place besides
//! this crate's own new `tests/rate_limit.rs` that exercises
//! `POST /auth/portal-login` end-to-end) were both updated in this same task
//! to wire `.into_make_service_with_connect_info::<SocketAddr>()` so the
//! fallback path actually works, in addition to the `X-Forwarded-For` path a
//! real deployment's edge proxy provides.
use governor::middleware::NoOpMiddleware;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::SmartIpKeyExtractor;
use tower_governor::GovernorLayer;

/// ~20 requests/minute/IP for login attempts: a burst of up to 20 requests
/// immediately, replenishing one element every 3 seconds thereafter (20
/// elements/60s steady-state) — a deliberately stricter budget than the
/// reference's undifferentiated 120/min public-GET limiter (6d's
/// `prices`/`branding` routes), since a login POST is a credential-stuffing
/// target, not a public read. The exact burst/period split is an
/// implementation detail tuned against `tower_governor`'s real
/// token-bucket semantics (see its README's "How does it work?" section),
/// not a value guessed blindly.
const LOGIN_BURST_SIZE: u32 = 20;
const LOGIN_REPLENISH_PERIOD_SECS: u64 = 3;

/// Builds the route-scoped rate-limit layer for `POST /auth/portal-login`.
/// Applied via `.route_layer(...)` in `routes/auth.rs::auth_router`, scoped
/// to JUST that route — never mounted globally, and never applied to
/// `/me`/`/logout` (session-authenticated traffic doesn't need this budget).
///
/// On exhaustion, `tower_governor`'s built-in
/// `From<GovernorError> for Response<axum::body::Body>` (the "axum" feature)
/// produces a `429 Too Many Requests` with `Retry-After`/`X-RateLimit-After`
/// headers already set — no custom `GovernorLayer::error_handler` needed for
/// this task's requirements.
pub fn login_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_second(LOGIN_REPLENISH_PERIOD_SECS)
        .burst_size(LOGIN_BURST_SIZE)
        .finish()
        // Only `None` when burst_size or period is zero — both are
        // non-zero `const`s above, so this can never actually panic.
        .expect("LOGIN_BURST_SIZE and LOGIN_REPLENISH_PERIOD_SECS are both non-zero");
    GovernorLayer::new(config)
}
