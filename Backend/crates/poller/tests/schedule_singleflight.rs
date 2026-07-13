// Backend/crates/poller/tests/schedule_singleflight.rs
//! DoD #1: (a) the account loop is single-flight — poll cycles never overlap
//! (structural: one task, sequential await); (b) a poke wakes the loop from its
//! `sleep` BEFORE the full interval elapses. Both use paused virtual time so the
//! elapsed durations are real controlled facts, not wall-clock waits.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

// A self-contained mini-loop mirroring `spawn_account_loop`'s structure, so the
// test can inject an observable async `poll_once` (the real one is not yet
// pluggable in Task 1; this proves the LOOP SHAPE that `schedule.rs` uses).
async fn run_loop(
    interval: Duration,
    poke: Arc<Notify>,
    in_flight: Arc<AtomicUsize>,
    cycles: Arc<AtomicUsize>,
    stop_after: usize,
) {
    loop {
        // Re-entrancy sentinel: if two cycles ever overlapped, this would exceed 1.
        let prev = in_flight.fetch_add(1, Ordering::SeqCst);
        assert_eq!(prev, 0, "poll cycles must never overlap (single-flight)");
        // Simulate the awaited body of poll_once.
        tokio::time::sleep(Duration::from_millis(5)).await;
        in_flight.fetch_sub(1, Ordering::SeqCst);

        let n = cycles.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= stop_after {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = poke.notified() => {}
        }
    }
}

#[tokio::test(start_paused = true)]
async fn poll_cycles_never_overlap() {
    let poke = Arc::new(Notify::new());
    let in_flight = Arc::new(AtomicUsize::new(0));
    let cycles = Arc::new(AtomicUsize::new(0));
    let handle = tokio::spawn(run_loop(
        Duration::from_millis(100),
        poke.clone(),
        in_flight.clone(),
        cycles.clone(),
        4,
    ));
    // Fire pokes faster than cycles complete — a broken (spawn-per-cycle) design
    // would overlap and trip the sentinel assert inside run_loop.
    for _ in 0..20 {
        poke.notify_one();
        tokio::time::advance(Duration::from_millis(3)).await;
    }
    tokio::time::advance(Duration::from_millis(500)).await;
    handle.await.unwrap();
    assert_eq!(cycles.load(Ordering::SeqCst), 4);
}

/// Advance the paused clock in small steps (instead of one big jump) until
/// `cond` holds or `max_steps` is exhausted, returning whether `cond` was
/// observed true. This matters for two reasons:
///
/// 1. `tokio::time::advance(d)` moves the mock clock forward *synchronously*
///    and only THEN does a single `yield_now().await` — it does not itself
///    drive the executor through a chain of not-yet-registered timers. A
///    single large `advance(10ms)` called before the spawned task has even
///    had its first poll can jump straight past the point where the task
///    registers its internal `sleep(5ms)`, so the task's timer ends up armed
///    relative to the already-advanced clock and never fires within that one
///    call. Stepping in 1ms increments (mirroring
///    `poll_cycles_never_overlap`'s working pattern) gives the executor a
///    poll/park opportunity at each intermediate instant, so a timer that
///    becomes due mid-step is actually observed and fired.
/// 2. Returning a `bool` (rather than unconditionally proceeding) matters
///    because callers must assert on it BEFORE ever `.await`ing the loop's
///    `JoinHandle` — see the comment at the call site below.
async fn advance_until(cond: impl Fn() -> bool, max_steps: usize) -> bool {
    for _ in 0..max_steps {
        if cond() {
            return true;
        }
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    cond()
}

#[tokio::test(start_paused = true)]
async fn poke_wakes_before_full_interval() {
    let poke = Arc::new(Notify::new());
    let cycles = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    // Long interval (10s): if the loop waited the FULL interval, driving only
    // ~20ms of virtual time (well below 10s) would never see a 2nd cycle. A
    // poke must produce one anyway, and quickly.
    let handle = tokio::spawn(run_loop(
        Duration::from_secs(10),
        poke.clone(),
        in_flight,
        cycles.clone(),
        2,
    ));
    // Let the first cycle finish (5ms body) and enter its 10s sleep. Budget
    // 10 x 1ms steps — comfortably more than the 5ms body needs.
    assert!(
        advance_until(|| cycles.load(Ordering::SeqCst) >= 1, 10).await,
        "first cycle did not complete within the 10ms budget"
    );
    assert_eq!(cycles.load(Ordering::SeqCst), 1, "one cycle done, now sleeping 10s");

    // Poke: must cancel the 10s sleep and run a 2nd cycle within ~5ms, NOT
    // 10s. Critically, we assert on the BOUNDED probe below BEFORE ever
    // calling `handle.await`: tokio's paused-clock runtime auto-advances to
    // the next timer deadline once it is fully idle (nothing else
    // progressing), so an unconditional `handle.await` would eventually
    // complete cycle 2 regardless of whether the poke did anything — it
    // would simply auto-fast-forward straight to the 10s deadline, silently
    // defeating the test (verified: with `poke.notify_one()` commented out,
    // a version of this test that called `handle.await` before asserting
    // still reported `cycles == 2`, via that auto-advance). Bounding the
    // manual advance to a tiny 10ms budget — far short of the 10s interval —
    // and asserting on THAT bounded probe, without giving the runtime any
    // chance to auto-advance past it, is what makes this a real proof of an
    // EARLY wake rather than an eventual one.
    poke.notify_one();
    assert!(
        advance_until(|| cycles.load(Ordering::SeqCst) >= 2, 10).await,
        "poke did not wake the loop within a tiny (10ms) budget — the sleep must not have been cancelled"
    );
    handle.await.unwrap();
    assert_eq!(cycles.load(Ordering::SeqCst), 2, "poke must wake before the 10s interval");
}
