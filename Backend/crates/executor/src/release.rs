// Backend/crates/executor/src/release.rs
//! Best-effort release of an auto claim so a TRANSIENT-failed ticket retries
//! next cycle instead of waiting out the 600s claim TTL. Keeps Redis keyspace
//! ownership inside `executor` (the design invariant that Fase 5 never touches
//! the shared keyspace directly). Best-effort: a failed release only leaves the
//! claim until its TTL — it never over-accepts.
use redis::AsyncCommands;
use uuid::Uuid;

use crate::gate::ExecutorHandle;

impl ExecutorHandle {
    pub async fn release_claim_auto(&self, account_id: &str, spx_id: &str, rule_id: Option<Uuid>) {
        let claim_key = format!("spx:claim:{account_id}:{spx_id}");
        if let Ok(mut con) = self.redis.conn().await {
            let _: Result<i64, _> = con.del(&claim_key).await;
            if let Some(rule) = rule_id {
                let inflight_key = format!("spx:inflight:{account_id}:{rule}");
                let _: Result<i64, _> = con.srem(&inflight_key, spx_id).await;
            }
        }
    }
}
