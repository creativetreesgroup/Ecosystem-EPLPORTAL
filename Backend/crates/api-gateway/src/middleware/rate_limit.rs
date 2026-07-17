// Backend/crates/api-gateway/src/middleware/rate_limit.rs
//! Per-IP rate limiting via `tower_governor`, applied route-scoped only —
//! never globally in `build_router`. `login_rate_limit_layer` gates ONLY
//! `POST /auth/portal-login` (see `routes/auth.rs::auth_router`'s
//! `.route_layer(...)`, scoped to just that one route — never global, never
//! `/me`/`/logout`) with a stricter budget (~20/min/IP) than
//! `public_rate_limit_layer`'s undifferentiated public-GET budget (120/min,
//! used by 6d's `routes/prices.rs::prices_router`'s public half, and Task 8's
//! `GET /branding` + friends) — a login POST is a credential-stuffing target,
//! a different threat model than a public read.
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
//! ### Trust invariant this depends on (review finding, addressed)
//!
//! `SmartIpKeyExtractor` takes the **leftmost** parseable IP out of
//! `X-Forwarded-For`, with **zero validation** that the request actually came
//! through a trusted proxy hop — this is documented, standard behavior for
//! this class of extractor (the crate's own docs: "Only use if you can
//! ensure these headers are set by a trusted provider"). That means this is
//! safe **ONLY** because the edge proxy in front of `reactor-core` — Caddy
//! locally, Traefik on the Fase 8 VPS overlay, per `Docker/Caddyfile` — is
//! configured to **OVERWRITE** `X-Forwarded-For` with the real observed peer
//! address, never append a client-supplied value onto it. `Docker/Caddyfile`
//! enforces this today via `header_up X-Forwarded-For {remote_host}` on both
//! `reverse_proxy` blocks (the security-critical one being the
//! `tower-reactor-core` block) — see that file for the actual directive and
//! its own comment explaining why.
//!
//! If `reactor-core` is ever exposed without going through a proxy that
//! enforces this invariant (e.g. a bare `reverse_proxy` with no `header_up`
//! override, or a Traefik config using `forwardedHeaders.insecure` instead of
//! a correctly-scoped `forwardedHeaders.trustedIPs`), `SmartIpKeyExtractor`
//! becomes bypassable: an attacker sends a spoofed `X-Forwarded-For` and
//! rotates its value on every request, and each rotation buys a fresh
//! rate-limit budget — unlimited login attempts, defeating this entire task.
//! Before relying on this extractor in any new deployment topology, verify
//! the fronting proxy still overwrites (not appends to) this header.
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

/// ~120 requests/minute/IP for public reads (`GET /prices`, and Task 8's `GET /branding` +
/// friends) — the reference's own "undifferentiated public-GET limiter" figure, matching the
/// design doc's binding constant. A burst of 120 immediately, replenishing one element every
/// 500ms thereafter (120 elements/60s steady-state) — the SAME `SmartIpKeyExtractor` as
/// `login_rate_limit_layer` (see that fn's own doc comment for the X-Forwarded-For trust
/// invariant this depends on; identical here, not re-derived).
///
/// `per_millisecond` verified to exist on `GovernorConfigBuilder` in the resolved
/// `tower_governor = 0.8.0` (`~/.cargo/registry/src/.../tower_governor-0.8.0/src/governor.rs`,
/// alongside `per_second`/`per_nanosecond`/their `const_*` counterparts) — no substitution
/// needed, unlike the brief's own flagged uncertainty.
const PUBLIC_BURST_SIZE: u32 = 120;
const PUBLIC_REPLENISH_PERIOD_MS: u64 = 500;

/// Builds the route-scoped rate-limit layer for public GET routes. Applied via `.route_layer(...)`
/// on the specific public sub-router (e.g. `routes/prices.rs::prices_router`'s public half) —
/// never mounted globally, never applied to session-authenticated traffic.
pub fn public_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(PUBLIC_REPLENISH_PERIOD_MS)
        .burst_size(PUBLIC_BURST_SIZE)
        .finish()
        // Only `None` when burst_size or period is zero — both are
        // non-zero `const`s above, so this can never actually panic.
        .expect("PUBLIC_BURST_SIZE and PUBLIC_REPLENISH_PERIOD_MS are both non-zero");
    GovernorLayer::new(config)
}

/// ~60 requests/minute/IP for quick-accept PAGE views (`GET /q/{token}`, `GET /accept/{code}`) —
/// matches the reference's own `rlView` budget exactly (60/60s). A lenient budget since this is a
/// page render (WhatsApp link previews/repeat taps), not a state-changing action — deliberately
/// looser than `quick_accept_action_rate_limit_layer` below. A burst of 60 immediately,
/// replenishing one element every 1000ms thereafter (60 elements/60s steady-state). Same
/// `SmartIpKeyExtractor` as every other layer in this file — see `login_rate_limit_layer`'s doc
/// comment for the X-Forwarded-For trust invariant this depends on; identical here, not
/// re-derived.
const QUICK_ACCEPT_VIEW_BURST_SIZE: u32 = 60;
const QUICK_ACCEPT_VIEW_REPLENISH_PERIOD_MS: u64 = 1000;

/// Builds the route-scoped rate-limit layer for the quick-accept PAGE-view routes. Applied via
/// `.route_layer(...)` on the GET-only half of `routes/quick_accept.rs::hmac_router` /
/// `short_code_router` — never mounted globally, never applied to the stricter POST/action half
/// (see `quick_accept_action_rate_limit_layer`).
pub fn quick_accept_view_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(QUICK_ACCEPT_VIEW_REPLENISH_PERIOD_MS)
        .burst_size(QUICK_ACCEPT_VIEW_BURST_SIZE)
        .finish()
        // Only `None` when burst_size or period is zero — both are
        // non-zero `const`s above, so this can never actually panic.
        .expect("QUICK_ACCEPT_VIEW_BURST_SIZE and QUICK_ACCEPT_VIEW_REPLENISH_PERIOD_MS are both non-zero");
    GovernorLayer::new(config)
}

/// ~12 requests/minute/IP for quick-accept ACTIONS (`POST /q/accept`, `POST /accept/{code}`) —
/// matches the reference's own `rlAccept` budget exactly (12/60s). A stricter budget than
/// `quick_accept_view_rate_limit_layer` above since this fires a real accept attempt against SPX
/// (a state-changing, external side effect — anti-brute-force on the token/code space AND
/// anti-DoS-against-SPX, not just anti-scraping). A burst of 12 immediately, replenishing one
/// element every 5000ms thereafter (12 elements/60s steady-state). Same `SmartIpKeyExtractor` as
/// every other layer in this file.
const QUICK_ACCEPT_ACTION_BURST_SIZE: u32 = 12;
const QUICK_ACCEPT_ACTION_REPLENISH_PERIOD_MS: u64 = 5000;

/// Builds the route-scoped rate-limit layer for the quick-accept ACTION routes. Applied via
/// `.route_layer(...)` on the POST-only half of `routes/quick_accept.rs::hmac_router` /
/// `short_code_router`. `GET /{code}` and `POST /{code}` share the same path in
/// `short_code_router` — `axum::Router::route_layer` wraps every route registered so far on the
/// `Router` value it's called on (verified against `axum 0.8.9`'s
/// `path_router.rs::PathRouter::route_layer`, which maps `.layer(...)` over ALL entries in
/// `self.routes`), with no per-HTTP-method scoping within a single path entry — a `MethodRouter`
/// combining `get(...).post(...)` at one path is ONE entry, so a single `.route_layer(...)` call
/// on it always wraps both methods identically. Method-scoped budgets therefore require two
/// separate single-method `Router::new()` values (one `.route_layer`'d with this fn, one with
/// `quick_accept_view_rate_limit_layer`), `.merge()`d together — the exact shape
/// `routes/prices.rs::prices_router` already established for "two different rate limits sharing a
/// mount point" (there via two different PATHS at the top level; here via two different METHODS
/// on the same path, which merges just as cleanly since axum only rejects merging the SAME method
/// at the same path twice, not different methods).
pub fn quick_accept_action_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(QUICK_ACCEPT_ACTION_REPLENISH_PERIOD_MS)
        .burst_size(QUICK_ACCEPT_ACTION_BURST_SIZE)
        .finish()
        // Only `None` when burst_size or period is zero — both are
        // non-zero `const`s above, so this can never actually panic.
        .expect("QUICK_ACCEPT_ACTION_BURST_SIZE and QUICK_ACCEPT_ACTION_REPLENISH_PERIOD_MS are both non-zero");
    GovernorLayer::new(config)
}
