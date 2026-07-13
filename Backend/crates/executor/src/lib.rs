//! Fase 4 — the executor library: 3-layer accept dedup, agency-dup verification,
//! and per-rule quota consumption. A pure library called by Fase 5 (poller) and
//! Fase 6 (api-gateway, manual accept); it owns the shared Redis keyspace so the
//! two callers cannot diverge.
pub mod account_lock;
pub mod dedup;
pub mod gate;
pub mod quota;
pub mod restore;

pub use dedup::AccountDedupState;
pub use gate::{
    ClaimOutcome, ExecutorError, ExecutorHandle, ManualClaimOutcome, RedisPool, ACCEPT_GATE_LUA,
};

// Later tasks add:
// pub mod agency_dup; (Task 6)
