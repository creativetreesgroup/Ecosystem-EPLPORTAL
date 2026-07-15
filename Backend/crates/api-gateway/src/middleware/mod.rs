// Backend/crates/api-gateway/src/middleware/mod.rs
//! Global HTTP middleware applied in `build_router` (Task 6: security
//! headers; Task 7: CORS exact-match allowlist + the global 1.5MB body-limit
//! layer, the latter applied directly via `tower_http::limit::RequestBodyLimitLayer`
//! in `lib.rs` rather than re-exported from here since it needs no
//! project-specific wrapper. Rate-limiting lands in a later task).
pub mod cors;
pub mod security_headers;

pub use cors::cors_layer;
pub use security_headers::security_headers;
