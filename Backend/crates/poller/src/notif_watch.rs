// Backend/crates/poller/src/notif_watch.rs
//! Per-account notif watcher: a SEPARATE task that only reads two light SPX
//! counters and pokes the poll loop when the pending pool changes. It NEVER
//! touches dedup/executor/DB — `poke.notify_one()` is its ONLY effect.
//!
//! "Staggered parallel lanes" (design correction #3: the master spec's
//! "tiered/multi-interval polling" description does NOT match the reference
//! — the reference runs ONE interval with `SPX_NOTIF_WATCH_CONCURRENCY`
//! overlapping lanes, default 2) is implemented here as a TICK-RATE
//! multiplier rather than literal concurrently-spawned sub-tasks: while
//! healthy, the nominal interval is divided across `concurrency` lanes so the
//! combined read cadence is `interval/concurrency` (the reference's stated
//! detection-latency target — "~interval/concurrency, not interval+RTT");
//! while backing off, lanes collapse to exactly 1 and the cadence is governed
//! by the backoff value. This deliberately avoids nested `tokio::spawn` per
//! lane: a second layer of spawned tasks would not be tied to the single
//! `JoinHandle` this module returns, so aborting/dropping that handle (how
//! the account supervisor stops a watcher — Task 1's `AccountHandle`
//! pattern) would leak orphaned lane tasks running forever in the
//! background. A single task with a tick-rate that itself varies achieves
//! the same OBSERVABLE property (N reads per interval when healthy, 1 slow
//! read per backoff period when not) with no such leak.
//!
//! Exponential backoff: ×2 per fully-failed tick, floor 250ms, cap 5000ms,
//! HARD reset to 0 the instant any tick is healthy (not gradual decay).
//!
//! Baseline suppression: the FIRST observation of the pending-count signal
//! never fires a poke (there is nothing to compare it against — treating a
//! pool that already existed before the watcher started as "new" would be
//! wrong); only a SUBSEQUENT change fires.
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::state::PollerConfig;

const BACKOFF_FLOOR_MS: u64 = 250;
const BACKOFF_CAP_MS: u64 = 5000;

/// ×2 with a 250ms floor and 5000ms cap (exact reference math). `0 -> 250`
/// (floor, not `0`), `250 -> 500 -> 1000 -> 2000 -> 4000 -> 5000` (cap), and
/// stays pinned at `5000` for any input `>= 2500`.
pub fn next_backoff(current_ms: u64) -> u64 {
    current_ms.saturating_mul(2).clamp(BACKOFF_FLOOR_MS, BACKOFF_CAP_MS)
}

/// Lanes active for the CURRENT tick: `concurrency` (clamped to at least 1)
/// while healthy, collapsed to exactly 1 the instant backoff is nonzero (the
/// reference: `state.watchBackoffMs > 0 ? 1 : concurrency`).
fn active_lanes(backoff_ms: u64, concurrency: u32) -> u32 {
    if backoff_ms > 0 {
        1
    } else {
        concurrency.max(1)
    }
}

/// The delay before the NEXT tick. Healthy: the nominal interval divided
/// across the active lanes (staggered cadence). Backing off: lanes are
/// already collapsed to 1, and the tick is governed by whichever of the
/// backoff value or the nominal interval is larger (so a tiny configured
/// interval can never defeat the backoff floor).
fn tick_delay_ms(notif_watch_ms: u64, backoff_ms: u64, concurrency: u32) -> u64 {
    if backoff_ms > 0 {
        backoff_ms.max(notif_watch_ms)
    } else {
        let lanes = active_lanes(backoff_ms, concurrency) as u64;
        (notif_watch_ms / lanes).max(1)
    }
}

/// Sum the pending-count signal from the two counter endpoints. Returns None
/// on any error (caller backs off). A change vs the last observed value is
/// what triggers a poke.
async fn read_pending_signal(client: &SpxClient, cookies: &SpxCookies) -> Option<i64> {
    // notification pending count (pn) + booking counts (count_v2). Either
    // alone is a valid change signal; sum the numeric fields defensively so
    // this does not depend on SPX's exact field names.
    let notif = client.notification_count(cookies).await.ok()?;
    let counts = client.fetch_booking_counts(cookies).await.ok()?;
    Some(sum_numeric(&notif).wrapping_add(sum_numeric(&counts)))
}

/// Sum all numeric leaves of a JSON value (a robust "did anything change"
/// hash that does not depend on SPX's exact field names).
fn sum_numeric(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::Array(a) => a.iter().map(sum_numeric).sum(),
        Value::Object(o) => o.values().map(sum_numeric).sum(),
        Value::String(s) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// Per-account watcher state: current backoff and the last observed signal
/// (`-1` = "no observation yet" — the baseline-suppression sentinel).
pub struct WatchState {
    pub backoff_ms: u64,
    pub last_pending: i64,
}

/// The core loop, generic over the read primitive so it is testable with
/// PURE in-memory futures under `tokio::time::pause` — no sockets — mirroring
/// `hedge::hedge_race`'s split between a paused-time unit test of the
/// primitive itself and a wiremock end-to-end test of the HTTP wiring
/// (`tests/notif_watch.rs`). Runs forever; the caller's `JoinHandle` is
/// aborted/dropped to stop it (there is no other exit).
async fn watch_loop<F, Fut>(notif_watch_ms: u64, concurrency: u32, poke: Arc<Notify>, mut read: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<i64>>,
{
    let mut st = WatchState {
        backoff_ms: 0,
        last_pending: -1,
    };
    loop {
        let delay = tick_delay_ms(notif_watch_ms, st.backoff_ms, concurrency);
        tokio::time::sleep(Duration::from_millis(delay)).await;

        match read().await {
            Some(sig) => {
                // Baseline suppression: only compare (and poke) once a prior
                // observation exists. The very first healthy read just seeds
                // `last_pending` and resets backoff.
                if st.last_pending >= 0 && sig != st.last_pending {
                    poke.notify_one(); // wake the poll loop -> full sweep next cycle
                }
                st.last_pending = sig;
                st.backoff_ms = 0; // HARD reset on the first healthy tick, not decay
            }
            None => {
                st.backoff_ms = next_backoff(st.backoff_ms);
            }
        }
    }
}

/// Spawn the watcher. It loops forever; abort/drop the handle to stop it
/// (done when the account's poller is torn down).
pub fn spawn_notif_watcher(
    client: Arc<SpxClient>,
    cookies: SpxCookies,
    cfg: PollerConfig,
    poke: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if cfg.notif_watch_ms == 0 {
            return; // watcher disabled
        }
        watch_loop(
            cfg.notif_watch_ms,
            cfg.notif_watch_concurrency,
            poke,
            move || {
                let client = client.clone();
                let cookies = cookies.clone();
                async move { read_pending_signal(&client, &cookies).await }
            },
        )
        .await;
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn backoff_ramps_from_floor_to_cap_and_stays_pinned() {
        assert_eq!(next_backoff(0), 250); // floor
        assert_eq!(next_backoff(250), 500);
        assert_eq!(next_backoff(500), 1000);
        assert_eq!(next_backoff(1000), 2000);
        assert_eq!(next_backoff(2000), 4000);
        assert_eq!(next_backoff(3000), 5000); // 6000 capped
        assert_eq!(next_backoff(5000), 5000); // stays at cap
    }

    #[test]
    fn active_lanes_collapses_to_one_while_backing_off() {
        assert_eq!(active_lanes(0, 4), 4, "healthy: full configured concurrency");
        assert_eq!(active_lanes(250, 4), 1, "any nonzero backoff collapses to 1 lane");
        assert_eq!(active_lanes(5000, 4), 1);
        assert_eq!(active_lanes(0, 0), 1, "concurrency is clamped to at least 1");
    }

    #[test]
    fn tick_delay_divides_interval_across_lanes_when_healthy() {
        assert_eq!(tick_delay_ms(100, 0, 4), 25, "interval/concurrency when healthy");
        assert_eq!(tick_delay_ms(100, 0, 1), 100);
        assert_eq!(tick_delay_ms(10, 0, 3), 3, "integer division, floored");
    }

    #[test]
    fn tick_delay_is_backoff_governed_and_never_below_it_while_unhealthy() {
        assert_eq!(tick_delay_ms(10, 250, 4), 250, "backoff dominates a tiny interval");
        assert_eq!(tick_delay_ms(9000, 250, 4), 9000, "a large interval still dominates a small backoff");
    }

    /// Drive `watch_loop` under `start_paused = true` and collect the exact
    /// virtual-time timestamp of each `read()` call, via an unbounded channel
    /// rather than a manually-stepped `tokio::time::advance()` loop.
    ///
    /// This distinction matters: `#[tokio::test(start_paused = true)]`'s
    /// built-in "auto-advance when fully idle" behavior (the same mechanism
    /// `schedule_singleflight.rs`'s tests rely on) jumps the mocked clock
    /// EXACTLY to the next timer deadline whenever the only thing left to
    /// run is `rx.recv().await` — verified empirically to reproduce a plain
    /// `sleep(250ms).await`'s real 250ms to the millisecond, with zero
    /// drift. A hand-rolled loop of many small `tokio::time::advance(1ms)`
    /// calls (this crate's `advance_until` pattern, used elsewhere for
    /// bounded "did X happen soon" checks) was tried first here and measured
    /// a few milliseconds of cumulative rounding drift over hundreds of
    /// small steps — harmless for a boolean "poked within budget" check, but
    /// enough to break an exact-millisecond gap assertion. Channel-driven
    /// waiting sidesteps manual stepping entirely.
    async fn collect_call_timestamps<F, Fut>(
        n: usize,
        notif_watch_ms: u64,
        concurrency: u32,
        mut script: F,
    ) -> (Vec<Duration>, JoinHandle<()>)
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Option<i64>> + Send + 'static,
    {
        let start = tokio::time::Instant::now();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Duration>();
        let poke = Arc::new(Notify::new());
        let handle = tokio::spawn(async move {
            watch_loop(notif_watch_ms, concurrency, poke, move || {
                let tx = tx.clone();
                let fut = script();
                async move {
                    let out = fut.await;
                    let _ = tx.send(tokio::time::Instant::now() - start);
                    out
                }
            })
            .await;
        });
        let mut stamps = Vec::with_capacity(n);
        for _ in 0..n {
            stamps.push(rx.recv().await.expect("watch_loop task ended before n ticks"));
        }
        (stamps, handle)
    }

    fn gaps_ms(stamps: &[Duration]) -> Vec<u64> {
        stamps.windows(2).map(|w| (w[1] - w[0]).as_millis() as u64).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_ramp_is_exact_under_real_simulated_time() {
        // concurrency=1 removes lane-division from this specific timing proof
        // (lane collapse gets its own dedicated test below); every tick here
        // fails, so the ONLY thing governing the sleep between calls after
        // the first is `next_backoff`. 8 calls needed to observe the full
        // ramp (250,500,1000,2000,4000,5000,5000-capped) as the 7 gaps AFTER
        // the first (healthy-interval) tick.
        let (stamps, handle) = collect_call_timestamps(8, 10, 1, || async { None::<i64> }).await;
        handle.abort();

        assert_eq!(
            gaps_ms(&stamps),
            vec![250, 500, 1000, 2000, 4000, 5000, 5000],
            "backoff must ramp exactly 250/500/1000/2000/4000/5000-capped"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_resets_to_zero_immediately_not_gradually_on_next_healthy_tick() {
        // Script: fail, fail, fail (backoff climbs 250 -> 500 -> 1000), then
        // succeed once (must hard-reset to 0), then fail again (must restart
        // the ramp from next_backoff(0)=250, NOT continue from 1000/2000).
        let script: Arc<Mutex<Vec<Option<i64>>>> =
            Arc::new(Mutex::new(vec![None, None, None, Some(1), None]));
        let (stamps, handle) = collect_call_timestamps(5, 10, 1, move || {
            let script = script.clone();
            async move {
                let mut s = script.lock().unwrap();
                if s.is_empty() { None } else { s.remove(0) }
            }
        })
        .await;
        handle.abort();

        // call1->call2: next_backoff(0)=250. call2->call3: next_backoff(250)=500.
        // call3->call4: next_backoff(500)=1000 (this is the tick where call4
        // itself succeeds, so the sleep BEFORE call4 still reflects the
        // pre-reset backoff of 1000 -- the reset only affects the NEXT sleep).
        // call4->call5: backoff was reset to 0 by call4's success, so this
        // gap is the HEALTHY interval-based delay (10ms/1 lane = 10), not
        // next_backoff(1000)=2000 and not any gradually-decayed value.
        assert_eq!(
            gaps_ms(&stamps),
            vec![250, 500, 1000, 10],
            "backoff must hard-reset to 0 (not decay) the instant a tick is healthy"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lane_concurrency_collapses_to_one_while_backing_off_then_restores() {
        // concurrency=4, interval=100 -> healthy cadence is 100/4=25ms between
        // ticks. Script: healthy, healthy, FAIL, healthy, healthy. Each
        // tick's PRE-sleep delay is governed by the backoff state left by the
        // PREVIOUS tick's outcome, so the failure at call3 shows up as the
        // gap BEFORE call4 (250ms, collapsed to 1 lane), and the following
        // healthy call4 resets backoff so the gap before call5 is back to the
        // steady 25ms/4-lane cadence.
        let script: Arc<Mutex<Vec<Option<i64>>>> =
            Arc::new(Mutex::new(vec![Some(1), Some(1), None, Some(1), Some(1)]));
        let (stamps, handle) = collect_call_timestamps(5, 100, 4, move || {
            let script = script.clone();
            async move {
                let mut s = script.lock().unwrap();
                if s.is_empty() { Some(1) } else { s.remove(0) }
            }
        })
        .await;
        handle.abort();

        assert_eq!(
            gaps_ms(&stamps),
            vec![25, 25, 250, 25],
            "steady healthy cadence is interval/concurrency (25ms) for the first two gaps; \
             a failed tick (call3) collapses lanes to 1, so the gap immediately AFTER it \
             (before call4) is backoff-governed (250ms); the following healthy tick (call4) \
             hard-resets backoff, so the gap before call5 returns to 25ms"
        );
    }

    /// Step the paused clock forward until `cond` holds or `max_steps` is
    /// exhausted (Task 1/3's established pattern —
    /// `schedule_singleflight.rs::advance_until`). Used below only for
    /// bounded "did X happen yet" checks (not exact-millisecond assertions,
    /// where `collect_call_timestamps`'s channel-based waiting is used
    /// instead — see its doc comment for why).
    async fn advance_until(step_ms: u64, cond: impl Fn() -> bool, max_steps: usize) -> bool {
        for _ in 0..max_steps {
            if cond() {
                return true;
            }
            tokio::time::advance(Duration::from_millis(step_ms)).await;
        }
        cond()
    }

    #[tokio::test(start_paused = true)]
    async fn first_observation_never_pokes_but_a_subsequent_change_does() {
        // Script: baseline (5), unchanged (5), changed (9). Only the third
        // tick may poke.
        let script: Arc<Mutex<Vec<Option<i64>>>> =
            Arc::new(Mutex::new(vec![Some(5), Some(5), Some(9)]));
        let poke = Arc::new(Notify::new());
        let poke_count = Arc::new(AtomicUsize::new(0));

        // A background "poke observer": re-arms after every notification, so
        // it counts total pokes over time without racing the producer (the
        // same pattern used to prove `poke.notify_one()` really fires in
        // Task 1's single-flight tests, generalized to count > 1 wake-up).
        let observer_poke = poke.clone();
        let observer_count = poke_count.clone();
        let observer = tokio::spawn(async move {
            loop {
                observer_poke.notified().await;
                observer_count.fetch_add(1, Ordering::SeqCst);
            }
        });

        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let calls2 = calls.clone();
        let script2 = script.clone();
        let watch_poke = poke.clone();
        let handle = tokio::spawn(async move {
            watch_loop(10, 1, watch_poke, move || {
                let calls = calls2.clone();
                let script = script2.clone();
                async move {
                    *calls.lock().unwrap() += 1;
                    let mut s = script.lock().unwrap();
                    if s.is_empty() {
                        None
                    } else {
                        s.remove(0)
                    }
                }
            })
            .await;
        });

        // After the first (baseline) tick: no poke yet.
        assert!(advance_until(2, || *calls.lock().unwrap() >= 1, 200).await);
        // Give the observer task a chance to run if (incorrectly) notified.
        tokio::task::yield_now().await;
        assert_eq!(poke_count.load(Ordering::SeqCst), 0, "the FIRST observation must never poke");

        // After the second (unchanged) tick: still no poke.
        assert!(advance_until(2, || *calls.lock().unwrap() >= 2, 200).await);
        tokio::task::yield_now().await;
        assert_eq!(poke_count.load(Ordering::SeqCst), 0, "an unchanged signal must not poke");

        // After the third (changed 5 -> 9) tick: exactly one poke.
        assert!(advance_until(2, || *calls.lock().unwrap() >= 3, 200).await);
        tokio::task::yield_now().await;
        assert_eq!(poke_count.load(Ordering::SeqCst), 1, "a changed signal must poke exactly once");

        handle.abort();
        observer.abort();
    }
}
