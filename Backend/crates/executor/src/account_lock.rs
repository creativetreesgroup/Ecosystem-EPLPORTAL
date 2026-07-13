// Backend/crates/executor/src/account_lock.rs
//! Per-account FIFO async lock (port of the reference `withAccountLock`
//! promise-chain, as a `tokio::sync::Mutex` per account). Serializes the
//! read-modify-write of a rule's quota so no two increments for the same account
//! overlap. Locks are created lazily via `DashMap::entry`.
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::gate::ExecutorHandle;

impl ExecutorHandle {
    /// Run `f` while holding this account's lock (created on first use). FIFO per
    /// account — identical serialization property to the reference's in-proc
    /// promise chain, but async-aware.
    pub async fn with_account_lock<T, Fut, F>(&self, account_id: &str, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        // Look up (or lazily insert) the per-account lock, then DROP the DashMap
        // shard guard before awaiting the async mutex (never hold a sync shard
        // lock across an await).
        let lock: Arc<Mutex<()>> = self
            .account_locks
            .entry(account_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        f().await
    }
}
