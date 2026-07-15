// Backend/crates/store/src/accept_rules.rs
//! `accept_rules` CRUD. Fase 6c's `PUT /bookings/settings` persists the WHOLE rule list on
//! every save (replace-all, not per-rule upsert) — `core_domain::sanitize_accept_rules`/
//! `dedupe_rules` operate on the FULL list anyway, and the client-facing `AcceptRule.id`
//! (`core_domain::rule::AcceptRule.id: String`, e.g. `"rule_3"`) has no stable relationship to
//! this table's real `Uuid` primary key across saves — reconciling "is this the same rule as
//! last time" would need its own identity scheme this crate does not have. `replace_all` avoids
//! that problem entirely: every save deletes every existing row for the tenant and inserts the
//! sanitized+deduped set fresh, inside one transaction (all-or-nothing, no partial state ever
//! visible). `accepted_count` survives across saves because the CLIENT echoes it back on every
//! PUT (fields it read from a prior GET) — `core_domain::RawRuleConditions::accepted_count`
//! already exists for exactly this round-trip.
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::AcceptRule;

/// Every field `replace_all`'s INSERT needs, already mapped from `core_domain` enums to the
/// DB's plain-TEXT columns by the caller (route layer) — this module has no dependency on
/// `core_domain` and shouldn't grow one just to convert an enum to a string.
#[derive(Debug, Clone)]
pub struct NewAcceptRule {
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: String,
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub origin: String,
    pub destinations: Vec<String>,
    pub booking_type: String,
    pub shift_types: Vec<i32>,
    pub trip_types: Vec<i32>,
    pub match_mode: String,
    pub min_deadline_min: Option<i32>,
    pub max_accept_count: i32,
    pub accepted_count: i32,
}

pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<AcceptRule>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, AcceptRule>(
        "SELECT id, tenant_id, name, enabled, priority, mode, service_types, max_weight, \
         coc_only, non_coc_only, max_cod_amount, origin, destinations, booking_type, \
         shift_types, trip_types, match_mode, min_deadline_min, max_accept_count, \
         accepted_count, route_signature, created_at, updated_at \
         FROM accept_rules WHERE tenant_id = $1 ORDER BY priority DESC, created_at ASC",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Deletes every existing `accept_rules` row for `tenant_id`, then inserts `rows` fresh (each
/// gets a new `gen_random_uuid()` id). `rule_booking_targets` rows are NOT touched here — Task
/// 3's `rule_booking_targets::replace_for_rule` is a separate call the route layer makes per
/// inserted `BookingId`-mode rule, using the fresh id this fn returns.
///
/// A residual duplicate `route_signature` that somehow survived `dedupe_rules` (it shouldn't —
/// this fn trusts the caller already ran it) surfaces as a real `23505`, propagated via `?` for
/// `ApiError::From<sqlx::Error>` to map to `409` — same non-special-casing convention as every
/// other CRUD module in this crate.
pub async fn replace_all(
    pool: &PgPool,
    tenant_id: Uuid,
    rows: &[NewAcceptRule],
) -> Result<Vec<AcceptRule>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query("DELETE FROM accept_rules WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;

    let mut inserted = Vec::with_capacity(rows.len());
    for r in rows {
        let row = sqlx::query_as::<_, AcceptRule>(
            "INSERT INTO accept_rules \
             (tenant_id, name, enabled, priority, mode, service_types, max_weight, coc_only, \
              non_coc_only, max_cod_amount, origin, destinations, booking_type, shift_types, \
              trip_types, match_mode, min_deadline_min, max_accept_count, accepted_count) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
             RETURNING id, tenant_id, name, enabled, priority, mode, service_types, max_weight, \
               coc_only, non_coc_only, max_cod_amount, origin, destinations, booking_type, \
               shift_types, trip_types, match_mode, min_deadline_min, max_accept_count, \
               accepted_count, route_signature, created_at, updated_at",
        )
        .bind(tenant_id)
        .bind(&r.name)
        .bind(r.enabled)
        .bind(r.priority)
        .bind(&r.mode)
        .bind(&r.service_types)
        .bind(r.max_weight)
        .bind(r.coc_only)
        .bind(r.non_coc_only)
        .bind(r.max_cod_amount)
        .bind(&r.origin)
        .bind(&r.destinations)
        .bind(&r.booking_type)
        .bind(&r.shift_types)
        .bind(&r.trip_types)
        .bind(&r.match_mode)
        .bind(r.min_deadline_min)
        .bind(r.max_accept_count)
        .bind(r.accepted_count)
        .fetch_one(&mut *tx)
        .await?;
        inserted.push(row);
    }

    tx.commit().await?;
    Ok(inserted)
}
