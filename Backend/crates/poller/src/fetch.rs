// Backend/crates/poller/src/fetch.rs
//! Page-fetch orchestration. Two modes: a ROTATING WINDOW (cheap, covers the
//! pool over several polls — `fetch_complete=false`, never gates anti-drift) and
//! a FULL SWEEP (every `FULL_SYNC_EVERY` cycles or when the pool size changed —
//! `fetch_complete=true` ONLY if every page succeeded). Fast-detect is an opt-in
//! page-1 peek (default OFF). Correctness gate: a partial/parallel-failed sweep
//! is NEVER `fetch_complete` (design correction #9).
use std::collections::HashSet;

use futures::future::join_all;
use spx_client::{SpxBooking, SpxClient, SpxCookies};

use crate::state::PollerConfig;

/// Result of one sweep, wrapping the `fetch_complete` gate as a TYPE so callers
/// cannot run anti-drift off a partial fetch (Task 5 consumes this).
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub fetch_complete: bool,
    pub spx_id_set: HashSet<String>,
    pub page_failures: u32,
    pub bookings: Vec<SpxBooking>,
    pub was_full_sweep: bool,
}

/// Do a full sweep this cycle? Forced by a pool-size change (new-ticket signal),
/// by `full_sync_every==0` (always-full), or every Nth cycle.
pub fn should_full_sweep(poll_count: u64, full_sync_every: u64, pool_changed: bool) -> bool {
    pool_changed || full_sync_every == 0 || poll_count.is_multiple_of(full_sync_every)
}

/// Rotating window for a non-full cycle. Rotates a `max_pages`-wide window across
/// polls so the whole pool is eventually covered. 1-indexed pageno, inclusive.
pub fn window_pages(poll_count: u64, max_pages: u32) -> (u32, u32) {
    let mp = max_pages.max(1);
    // Rotate the START page by (poll_count % something); keep it simple and
    // bounded so a small pool still gets page 1 frequently (where new tickets
    // land). Windows of width `mp`, offset stepping by `mp` each poll.
    let start = 1 + (poll_count as u32 % mp);
    (start, start + mp - 1)
}

/// Fetch `pageno_start..=pageno_end` in parallel. Each page is best-effort; a
/// failed page → `page_failures += 1` and forces `fetch_complete=false`.
/// `hedge_ms` is routed straight to `hedged_page` per page: `0` (the default,
/// and always the value `fast_detect` passes) is a single shot identical to a
/// plain `fetch_bookings` call; `sweep` passes `cfg.sweep_hedge_ms`, which is
/// itself only ever non-zero on the FULL-SWEEP path (never the rotating
/// window — Task 3's correction #1).
async fn fetch_range(
    client: &SpxClient,
    cookies: &SpxCookies,
    page_size: u32,
    pageno_start: u32,
    pageno_end: u32,
    hedge_ms: u64,
) -> (Vec<SpxBooking>, u32) {
    let futs = (pageno_start..=pageno_end)
        .map(|pageno| crate::hedge::hedged_page(client, cookies, pageno, page_size, hedge_ms));
    let results = join_all(futs).await;
    let mut bookings = Vec::new();
    let mut failures = 0u32;
    for r in results {
        match r {
            Ok(mut page) => bookings.append(&mut page),
            Err(()) => failures += 1, // best-effort: a failed page does not abort
        }
    }
    (bookings, failures)
}

/// Full sweep OR rotating window per `full`.
pub async fn sweep(
    client: &SpxClient,
    cookies: &SpxCookies,
    cfg: &PollerConfig,
    poll_count: u64,
    full: bool,
) -> FetchOutcome {
    let (start, end) = if full {
        (1, cfg.max_pages.max(1)) // whole pool (bounded by max_pages)
    } else {
        window_pages(poll_count, cfg.max_pages)
    };
    // Hedging is gated to the FULL-SWEEP path only: a rotating window is
    // cheap-by-design and re-covers any given page again within a few polls,
    // so trimming its tail latency isn't worth the extra QPS (correction #1
    // / design note — matches the reference's real
    // `forceFullSweep ? env.SPX_SWEEP_HEDGE_MS : 0`).
    let hedge_ms = if full { cfg.sweep_hedge_ms } else { 0 };
    let (bookings, page_failures) =
        fetch_range(client, cookies, cfg.page_size, start, end, hedge_ms).await;

    let spx_id_set: HashSet<String> = bookings.iter().map(|b| b.id.clone()).collect();
    // fetch_complete: a FULL sweep with ZERO page failures. A rotating window is
    // never fetch_complete (it did not observe the whole pool).
    let fetch_complete = full && page_failures == 0;

    FetchOutcome {
        fetch_complete,
        spx_id_set,
        page_failures,
        bookings,
        was_full_sweep: full,
    }
}

/// Opt-in fast-detect: peek pages 1..=`fast_detect_pages` (default 0 → no HTTP,
/// empty result). When enabled, this is a cheap early signal that new tickets
/// exist so the caller can jump straight to a full sweep (~75–150ms faster
/// detection). Correction #1: OFF by default.
pub async fn fast_detect(
    client: &SpxClient,
    cookies: &SpxCookies,
    cfg: &PollerConfig,
) -> Vec<SpxBooking> {
    if cfg.fast_detect_pages == 0 {
        return Vec::new(); // OFF — no network at all
    }
    // Fast-detect is an early peek, never hedged (hedge_ms=0 → single shot).
    let (bookings, _fail) =
        fetch_range(client, cookies, cfg.page_size, 1, cfg.fast_detect_pages, 0).await;
    bookings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_sweep_cadence_every_third_cycle() {
        // full_sync_every=3, no pool change: full sweep on 3,6,9,... (poll_count%3==0).
        let hits: Vec<u64> = (1..=9).filter(|&pc| should_full_sweep(pc, 3, false)).collect();
        assert_eq!(hits, vec![3, 6, 9]);
    }

    #[test]
    fn pool_change_forces_full_sweep_off_cadence() {
        assert!(should_full_sweep(1, 3, true), "a pool-size change forces a full sweep");
        assert!(!should_full_sweep(1, 3, false));
    }

    #[test]
    fn full_sync_every_zero_is_always_full() {
        assert!(should_full_sweep(7, 0, false));
    }

    #[test]
    fn window_rotates_and_covers_page_one_frequently() {
        assert_eq!(window_pages(0, 10), (1, 10));
        assert_eq!(window_pages(10, 10), (1, 10)); // wraps
        let (s, e) = window_pages(3, 10);
        assert_eq!((s, e), (4, 13));
    }
}
