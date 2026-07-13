// Backend/crates/executor/src/quota.rs
//! Re-read-in-lock per-rule quota consumption (port of `applyRuleConsumption`).
//! Inside the per-account lock: consume one DB quota slot atomically, then (only
//! if consumed) release the Redis in-flight slot — persist BEFORE release so the
//! effective count never dips.
use redis::AsyncCommands;
use uuid::Uuid;

use crate::gate::{ExecutorError, ExecutorHandle};

impl ExecutorHandle {
    pub async fn apply_rule_consumption(
        &self,
        pool: &store::PgPool,
        tenant_id: Uuid,
        account_id: &str,
        rule_id: Uuid,
        spx_id: &str,
    ) -> Result<store::QuotaConsumeOutcome, ExecutorError> {
        // (1)-(3): re-read latest + increment + persist, atomically, serialized
        // per account. `sqlx::Error` from `store` is consumed only via Display,
        // so `executor` needs no direct `sqlx` dependency.
        let outcome = self
            .with_account_lock(account_id, || async {
                store::consume_rule_quota(pool, tenant_id, rule_id)
                    .await
                    .map_err(|e| ExecutorError::Db(e.to_string()))
            })
            .await?;

        // (4): release the Redis in-flight slot AFTER the DB persist, and only
        // when a slot was actually consumed. Best-effort — a failed SREM only
        // leaves a slot occupied until its 600s TTL, never over-accepts.
        if matches!(outcome, store::QuotaConsumeOutcome::Consumed { .. }) {
            let inflight_key = format!("spx:inflight:{account_id}:{rule_id}");
            if let Ok(mut con) = self.redis.conn().await {
                let _: Result<usize, _> = con.srem(&inflight_key, spx_id).await;
            }
        }
        Ok(outcome)
    }
}
