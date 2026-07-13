// Backend/crates/poller/src/schedule.rs
//! The per-account task loop. Single-flight is a STRUCTURAL guarantee: this is
//! the ONLY place `poll_once` is invoked for an account, and it is invoked
//! sequentially inside one task — two cycles for the same account can never
//! overlap (the property the reference had to defend with a `state.polling`
//! flag is free here). `poke.notify_one()` (from the notif watcher) cancels the
//! `sleep` via `select!` so a fresh ticket is picked up within ~1 notif RTT
//! (port of `pokePoll`'s "reschedule in 1ms", but as real cancellation).
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::state::{PollerShared, PollerState};

/// Spawn the account's poll loop. Returns the `JoinHandle`; the caller stores it
/// in `AccountHandle` alongside the same `poke` it passes here.
pub fn spawn_account_loop(
    shared: Arc<PollerShared>,
    mut st: PollerState,
    poke: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_millis(shared.config.poll_interval_ms);
        loop {
            // ONE cycle, awaited to completion before the next can begin.
            poll_once(&shared, &mut st).await;

            // Sleep for the interval, but wake EARLY if poked.
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = poke.notified() => {
                    tracing::trace!(account = %st.account_id, "poked → early wake");
                }
            }
        }
    })
}

/// One poll cycle. Task 1 ships the skeleton (bump `poll_count`); Tasks 2/5/6
/// fill in fetch → match → dispatch → anti-drift. Kept as a free `async fn` so
/// tests can substitute a probe (see the single-flight test, which drives the
/// loop through a trait-free indirection by calling `spawn_account_loop` with a
/// state whose `poll_count` it observes).
pub async fn poll_once(_shared: &PollerShared, st: &mut PollerState) {
    st.poll_count = st.poll_count.wrapping_add(1);
    // Later tasks replace this body. It MUST remain `&mut PollerState` and MUST
    // NOT spawn a second task per cycle (that would break single-flight).
}
