// Backend/crates/executor/src/heartbeat.rs
//! Generic, best-effort heartbeat write. Keeps Redis keyspace ownership inside
//! `executor` (the same design invariant `release.rs` documents: Fase 5 never
//! reaches into the raw Redis connection directly). `RedisPool`'s connection
//! field is `pub(crate)` to `executor`, so the poller's watchdog (Task 8)
//! cannot build `SET <key> <val> EX <ttl>` itself — this additive helper is
//! the public seam it calls instead.
//!
//! Written for a FUTURE Fase-8 observability consumer that is NOT built now
//! (YAGNI / correction #4) — a failed or missing heartbeat write must never
//! affect the caller's hot path, so all errors are swallowed.
use redis::AsyncCommands;

use crate::gate::ExecutorHandle;

impl ExecutorHandle {
    /// Best-effort `SET <key> <value_ms> EX <ttl_secs>`. Errors (including a
    /// wholly unreachable Redis) are swallowed — this must never be allowed to
    /// affect the poll loop or the watchdog's recreate decision.
    pub async fn heartbeat_set(&self, key: &str, value_ms: i64, ttl_secs: u64) {
        if let Ok(mut con) = self.redis.conn().await {
            let opts = redis::SetOptions::default().with_expiration(redis::SetExpiry::EX(ttl_secs));
            let _: Result<(), _> = con.set_options(key, value_ms, opts).await;
        }
    }
}
