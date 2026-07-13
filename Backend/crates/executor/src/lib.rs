//! Fase 4 — the executor library: 3-layer accept dedup, agency-dup verification,
//! and per-rule quota consumption. A pure library called by Fase 5 (poller) and
//! Fase 6 (api-gateway, manual accept); it owns the shared Redis keyspace so the
//! two callers cannot diverge.
pub mod gate;

pub use gate::{ExecutorError, ExecutorHandle, RedisPool, ACCEPT_GATE_LUA};

// Later tasks add: pub mod dedup; (Task 2) pub mod restore; (Task 4)
// pub mod account_lock; pub mod quota; (Task 5) pub mod agency_dup; (Task 6)
