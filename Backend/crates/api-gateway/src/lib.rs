// Backend/crates/api-gateway/src/lib.rs
//! Fase 6 — api-gateway: the REST + WebSocket HTTP layer over Fases 1-5.
//! Session auth + centralized RBAC + security/CORS/rate-limit/body-limit
//! middleware. This sub-phase (6a) ships only the foundation: crate
//! scaffold, session/RBAC plumbing, login/me/logout, and the middleware
//! stack. Later sub-phases (6b-6e) add route modules here.
pub mod auth;
pub mod error;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/auth", routes::auth::auth_router(state.clone()))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}
