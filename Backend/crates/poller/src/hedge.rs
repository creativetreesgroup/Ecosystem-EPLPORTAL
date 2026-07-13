// Backend/crates/poller/src/hedge.rs
//! Opt-in hedged single-page fetch (default OFF — correction #1). A parallel
//! full sweep waits on its SLOWEST page; if a page lags past `hedge_ms` we fire
//! ONE backup and take the first to answer. Bounded extra QPS (≤1 backup/page).
//! Gated to the FORCED FULL-SWEEP path only (never the steady-state rotating
//! window) — the caller (`fetch.rs::sweep`) is what decides `hedge_ms`, this
//! module just implements the race.
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use spx_client::{SpxBooking, SpxClient, SpxCookies};

static HEDGE_FIRES: AtomicU64 = AtomicU64::new(0);

/// Read-and-reset the hedge-fire counter (port of `takeHedgeFires`). 0 across a
/// slow whole-server sweep = unfixable client-side; >0 shrinking the tail =
/// hedging earning its keep.
pub fn hedge_fires_since_reset() -> u64 {
    HEDGE_FIRES.swap(0, Ordering::Relaxed)
}

/// Race a fallible async operation against an optional hedge timer.
///
/// `hedge_ms == 0` is a single shot: no timer is ever armed, `op` runs exactly
/// once, zero extra QPS — bit-for-bit the same code path as calling `op()`
/// directly (this IS the default-off guarantee).
///
/// `hedge_ms > 0`: fire `op()` (the primary). If it hasn't resolved within
/// `hedge_ms`, record ONE hedge fire and fire a second, independent copy of
/// `op()` (the backup); race primary vs. backup and return whichever settles
/// first. The loser is a function-local pinned future that is simply dropped
/// when this function returns — Tokio's `select!` never polls it again, so
/// its result (success or failure) is discarded, not double-counted.
///
/// `op` is called by-reference (`Fn`, not `FnOnce`) since it may run twice.
async fn hedge_race<T, F, Fut>(hedge_ms: u64, op: F) -> Result<T, ()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, ()>>,
{
    if hedge_ms == 0 {
        // OFF: identical to a plain call — no timer registered, no backup,
        // HEDGE_FIRES never touched.
        return op().await;
    }

    let primary = op();
    tokio::pin!(primary);

    tokio::select! {
        r = &mut primary => r,
        _ = tokio::time::sleep(Duration::from_millis(hedge_ms)) => {
            // Primary is slow -> fire exactly ONE backup and take the first
            // of {primary, backup} to answer.
            HEDGE_FIRES.fetch_add(1, Ordering::Relaxed);
            let backup = op();
            tokio::pin!(backup);
            tokio::select! {
                r = &mut primary => r,
                r = &mut backup  => r,
            }
        }
    }
}

/// One page, optionally hedged. `hedge_ms==0` → single shot (original
/// behavior, zero extra QPS); `hedge_ms>0` → primary + at-most-one backup,
/// first response wins, the other is dropped/cancelled.
pub async fn hedged_page(
    client: &SpxClient,
    cookies: &SpxCookies,
    pageno: u32,
    count: u32,
    hedge_ms: u64,
) -> Result<Vec<SpxBooking>, ()> {
    hedge_race(hedge_ms, move || async move {
        client
            .fetch_bookings(cookies, pageno, count)
            .await
            .map_err(|_| ())
    })
    .await
}

#[cfg(test)]
mod race_tests {
    //! Unit-level proof of the race primitive itself, using PURE virtual time
    //! (no sockets — `op` below sleeps via `tokio::time::sleep`, which paused
    //! time controls exactly). This is deliberately separate from
    //! `tests/hedge.rs`'s wiremock-based end-to-end tests: wiremock drives a
    //! real loopback socket, so `tokio::time::pause` cannot govern *its*
    //! delay. Here there is no real I/O at all, so paused time deterministically
    //! proves the delay-gating with zero wall-clock cost and zero flakiness.
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};

    use tokio::sync::Mutex as AsyncMutex;

    use super::*;

    // `HEDGE_FIRES` is a single process-global static, and `cargo test` runs
    // every `#[test]`/`#[tokio::test]` fn in this binary concurrently on
    // separate threads by default. These three tests all assert an EXACT
    // count on that shared counter, so without serializing them a genuine
    // race (test A's reset landing between test B's fire and test B's
    // assertion, or vice versa) would make the suite flaky. An async-aware
    // `tokio::sync::Mutex` (rather than `std::sync::Mutex`) is used so the
    // guard can be held across `.await` points without tripping
    // `clippy::await_holding_lock`.
    static HEDGE_FIRES_TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

    #[tokio::test(start_paused = true)]
    async fn primary_faster_than_hedge_ms_never_fires_a_backup() {
        let _guard = HEDGE_FIRES_TEST_LOCK.lock().await;
        let _ = hedge_fires_since_reset(); // reset any cross-test residue
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let result = hedge_race(50, move || {
            let calls = calls2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await; // < hedge_ms
                Ok::<u32, ()>(7)
            }
        })
        .await;
        assert_eq!(result, Ok(7));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "primary answered well before hedge_ms elapsed; op must have been called exactly once"
        );
        assert_eq!(hedge_fires_since_reset(), 0, "no backup means no hedge fire");
    }

    #[tokio::test(start_paused = true)]
    async fn backup_fires_exactly_once_only_after_hedge_ms_and_faster_response_wins() {
        let _guard = HEDGE_FIRES_TEST_LOCK.lock().await;
        let _ = hedge_fires_since_reset();
        let calls = Arc::new(AtomicU32::new(0));
        // Records the virtual-clock offset (relative to test start) at which
        // each call to `op` began, so we can assert the SECOND call happened
        // no earlier than exactly `hedge_ms` — proving the gate, not just the
        // call count.
        let fired_at: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
        let start = tokio::time::Instant::now();

        let calls2 = calls.clone();
        let fired_at2 = fired_at.clone();
        let result = hedge_race(50, move || {
            let calls = calls2.clone();
            let fired_at = fired_at2.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                fired_at.lock().unwrap().push(tokio::time::Instant::now() - start);
                if n == 0 {
                    // Primary: slower than the hedge window.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Ok::<u32, ()>(0) // "primary" answer
                } else {
                    // Backup: faster — should win the race.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok::<u32, ()>(1) // "backup" answer
                }
            }
        })
        .await;

        assert_eq!(result, Ok(1), "the faster (backup) response must win the race");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "exactly one backup must fire (primary + backup = 2 total calls, never a storm)"
        );
        assert_eq!(hedge_fires_since_reset(), 1, "exactly one hedge fire must be recorded");

        let stamps = fired_at.lock().unwrap();
        assert_eq!(stamps.len(), 2);
        assert_eq!(stamps[0], Duration::from_millis(0), "primary fires immediately");
        assert_eq!(
            stamps[1],
            Duration::from_millis(50),
            "backup must fire only AFTER hedge_ms of virtual time elapses, not before and not late"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hedge_disabled_never_arms_a_timer_or_backup() {
        let _guard = HEDGE_FIRES_TEST_LOCK.lock().await;
        let _ = hedge_fires_since_reset();
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = calls.clone();
        let result = hedge_race(0, move || {
            let calls = calls2.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<u32, ()>(42)
            }
        })
        .await;
        assert_eq!(result, Ok(42));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "hedge_ms=0 must call op exactly once");
        assert_eq!(hedge_fires_since_reset(), 0, "hedge_ms=0 must never fire a backup");
    }
}
