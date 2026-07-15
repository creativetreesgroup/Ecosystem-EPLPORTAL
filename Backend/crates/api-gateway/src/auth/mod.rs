// Backend/crates/api-gateway/src/auth/mod.rs
pub mod middleware;
pub mod permission;

pub use middleware::{session_auth, CurrentUser};
pub use permission::{require_permission, Permission};
