// Backend/crates/poller/src/lib.rs
//! Fase 5 — the poller: one Tokio task per SPX account (single-flight by
//! construction), fetch orchestration, notif watcher, anti-drift, the accept
//! decision pipeline, 3-tier auto-login, and a durable-primary watchdog.
//! Depends on Fase 3 `spx-client` (HTTP) and Fase 4 `executor` (dedup/quota) —
//! and, deliberately, on NO browser-automation crate (tier-1 login is HTTP to
//! `auth-sidecar`, so a Chromium crash can never take down this hot-path
//! process — design correction #2 / DoD #10).
pub mod schedule;
pub mod state;

pub use schedule::{poll_once, spawn_account_loop};
pub use state::{AccountHandle, PollerConfig, PollerShared, PollerState};

// Later tasks add: pub mod fetch; (Task 2) pub mod hedge; (Task 3)
// pub mod notif_watch; (Task 4) pub mod antidrift; (Task 5) pub mod dispatch;
// (Task 6) pub mod login; (Task 7) pub mod watchdog; (Task 8)
