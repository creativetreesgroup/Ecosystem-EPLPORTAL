// Backend/crates/api-gateway/src/middleware/mod.rs
//! Global + route-scoped HTTP middleware. `build_router` (`lib.rs`) applies
//! `security_headers` and `cors_layer` globally, plus
//! `tower_http::limit::RequestBodyLimitLayer` directly (Task 6: security
//! headers; Task 7: CORS exact-match allowlist + the global 1.5MB body-limit,
//! the latter needing no project-specific wrapper so it isn't re-exported
//! from here). `rate_limit::login_rate_limit_layer` (Task 8) is NOT applied
//! in `build_router` — it's route-scoped to just `POST /auth/portal-login`
//! via `.route_layer(...)` in `routes/auth.rs::auth_router`, since a
//! login-attempt budget would be wrong for `/me`/`/logout` or any other
//! route. `rate_limit::public_rate_limit_layer` (Fase 6d Task 4) is likewise
//! NOT global — it's route-scoped to just `GET /prices`'s public half via
//! `routes/prices.rs::prices_router`'s own `.route_layer(...)`.
pub mod cors;
pub mod rate_limit;
pub mod security_headers;

pub use cors::cors_layer;
pub use rate_limit::{login_rate_limit_layer, public_rate_limit_layer};
pub use security_headers::security_headers;
