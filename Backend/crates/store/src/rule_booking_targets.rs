// Backend/crates/store/src/rule_booking_targets.rs
//! `rule_booking_targets` CRUD — the child table `BookingId`-mode `accept_rules` rows use to
//! carry their `booking_ids` list (the parent table has no such array column; see
//! `Backend/crates/store/migrations/0006_rule_booking_targets.sql`'s module context in the
//! Fase 6c design research). `booking_id_norm` is computed here via `core_domain::norm_id` —
//! the SAME normalization `core_domain::rule::dedupe_rules`'s own booking-id claiming logic and
//! `core_domain::matching::CompiledRule::compile`'s `booking_ids_norm` both use, so a row
//! written here can never disagree with how the rule engine itself would normalize the same
//! raw id (this is the whole reason Task 3 exported `norm_id` instead of writing a second
//! copy).
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::RuleBookingTarget;

/// Deletes every existing `rule_booking_targets` row for `rule_id`, then inserts one row per
/// `booking_ids` entry (raw form preserved in `booking_id_raw`; `booking_id_norm` computed via
/// `core_domain::norm_id`). Called once per `BookingId`-mode rule immediately after
/// `accept_rules::replace_all` returns that rule's fresh id — mirrors that fn's own
/// delete-then-insert-fresh strategy for the same reason (no stable cross-save identity to
/// reconcile against).
///
/// A duplicate `booking_id_norm` across TWO DIFFERENT rules in the same save would violate the
/// `(tenant_id, booking_id_norm)` UNIQUE constraint — but `core_domain::dedupe_rules` already
/// guarantees no two rules in a deduped list claim the same normalized id (see its own
/// `two_enabled_rules_share_id_earlier_one_wins` test), so this should never actually fire in
/// practice; if it does (a caller skipped dedup), it surfaces as a real `23505` via `?`.
pub async fn replace_for_rule(
    pool: &PgPool,
    tenant_id: Uuid,
    rule_id: Uuid,
    booking_ids: &[String],
) -> Result<Vec<RuleBookingTarget>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query("DELETE FROM rule_booking_targets WHERE tenant_id = $1 AND rule_id = $2")
        .bind(tenant_id)
        .bind(rule_id)
        .execute(&mut *tx)
        .await?;

    let mut inserted = Vec::with_capacity(booking_ids.len());
    for raw in booking_ids {
        let norm = core_domain::norm_id(raw);
        let row = sqlx::query_as::<_, RuleBookingTarget>(
            "INSERT INTO rule_booking_targets (tenant_id, rule_id, booking_id_raw, booking_id_norm) \
             VALUES ($1, $2, $3, $4) \
             RETURNING id, tenant_id, rule_id, booking_id_raw, booking_id_norm, created_at",
        )
        .bind(tenant_id)
        .bind(rule_id)
        .bind(raw)
        .bind(&norm)
        .fetch_one(&mut *tx)
        .await?;
        inserted.push(row);
    }

    tx.commit().await?;
    Ok(inserted)
}

/// Every `rule_booking_targets` row for `tenant_id`, grouped by nothing in particular — the
/// caller (Task 11's `GET /bookings/settings`) groups by `rule_id` itself. One query for the
/// whole tenant rather than N queries (one per `BookingId`-mode rule) keeps a settings-page
/// load to two round trips total (`accept_rules::list_all` + this), not `1 + N`.
pub async fn list_for_tenant(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Vec<RuleBookingTarget>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, RuleBookingTarget>(
        "SELECT id, tenant_id, rule_id, booking_id_raw, booking_id_norm, created_at \
         FROM rule_booking_targets WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}
