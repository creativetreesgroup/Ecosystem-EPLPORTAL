// Backend/crates/api-gateway/src/lib.rs
//! Fase 6 — api-gateway: the REST + WebSocket HTTP layer over Fases 1-5.
//! Session auth + centralized RBAC + security/CORS/rate-limit/body-limit
//! middleware. This sub-phase (6a) ships only the foundation: crate
//! scaffold, session/RBAC plumbing, login/me/logout, and the middleware
//! stack. Later sub-phases (6b-6e) add route modules here.
pub mod auth;
pub mod branding;
pub mod error;
pub mod middleware;
pub mod otp;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use tower_http::limit::RequestBodyLimitLayer;

/// Global request body-limit: 1.5MB, matching the reference's default. `/branding`'s 15MB
/// carve-out (Task 8) is NOT an additive inner layer on top of this — verified against
/// `tower-http 0.7.0`'s actual `RequestBodyLimit::call` source: an outer/global layer
/// short-circuits on `Content-Length` before routing runs, and nested `Limited` body wrapping
/// always enforces the SMALLEST cap regardless of layering order, so a naive "bigger inner
/// layer" would still be capped at 1.5MB. The actual fix: `branding` is built as its OWN
/// `Router`, with its OWN `RequestBodyLimitLayer`, `.merge()`d into `rest` AFTER `rest` already
/// has its OWN separate 1.5MB layer applied — two independently-layered route trees, not one
/// router with two competing layers. `cors_layer`/`security_headers` don't need to differ
/// per-route, so they stay wrapping the FINAL merged whole, same as before this task.
const GLOBAL_BODY_LIMIT_BYTES: usize = 1_500_000;
const BRANDING_BODY_LIMIT_BYTES: usize = 15_000_000;

pub fn build_router(state: AppState) -> Router {
    let rest = Router::new()
        .route("/healthz", get(healthz))
        .nest("/auth", routes::auth::auth_router(state.clone()))
        // Mounted directly at `/auth` (not a further sub-path): this
        // router's own routes are already named in full
        // (`/request-aa-otp`, `/verify-aa-otp`), matching the reference's
        // `/auth/request-aa-otp` / `/auth/verify-aa-otp` paths.
        .nest("/auth", routes::otp::otp_router(state.clone()))
        .nest(
            "/auth/spx-credentials",
            routes::spx_credentials::spx_credentials_router(state.clone()),
        )
        .nest(
            "/auth/spx-login",
            routes::spx_login::spx_login_router(state.clone()),
        )
        .nest(
            "/auth/portal-users",
            routes::portal_users::portal_users_router(state.clone()),
        )
        .nest(
            "/bookings",
            routes::bookings::bookings_router(state.clone())
                .merge(routes::rules::rules_router(state.clone())),
        )
        .nest("/prices", routes::prices::prices_router(state.clone()))
        .nest("/locations", routes::locations::locations_router(state.clone()))
        .nest("/bot", routes::bot::bot_router(state.clone()))
        // `/q/:token` (HMAC quick-accept) + `POST /q/accept` — deliberately mounted OUTSIDE
        // every `session_auth`-gated nest above (the token itself is the authorization; there is
        // no session cookie on this route at all). Mounted here now, in Task 4, rather than left
        // for a later task as `quick_accept.rs`'s own module doc originally sketched: this task's
        // own test suite (`tests/quick_accept_routes.rs`) drives it through this exact
        // `build_router`, so it has to be reachable for those tests to mean anything. No
        // dedicated rate-limit layer added on this nest — that's a disclosed follow-up hardening
        // item, not silently dropped scope.
        .nest("/q", routes::quick_accept::hmac_router(state.clone()))
        // `/accept/:code` (Redis short-code quick-accept, Task 5) — same reasoning as `/q` right
        // above: also outside every `session_auth` nest (the short code itself is the
        // authorization, resolved via Redis instead of an HMAC token), and mounted here now
        // rather than deferred, because this task's own test suite drives it through this exact
        // `build_router`. Same disclosed follow-up gap as `/q`: no dedicated rate-limit layer yet.
        .nest("/accept", routes::quick_accept::short_code_router(state.clone()))
        .with_state(state.clone())
        .layer(RequestBodyLimitLayer::new(GLOBAL_BODY_LIMIT_BYTES));

    let branding = Router::new()
        .nest("/branding", routes::branding::branding_router(state.clone()))
        .with_state(state.clone())
        // `DefaultBodyLimit::disable()` (NOT in the brief's Step 6 snippet — added here after
        // that snippet's own `RequestBodyLimitLayer::new(BRANDING_BODY_LIMIT_BYTES)` alone was
        // empirically found insufficient): axum-core's `Bytes`/`Json` extractors enforce their
        // OWN hardcoded 2MB default (`with_limited_body`'s `DEFAULT_LIMIT = 2_097_152`,
        // `axum-core 0.5.6/src/ext_traits/request.rs`) independently of ANY `tower_http`
        // body-limit layer — an entirely separate mechanism from the tower-http
        // smallest-cap-wins bug this task's own risk note is about, verified by this task's own
        // route test (`put_branding_accepts_a_4mb_body_but_prices_still_rejects_it`) still 413'ing
        // a ~4MB PUT even after Step 6's exact two-independently-layered-trees restructuring was
        // applied verbatim. `axum_core::extract::default_body_limit`'s own rustdoc example pairs
        // these two layers for exactly this scenario ("accept bodies larger than the default
        // limit of 2MB using Bytes or an extractor built on it such as ... Json"), so this is
        // axum's documented complementary API, not an alternative tower-http layering strategy —
        // `PUT /branding`'s `Json<BrandingInput>` extractor is the only consumer in this sub-router
        // that would otherwise be silently re-capped at 2MB regardless of the 15MB layer below.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(BRANDING_BODY_LIMIT_BYTES));

    rest.merge(branding)
        // CORS (Task 7): exact-match allowlist, applied to every route this
        // router produces. Innermost of the two global layers below —
        // position relative to the security-headers layer doesn't matter
        // for its own behavior (it only reads the `Origin` request header
        // and adds response headers), so it's placed closest to the router
        // for readability.
        .layer(middleware::cors_layer(&state.cors_origins))
        // Outermost layer: runs on EVERY response this router produces,
        // including `ApiError`-derived error responses (401/404/etc — see
        // `error.rs`), a body-limit `413` from EITHER of the two merged
        // sub-routers' own layers above, and `/healthz`, since it wraps the
        // whole merged `Router` rather than being scoped to any one route or
        // nested sub-router.
        .layer(axum::middleware::from_fn(middleware::security_headers))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}
