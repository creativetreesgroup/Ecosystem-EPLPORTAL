// Backend/crates/api-gateway/src/auth/mod.rs
pub mod middleware;

pub use middleware::{session_auth, CurrentUser};
