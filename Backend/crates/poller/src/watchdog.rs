// Backend/crates/poller/src/watchdog.rs
//! Durable-primary watchdog: ONE global 60s task that recreates the primary
//! account's poller if it has died, and writes an (as-yet-unconsumed)
//! heartbeat key for Fase-8 observability. Guards ONLY
//! `PollerConfig::primary_account_id` (the `PORTAL_USERNAME` analog) — a
//! faithful port of the reference's `ensureDurablePollerAlive`, NOT a
//! health-check of every account (design note / correction #4).
use std::sync::Arc;
use std::time::Duration;

use executor::ExecutorHandle;
use tokio::task::JoinHandle;

use crate::state::PollerShared;

const WATCHDOG_INTERVAL: Duration = Duration::from_secs(60);
/// TTL on the heartbeat key: a bit more than 2x the interval, so a single
/// missed write (transient Redis blip) doesn't make the key vanish before the
/// NEXT cycle's write refreshes it.
const HEARTBEAT_TTL_SECS: u64 = 120;

/// Best-effort heartbeat — `SET spx:poller_heartbeat:<acct> <now_ms> EX 120`.
/// No consumer is built now (Fase 8 observability, YAGNI / correction #4):
/// this key is written purely for a future dashboard/alert to read; nothing
/// in THIS repo ever reads it back, matching the reference's own aspirational
/// (never-consumed-there-either) heartbeat write.
pub async fn heartbeat(executor: &ExecutorHandle, account_id: &str) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let key = format!("spx:poller_heartbeat:{account_id}");
    executor
        .heartbeat_set(&key, now_ms, HEARTBEAT_TTL_SECS)
        .await;
}

/// Spawn the global watchdog: ONE task (not per-account) on a 60s interval.
/// Every cycle: write the primary's heartbeat, then check whether it has a
/// live `AccountHandle` in `shared.accounts` — missing, OR present but its
/// `JoinHandle::is_finished()` (the task panicked or returned) — and if not
/// alive, call `respawn(primary_id)` to recreate it.
///
/// `respawn` is injected (not called directly as
/// `ensure_restored_then_spawn`) because the watchdog does not itself own
/// account bootstrap — building a fresh `PollerState` needs credentials,
/// tenant/agency ids, and compiled rules, none of which this module has any
/// business constructing. The mount layer wires `respawn` to
/// `ensure_restored_then_spawn` (via a thin `PollerState`-building closure),
/// so a watchdog-triggered respawn still honors the Layer-3
/// restore-before-first-poll contract (Task 6/CP-7) exactly like any other
/// account start.
pub fn spawn_watchdog(
    shared: Arc<PollerShared>,
    respawn: Arc<dyn Fn(String) + Send + Sync>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let primary = shared.config.primary_account_id.clone();
        if primary.is_empty() {
            return; // no durable-primary configured — nothing to guard
        }
        let mut ticker = tokio::time::interval(WATCHDOG_INTERVAL);
        loop {
            ticker.tick().await;
            heartbeat(&shared.executor, &primary).await;

            let alive = shared
                .accounts
                .get(&primary)
                .map(|h| !h.join.is_finished())
                .unwrap_or(false);
            if !alive {
                tracing::warn!(
                    account = %primary,
                    "durable-primary poller missing or dead -> recreating"
                );
                (respawn)(primary.clone());
            }
        }
    })
}
