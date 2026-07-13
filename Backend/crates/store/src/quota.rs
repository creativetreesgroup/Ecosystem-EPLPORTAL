// Backend/crates/store/src/quota.rs
//! Per-rule accept-quota consumption. Atomic conditional increment so cap
//! enforcement and lost-update prevention hold even under concurrency (the
//! single UPDATE is the reference applyRuleConsumption's re-read+increment+
//! persist, race-free). No schema change — writes the existing
//! `accept_rules.accepted_count` / reads `max_accept_count`.
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaConsumeOutcome {
    /// One slot consumed; `accepted_count` is the NEW persisted value.
    Consumed { accepted_count: i32 },
    /// The cap is full (no slot consumed).
    CapReached {
        accepted_count: i32,
        max_accept_count: i32,
    },
    /// No such rule for this tenant.
    NoRule,
}

/// Consume one quota slot for `rule_id` under `tenant_id`. `max_accept_count = 0`
/// means unlimited. Atomic: the conditional UPDATE increments only if under cap,
/// and `RETURNING` reports the new value; a 0-row update means either cap-full or
/// no-such-rule (disambiguated by a follow-up read in the same transaction).
pub async fn consume_rule_quota(
    pool: &PgPool,
    tenant_id: Uuid,
    rule_id: Uuid,
) -> Result<QuotaConsumeOutcome, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;

    let updated: Option<(i32,)> = sqlx::query_as(
        "UPDATE accept_rules \
         SET accepted_count = accepted_count + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 \
           AND (max_accept_count = 0 OR accepted_count < max_accept_count) \
         RETURNING accepted_count",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    let outcome = match updated {
        Some((accepted_count,)) => QuotaConsumeOutcome::Consumed { accepted_count },
        None => {
            let row: Option<(i32, i32)> = sqlx::query_as(
                "SELECT accepted_count, max_accept_count FROM accept_rules \
                 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(rule_id)
            .bind(tenant_id)
            .fetch_optional(&mut *tx)
            .await?;
            match row {
                Some((accepted_count, max_accept_count)) => QuotaConsumeOutcome::CapReached {
                    accepted_count,
                    max_accept_count,
                },
                None => QuotaConsumeOutcome::NoRule,
            }
        }
    };

    tx.commit().await?;
    Ok(outcome)
}
