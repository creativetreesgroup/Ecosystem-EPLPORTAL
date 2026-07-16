// Backend/crates/api-gateway/src/lib.rs
//! Fase 6 — api-gateway: the REST + WebSocket HTTP layer over Fases 1-5.
//! Session auth + centralized RBAC + security/CORS/rate-limit/body-limit
//! middleware. This sub-phase (6a) ships only the foundation: crate
//! scaffold, session/RBAC plumbing, login/me/logout, and the middleware
//! stack. Later sub-phases (6b-6e) add route modules here.
pub mod auth;
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

/// Global request body-limit (Task 7): 1.5MB, matching the reference's
/// default. The reference's 15MB branding carve-out is Task 8 of the Fase 6d
/// plan — that route doesn't exist yet, so no per-route override
/// infrastructure is built here for it.
const GLOBAL_BODY_LIMIT_BYTES: usize = 1_500_000;

pub fn build_router(state: AppState) -> Router {
    Router::new()
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
        .with_state(state.clone())
        // CORS (Task 7): exact-match allowlist, applied to every route this
        // router produces. Innermost of the three global layers below —
        // position relative to the body-limit/security-headers layers
        // doesn't matter for its own behavior (it only reads the `Origin`
        // request header and adds response headers), so it's placed closest
        // to the router for readability.
        .layer(middleware::cors_layer(&state.cors_origins))
        // Body-limit (Task 7): rejects an over-sized request with a
        // `413 Payload Too Large` BEFORE it ever reaches routing/handlers —
        // see `tower_http::limit`'s doc comment on how this is enforced
        // immediately from `Content-Length` when present.
        .layer(RequestBodyLimitLayer::new(GLOBAL_BODY_LIMIT_BYTES))
        // Outermost layer: runs on EVERY response this router produces,
        // including `ApiError`-derived error responses (401/404/etc — see
        // `error.rs`), a body-limit `413`, and `/healthz`, since it wraps the
        // whole `Router` rather than being scoped to any one route or nested
        // sub-router.
        .layer(axum::middleware::from_fn(middleware::security_headers))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}
