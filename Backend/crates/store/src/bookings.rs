// Backend/crates/store/src/bookings.rs
//! Booking lifecycle writes for the poller: upsert (enrichment-preserving) +
//! the two anti-drift transitions. Ported from spx-portal-ref db.ts
//! (upsertBooking, expireStaleBookings, resurrectPending). No schema change.
//!
//! Two deliberate deviations from the reference SQL, both required by the
//! REAL schema already shipped in Fase 2 (`migrations/0007_bookings.sql`),
//! not stylistic choices:
//!
//! 1. `is_coc` is `GENERATED ALWAYS AS (...) STORED` — Postgres rejects an
//!    explicit value in that column on INSERT ("cannot insert a non-DEFAULT
//!    value into column"). `upsert_booking` therefore never lists `is_coc` in
//!    its INSERT; it is computed by Postgres from `spx_id`/`raw_data` exactly
//!    like every other booking insert path in this crate (see
//!    `is_coc_generated_column_matches_core_domain_is_coc_name` in
//!    `lib.rs`'s test module). `BookingUpsert::is_coc` is kept as a field for
//!    call-site/API shape reasons but is intentionally unused by the query.
//! 2. `rule_matched` is `UUID REFERENCES accept_rules(id) ON DELETE SET
//!    NULL` — it names WHICH accept rule auto-accepted a booking, not a
//!    free-text reason code. Writing the string `'expired'`/`'taken_by_other'`
//!    into it, as the reference's `rule_matched` column (a plain text field
//!    in db.ts) does, would fail every UPDATE with `invalid input syntax for
//!    type uuid`. The expired-vs-taken-by-other distinction is instead
//!    recorded as `raw_data->>'drift_reason'`, merged additively (`||`) so no
//!    existing `raw_data` key is ever clobbered — consistent with
//!    `upsert_booking`'s "never overwrite raw_data" rule — and removed again
//!    (`-`) on resurrect. `rule_matched` itself is left untouched by both
//!    functions.
use std::collections::HashSet;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Minimal fields the poller has at upsert time.
#[derive(Debug, Clone)]
pub struct BookingUpsert {
    pub spx_id: String,
    pub status: String, // "pending" on first sight
    /// Not written by `upsert_booking` — `bookings.is_coc` is a Postgres
    /// GENERATED column computed from `spx_id`/`raw_data`. Kept on this
    /// struct for caller convenience/API shape only.
    pub is_coc: bool,
    pub raw_data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StaleOutcome {
    pub expired: u64,
    pub taken: u64,
}

/// Upsert a booking. On conflict: NEVER downgrade a non-pending status to
/// pending, and NEVER overwrite raw_data (enrichment must survive poll cycles).
pub async fn upsert_booking(
    pool: &PgPool,
    tenant_id: Uuid,
    b: &BookingUpsert,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query(
        "INSERT INTO bookings (id, tenant_id, spx_id, status, raw_data, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, now(), now()) \
         ON CONFLICT (tenant_id, spx_id) DO UPDATE SET \
           status = CASE WHEN bookings.status = 'pending' THEN EXCLUDED.status ELSE bookings.status END, \
           updated_at = now()",
    )
    .bind(tenant_id)
    .bind(&b.spx_id)
    .bind(&b.status)
    .bind(&b.raw_data)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Expire pending bookings no longer in SPX's active pool. Unknown/past deadline
/// → 'expired'; future deadline → 'taken_by_other'. Only touches 'pending'.
/// The reason is stamped into `raw_data->>'drift_reason'` (see module doc for
/// why `rule_matched`, a UUID FK, cannot hold it).
pub async fn expire_stale_bookings(
    pool: &PgPool,
    tenant_id: Uuid,
    active: &HashSet<String>,
) -> Result<StaleOutcome, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT spx_id, NULLIF(raw_data->>'bidding_ddl','')::bigint \
         FROM bookings WHERE tenant_id = $1 AND status = 'pending'",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut to_expire: Vec<String> = Vec::new();
    let mut to_taken: Vec<String> = Vec::new();
    for (spx_id, ddl_raw) in rows {
        if active.contains(&spx_id) {
            continue;
        }
        let ddl = ddl_raw.unwrap_or(0);
        let ddl_ms = if ddl > 0 {
            if ddl > 1_000_000_000_000 { ddl } else { ddl * 1000 }
        } else {
            0
        };
        // Unknown deadline → conservative 'expired' (don't falsely credit a rival).
        if ddl_ms == 0 || ddl_ms < now_ms {
            to_expire.push(spx_id);
        } else {
            to_taken.push(spx_id);
        }
    }

    if !to_expire.is_empty() {
        sqlx::query(
            "UPDATE bookings SET status='failed', \
               raw_data = raw_data || jsonb_build_object('drift_reason', 'expired'), \
               updated_at=now() \
             WHERE tenant_id=$1 AND status='pending' AND spx_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(&to_expire)
        .execute(&mut *tx)
        .await?;
    }
    if !to_taken.is_empty() {
        sqlx::query(
            "UPDATE bookings SET status='failed', \
               raw_data = raw_data || jsonb_build_object('drift_reason', 'taken_by_other'), \
               updated_at=now() \
             WHERE tenant_id=$1 AND status='pending' AND spx_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(&to_taken)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StaleOutcome { expired: to_expire.len() as u64, taken: to_taken.len() as u64 })
}

/// Inverse of expire: flip 'failed' rows we POSITIVELY see back to 'pending'.
/// NEVER touches 'accepted' (our own wins). Kills the "REG only 500" drift.
/// Clears `raw_data->>'drift_reason'` (added by `expire_stale_bookings`) so a
/// resurrected row carries no stale expiry/taken marker.
pub async fn resurrect_pending(
    pool: &PgPool,
    tenant_id: Uuid,
    spx_ids: &[String],
) -> Result<u64, sqlx::Error> {
    if spx_ids.is_empty() {
        return Ok(0);
    }
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let res = sqlx::query(
        "UPDATE bookings SET status='pending', \
           raw_data = raw_data - 'drift_reason', \
           updated_at=now() \
         WHERE tenant_id=$1 AND status='failed' AND spx_id = ANY($2)",
    )
    .bind(tenant_id)
    .bind(spx_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(res.rows_affected())
}
