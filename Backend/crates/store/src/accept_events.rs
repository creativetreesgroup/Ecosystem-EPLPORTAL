// Backend/crates/store/src/accept_events.rs
//! `accept_events` — append-only audit trail (`app_role` has no UPDATE/DELETE grant on this
//! table at all, migration 0008; see `accept_events_is_append_only_for_app_role` in this
//! crate's own test module for the proof). `/bookings/spx-log` (Task 9) reads it; Task 10's
//! manual-accept route is this module's first production writer.
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::AcceptEvent;

/// Fields `insert` needs. `outcome` must be one of the CHECK constraint's allowed values
/// (`'accepted' | 'rejected' | 'skipped' | 'taken_by_agency' | 'failed' | 'agency_dup_unverified'`,
/// migration 0008) — this module does not re-validate that client-side; an invalid value
/// surfaces as a real Postgres CHECK-constraint violation (`23514`), which `ApiError::From<sqlx::Error>`
/// maps to `500 Internal` (not `23505`, so NOT the `Conflict` path) — acceptable here since this
/// is an internal audit write, not a client-facing form field a user could realistically get
/// wrong.
#[derive(Debug, Clone)]
pub struct NewAcceptEvent {
    pub booking_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub outcome: String,
    pub local_dispatch_us: Option<i64>,
    pub accept_e2e_ms: Option<i64>,
    pub detail: Value,
}

pub async fn insert(
    pool: &PgPool,
    tenant_id: Uuid,
    new: &NewAcceptEvent,
) -> Result<AcceptEvent, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AcceptEvent>(
        "INSERT INTO accept_events (tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail, created_at",
    )
    .bind(tenant_id)
    .bind(new.booking_id)
    .bind(new.rule_id)
    .bind(&new.outcome)
    .bind(new.local_dispatch_us)
    .bind(new.accept_e2e_ms)
    .bind(&new.detail)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// `/bookings/spx-log`: newest first, using the `idx_accept_events_tenant_created` index.
pub async fn list_for_tenant(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<AcceptEvent>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, AcceptEvent>(
        "SELECT id, tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail, created_at \
         FROM accept_events WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(tenant_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Per-booking audit trail — `GET /bookings/:id/audit-trail` (Fase 7c). A single booking has
/// at most a handful of accept attempts (one per manual/auto try), so unlike `list_for_tenant`
/// this takes no `limit`/`offset` — there is no realistic case where pagination matters here.
pub async fn list_for_booking(
    pool: &PgPool,
    tenant_id: Uuid,
    booking_id: Uuid,
) -> Result<Vec<AcceptEvent>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, AcceptEvent>(
        "SELECT id, tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail, created_at \
         FROM accept_events WHERE tenant_id = $1 AND booking_id = $2 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .bind(booking_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}
