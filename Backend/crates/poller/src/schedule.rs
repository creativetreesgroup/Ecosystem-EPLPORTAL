// Backend/crates/poller/src/schedule.rs
//! The per-account task loop. Single-flight is a STRUCTURAL guarantee: this is
//! the ONLY place `poll_once` is invoked for an account, and it is invoked
//! sequentially inside one task — two cycles for the same account can never
//! overlap (the property the reference had to defend with a `state.polling`
//! flag is free here). `poke.notify_one()` (from the notif watcher) cancels the
//! `sleep` via `select!` so a fresh ticket is picked up within ~1 notif RTT
//! (port of `pokePoll`'s "reschedule in 1ms", but as real cancellation) — AND
//! (Task 6) marks the upcoming cycle as `pool_changed=true`, so it forces a
//! full sweep rather than continuing the cheap rotating window. Without this,
//! the notif watcher's poke (Task 4) is pure motion: it wakes the loop early
//! but the next `poll_once` would still only look at whatever page the
//! rotating window happened to land on, and a genuinely new ticket could sit
//! unseen for up to `full_sync_every` cycles.
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use secrecy::ExposeSecret;

use crate::dispatch::dispatch_booking;
use crate::fetch::{fast_detect, should_full_sweep, sweep};
use crate::login::{auto_login, should_daily_relogin, should_reactive_relogin, wib_day};
use crate::state::{AccountHandle, PollerShared, PollerState};

/// Spawn the account's poll loop. Returns the `JoinHandle`; the caller stores it
/// in `AccountHandle` alongside the same `poke` it passes here.
///
/// NOTE: prefer `ensure_restored_then_spawn` over calling this directly — it
/// enforces the Layer-3 "restore before first poll" contract (Fase 4 CP-7).
/// This function is kept `pub` (not `pub(crate)`) only because the
/// single-flight tests exercise the loop SHAPE directly; production code
/// should not call it without having already awaited
/// `executor.restore_accepted_ids` for this account.
pub fn spawn_account_loop(
    shared: Arc<PollerShared>,
    mut st: PollerState,
    poke: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_millis(shared.config.poll_interval_ms);
        // Whether the NEXT cycle's wake-up was caused by a poke (vs. the plain
        // interval timer). The very first cycle has no prior wake to attribute,
        // so it starts `false` — a poke can only ever be observed AFTER at
        // least one `select!` has run.
        let mut woken_by_poke = false;
        loop {
            // ONE cycle, awaited to completion before the next can begin.
            poll_once(&shared, &mut st, woken_by_poke).await;
            woken_by_poke = false;

            // Sleep for the interval, but wake EARLY if poked. Capture WHICH
            // arm fired: only the poke arm feeds `pool_changed=true` into the
            // NEXT poll_once call (Task 4/6 hand-off — see module doc).
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = poke.notified() => {
                    woken_by_poke = true;
                    tracing::trace!(account = %st.account_id, "poked → early wake, next cycle forces a full sweep");
                }
            }
        }
    })
}

/// One poll cycle: fetch (fast-detect peek + full-sweep-or-rotating-window
/// sweep) → upsert every seen booking → dispatch every not-yet-known pending
/// one → anti-drift (no-ops unless the sweep was `fetch_complete`).
///
/// `woken_by_poke` is `true` exactly when THIS cycle's wake-up (the `select!`
/// in `spawn_account_loop`) was caused by the notif watcher's poke rather than
/// the plain interval timer — it is OR'd into the `pool_changed` signal fed to
/// `should_full_sweep`, so a poke (a real "something changed" signal from
/// Task 4's watcher) reliably forces a full sweep on the very next cycle
/// (DoD #4), not merely an early wake with no behavioral effect.
pub async fn poll_once(shared: &PollerShared, st: &mut PollerState, woken_by_poke: bool) {
    st.poll_count = st.poll_count.wrapping_add(1);

    // Fast-detect (opt-in) hints a pool change → jump straight to a full sweep.
    let fast = fast_detect(&shared.client, &st.cookies, &shared.config).await;
    let pool_changed = woken_by_poke || !fast.is_empty();
    let full = should_full_sweep(st.poll_count, shared.config.full_sync_every, pool_changed);

    let outcome = sweep(
        &shared.client,
        &st.cookies,
        &shared.config,
        st.poll_count,
        full,
    )
    .await;

    // Upsert every seen booking (enrichment-preserving) then dispatch pendings.
    for booking in &outcome.bookings {
        let _ = store::upsert_booking(
            &shared.pool,
            st.tenant_id,
            &store::BookingUpsert {
                spx_id: booking.id.clone(),
                status: "pending".into(),
                is_coc: matches!(booking.booking_type, core_domain::BookingType::Spxid),
                raw_data: booking.raw.clone(),
            },
        )
        .await;
        if st.dedup.is_known(&booking.id) {
            continue;
        }
        let _ = dispatch_booking(shared, st, booking).await;
    }

    // Anti-drift — the FetchOutcome type gate ensures this no-ops unless the
    // sweep was complete (Task 5).
    let _ = crate::antidrift::run_anti_drift(&shared.pool, st.tenant_id, &outcome).await;

    st.last_pending_count = outcome.spx_id_set.len() as i64;

    // Relogin check (Task 7b — wires Task 7's already-tested `login` module
    // into the live loop). Runs on the SAME task/cycle as the rest of
    // `poll_once`, not spawned: a relogin-in-progress naturally blocks this
    // account's NEXT accept dispatch, which is correct — dispatching accepts
    // with a session already known to be stale would just produce more `Auth`
    // outcomes.
    let now = chrono::Utc::now();
    let today_wib = wib_day(now);
    if should_reactive_relogin(st.consecutive_401s)
        || should_daily_relogin(&st.last_daily_relogin_day, &today_wib)
    {
        st.last_relogin_attempt_ms = now.timestamp_millis();
        match auto_login(
            &shared.sidecar,
            &shared.client,
            &st.account_id,
            st.username.expose_secret(),
            st.password.expose_secret(),
        )
        .await
        {
            Some((cookies, tier)) => {
                st.cookies = cookies;
                st.consecutive_401s = 0;
                st.last_daily_relogin_day = today_wib;
                tracing::info!(account = %st.account_id, tier = ?tier, "relogin succeeded");
            }
            None => {
                // Do NOT panic, do NOT stop the loop, do NOT touch
                // `st.cookies`/`st.consecutive_401s` — a failed relogin just
                // means the next cycle's accepts keep hitting Auth until a
                // future cycle's relogin succeeds.
                tracing::warn!(account = %st.account_id, "relogin attempt failed, will retry next cycle");
            }
        }
    }
}

/// CP-7 CONTRACT: await the Layer-3 durable restore to completion BEFORE the
/// first poll is ever scheduled. Fase 4 documented this as the poller's
/// responsibility; here it is enforced as a hard ordering (the loop cannot start
/// until restore returns) — this is the ONLY spawn path this crate exposes for
/// starting an account's loop in production (see `spawn_account_loop`'s doc).
pub async fn ensure_restored_then_spawn(
    shared: Arc<PollerShared>,
    st: PollerState,
) -> AccountHandle {
    // MUST complete before spawn — otherwise the first poll could re-accept a
    // ticket won in a previous process lifetime (Layer 1 starts empty; the
    // Layer 2 claim key may have expired).
    let _ = shared
        .executor
        .restore_accepted_ids(&st.account_id, &st.dedup)
        .await;
    let poke = Arc::new(Notify::new());
    let join = spawn_account_loop(shared, st, poke.clone());
    AccountHandle { poke, join }
}
