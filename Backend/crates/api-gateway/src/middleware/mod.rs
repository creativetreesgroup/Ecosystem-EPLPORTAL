// Backend/crates/api-gateway/src/middleware/mod.rs
//! Global HTTP middleware applied in `build_router` (Task 6+: security
//! headers here; CORS/rate-limit/body-limit land in later Task 7/8 modules).
pub mod security_headers;

pub use security_headers::security_headers;
