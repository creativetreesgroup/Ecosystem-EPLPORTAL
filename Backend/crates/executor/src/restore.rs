// Backend/crates/executor/src/restore.rs
//! Layer 3 (durable): the `spx:accepted:<acct>` sorted set (member = spxId,
//! score = accept epoch-seconds). `restore_accepted_ids` trims the set to a
//! 7-day window and loads the survivors into Layer 1 BEFORE the first poll.
use std::time::{SystemTime, UNIX_EPOCH};

use redis::AsyncCommands;

use crate::dedup::AccountDedupState;
use crate::gate::{ExecutorError, ExecutorHandle};

/// Seven days in seconds.
const WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl ExecutorHandle {
    /// Restore this account's durable accepted-ids into Layer 1.
    ///
    /// CONTRACT — Fase 5 MUST `.await` this to completion BEFORE scheduling the
    /// account's FIRST poll (reference race CP-7, poller.ts:288-292): otherwise
    /// the first poll can re-accept a ticket already won in a previous process
    /// lifetime, because Layer 1 starts empty and Layer 2's claim key may have
    /// expired. Fase 4 has no poll loop and cannot enforce this ordering; the
    /// enforcement is part of Fase 5's DoD. This function only guarantees its
    /// own correctness (a filled ZSET restores the right, in-window entries).
    pub async fn restore_accepted_ids(
        &self,
        account_id: &str,
        state: &AccountDedupState,
    ) -> Result<usize, ExecutorError> {
        let key = format!("spx:accepted:{account_id}");
        let mut con = self.redis.conn().await?;

        // Trim everything older than the 7-day window (inclusive of the cutoff).
        let cutoff = now_epoch_secs() - WINDOW_SECS;
        let _removed: usize = con.zrembyscore(&key, 0i64, cutoff).await?;

        // Load the survivors into Layer 1.
        let members: Vec<String> = con.zrange(&key, 0, -1).await?;
        for m in &members {
            state.insert_restored(m);
        }
        Ok(members.len())
    }

    /// Record a durable accept at the current time (Fase 5 calls this after a
    /// confirmed accept). Pairs with `restore_accepted_ids`.
    pub async fn record_durable_accept(
        &self,
        account_id: &str,
        spx_id: &str,
    ) -> Result<(), ExecutorError> {
        self.record_durable_accept_at(account_id, spx_id, now_epoch_secs())
            .await
    }

    /// Testable variant with an explicit epoch score.
    pub async fn record_durable_accept_at(
        &self,
        account_id: &str,
        spx_id: &str,
        epoch_secs: i64,
    ) -> Result<(), ExecutorError> {
        let key = format!("spx:accepted:{account_id}");
        let mut con = self.redis.conn().await?;
        let _: () = con.zadd(&key, spx_id, epoch_secs).await?;
        Ok(())
    }
}
