// Backend/crates/ws-hub/src/lib.rs
//! Fase 5 — ws-hub: an axum WebSocket server with a per-session + per-account
//! local registry, a 30s ping, and (Task 13) a Redis pub/sub bridge for
//! cross-process broadcast. Uses ONLY axum's `ws` feature — no second WS crate.
pub mod bridge;
pub mod events;
pub mod hub;

pub use bridge::spawn_bridge;
pub use events::WsEvent;
pub use hub::{
    ws_handler, ws_handler_with_auth, ws_router, ws_router_with_auth, Hub, SessionValidator,
    WsQuery,
};
