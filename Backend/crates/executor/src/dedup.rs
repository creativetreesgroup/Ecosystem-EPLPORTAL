// Backend/crates/executor/src/dedup.rs
//! Layer 1 (in-proc, fastest): per-account in-flight + accepted sets. One
//! `AccountDedupState` PER account (not a global set — SPX allows numerically
//! identical spxIds across different accounts, so a shared set would collide).
//!
//! `DashSet::insert() -> bool` (true = newly inserted) is an atomic
//! check-and-set per key: the single call closes the `has()`+`add()` race a
//! literal port of the reference's JS `Set` would introduce under concurrent
//! Tokio tasks. `accepted_ids` is a `DashMap<String, Instant>` (not a plain
//! `DashSet`) so the memory bound (5000, per the reference) can evict the OLDEST
//! entry — JS `Set` iterates in insertion order natively, so this is parity.
use std::time::Instant;

use dashmap::{DashMap, DashSet};

/// Reference memory bound for `accepted_ids` (poller.ts:997-1000).
const MAX_ACCEPTED_IDS: usize = 5000;

pub struct AccountDedupState {
    accepting_now: DashSet<String>,
    accepted_ids: DashMap<String, Instant>,
}

impl Default for AccountDedupState {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountDedupState {
    pub fn new() -> Self {
        Self {
            accepting_now: DashSet::new(),
            accepted_ids: DashMap::new(),
        }
    }

    /// Claim an spxId for in-flight accept. Returns `true` iff THIS caller won
    /// the claim. Already-accepted spxIds return `false` immediately; the
    /// atomic decision is `accepting_now.insert()` (single call, no race).
    pub fn try_begin_accept(&self, spx_id: &str) -> bool {
        if self.accepted_ids.contains_key(spx_id) {
            return false;
        }
        self.accepting_now.insert(spx_id.to_string())
    }

    /// Release an in-flight claim without recording an accept (e.g. the dispatch
    /// failed and may be retried later).
    pub fn abort_accept(&self, spx_id: &str) {
        self.accepting_now.remove(spx_id);
    }

    /// Record a successful accept: drop the in-flight claim and add to
    /// `accepted_ids`, evicting the oldest entry if over the 5000 bound.
    pub fn commit_accept(&self, spx_id: &str) {
        self.accepting_now.remove(spx_id);
        self.accepted_ids.insert(spx_id.to_string(), Instant::now());
        self.evict_if_needed();
    }

    /// Seed an accepted spxId from the durable ZSET restore (Layer 3).
    pub fn insert_restored(&self, spx_id: &str) {
        self.accepted_ids.insert(spx_id.to_string(), Instant::now());
        self.evict_if_needed();
    }

    /// True if this spxId is in-flight OR already accepted.
    pub fn is_known(&self, spx_id: &str) -> bool {
        self.accepting_now.contains(spx_id) || self.accepted_ids.contains_key(spx_id)
    }

    pub fn accepted_len(&self) -> usize {
        self.accepted_ids.len()
    }

    fn evict_if_needed(&self) {
        while self.accepted_ids.len() > MAX_ACCEPTED_IDS {
            // Find the oldest key WITHOUT holding an iterator across the remove
            // (DashMap: holding a shard ref while removing can deadlock).
            let mut oldest_key: Option<String> = None;
            let mut oldest_t: Option<Instant> = None;
            for e in self.accepted_ids.iter() {
                if oldest_t.is_none_or(|t| *e.value() < t) {
                    oldest_t = Some(*e.value());
                    oldest_key = Some(e.key().clone());
                }
            }
            match oldest_key {
                Some(k) => {
                    self.accepted_ids.remove(&k);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_then_commit_transitions_known_state() {
        let s = AccountDedupState::new();
        assert!(!s.is_known("A"));
        assert!(s.try_begin_accept("A"));
        assert!(s.is_known("A")); // in accepting_now
        s.commit_accept("A");
        assert!(s.is_known("A")); // now in accepted_ids
                                  // A second begin on an already-accepted id must fail.
        assert!(!s.try_begin_accept("A"));
    }

    #[test]
    fn abort_releases_inflight_claim() {
        let s = AccountDedupState::new();
        assert!(s.try_begin_accept("B"));
        s.abort_accept("B");
        assert!(!s.is_known("B"));
        assert!(s.try_begin_accept("B")); // reclaimable after abort
    }

    #[test]
    fn evicts_oldest_when_over_5000() {
        let s = AccountDedupState::new();
        // Insert 5000 with increasing timestamps (natural order), then one more.
        for i in 0..MAX_ACCEPTED_IDS {
            s.insert_restored(&format!("id-{i:05}"));
        }
        assert_eq!(s.accepted_len(), MAX_ACCEPTED_IDS);
        // The first-inserted ("id-00000") is the oldest; inserting a new one
        // must evict it, keeping the length at the bound.
        std::thread::sleep(std::time::Duration::from_millis(2));
        s.insert_restored("id-newest");
        assert_eq!(s.accepted_len(), MAX_ACCEPTED_IDS);
        assert!(s.is_known("id-newest"));
        assert!(
            !s.is_known("id-00000"),
            "oldest entry must have been evicted"
        );
    }

    // DoD #1: many concurrent Tokio tasks race to claim the SAME new spxId;
    // exactly one must win. This is the atomic `DashSet::insert()` (single call),
    // NOT a race-prone `has()`+`add()` two-step.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_claim_exactly_one_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let state = Arc::new(AccountDedupState::new());
        let wins = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..128 {
            let s = state.clone();
            let w = wins.clone();
            handles.push(tokio::spawn(async move {
                if s.try_begin_accept("SPXID_CONTESTED") {
                    w.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            wins.load(Ordering::SeqCst),
            1,
            "exactly one task may win the in-flight claim"
        );
    }
}
