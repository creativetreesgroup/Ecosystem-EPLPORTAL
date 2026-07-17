// Backend/crates/store/src/bookings.rs
//! Booking lifecycle writes for the poller: upsert (enrichment-preserving) +
//! the two anti-drift transitions. Ported from spx-portal-ref db.ts
//! (upsertBooking, expireStaleBookings, resurrectPending). No schema change.
//! Also home to the three read queries backing the manual-accept routes —
//! `list_live` (`/bookings/live`), `list_history` (`/bookings/history`), and
//! `get_detail` (`/bookings/:id/detail`) — added in Fase 6c.
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
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

/// Optional filter conditions for `list_live`/`list_history` — the first dynamic-WHERE-clause
/// pattern in this crate. `status` is `&'static str` because callers must validate against the
/// real 3-value vocabulary BEFORE constructing this (see `api-gateway`'s `parse_status_filter`)
/// — this type intentionally cannot represent an invalid status, so validation can't be
/// forgotten at a call site.
#[derive(Debug, Default, Clone)]
pub struct BookingFilter {
    pub status: Option<&'static str>,
    pub spx_id: Option<String>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// Escapes `%`/`_`/`\` in a caller-supplied search term before it's embedded in a `LIKE`
/// pattern — without this, a user searching for a literal `%` or `_` in an spx_id would get
/// unintended wildcard matches. Not a SQL-injection concern (the value is still `push_bind`-ed,
/// never string-interpolated into the query) — this is purely about `LIKE` semantics.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Minimal fields the poller has at upsert time.
#[derive(Debug, Clone)]
pub struct BookingUpsert {
    pub account_id: String,
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
        "INSERT INTO bookings (id, tenant_id, account_id, spx_id, status, raw_data, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, now(), now()) \
         ON CONFLICT (tenant_id, account_id, spx_id) DO UPDATE SET \
           status = CASE WHEN bookings.status = 'pending' THEN EXCLUDED.status ELSE bookings.status END, \
           updated_at = now()",
    )
    .bind(tenant_id)
    .bind(&b.account_id)
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
            if ddl > 1_000_000_000_000 {
                ddl
            } else {
                ddl * 1000
            }
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
    Ok(StaleOutcome {
        expired: to_expire.len() as u64,
        taken: to_taken.len() as u64,
    })
}

/// Record the terminal outcome of an accept attempt on a booking (the accept
/// dispatch pipeline's write-back — Fase 5 Task 6).
///
/// Two type corrections vs. the Task 6 brief's initial transcription (same bug
/// class Task 5 already hit once on this table — see the module doc above):
/// 1. `rule_matched` is `UUID REFERENCES accept_rules(id) ON DELETE SET NULL`,
///    NOT text — `rule_matched` MUST be the winning rule's real `Uuid` (or
///    `None`), never a rule NAME or a reason string like `"taken_by_other"`.
/// 2. `accept_latency_ms` is `INT` (Postgres int4 = Rust `i32`), not `i64` — a
///    booking's own accept latency in milliseconds will never remotely
///    approach i32's ~2.1 billion range, so the narrower type is safe.
///
/// A sub-classification reason that is NOT a rule uuid (e.g. `"taken_by_other"`
/// for a `Taken`/agency-dup-loss outcome) cannot be squeezed into the
/// `rule_matched` FK column either, so — mirroring `expire_stale_bookings`'s
/// `drift_reason` pattern — it is merged additively into
/// `raw_data->>'accept_reason'` via `||` (never clobbers other `raw_data`
/// keys) when `accept_reason` is `Some`.
///
/// Bundled into a struct (rather than more positional args) both to dodge
/// `clippy::too_many_arguments` and — more importantly — because several
/// fields here are same-typed `Option`/`bool` values where a positional
/// call site is a real transposition hazard (e.g. swapping `auto_accepted`
/// and an `Option<&str>` would not be caught by the type checker at a couple
/// of these call sites).
#[derive(Debug, Clone, Copy)]
pub struct BookingStatusUpdate<'a> {
    pub status: &'a str,
    pub latency_ms: Option<i32>,
    pub auto_accepted: bool,
    pub rule_matched: Option<Uuid>,
    pub accept_reason: Option<&'a str>,
}

pub async fn update_booking_status(
    pool: &PgPool,
    tenant_id: Uuid,
    spx_id: &str,
    update: BookingStatusUpdate<'_>,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    match update.accept_reason {
        Some(reason) => {
            sqlx::query(
                "UPDATE bookings SET status=$3, accept_latency_ms=$4, auto_accepted=$5, \
                 rule_matched=$6, \
                 raw_data = raw_data || jsonb_build_object('accept_reason', $7::text), \
                 updated_at=now() WHERE tenant_id=$1 AND spx_id=$2",
            )
            .bind(tenant_id)
            .bind(spx_id)
            .bind(update.status)
            .bind(update.latency_ms)
            .bind(update.auto_accepted)
            .bind(update.rule_matched)
            .bind(reason)
            .execute(&mut *tx)
            .await?;
        }
        None => {
            sqlx::query(
                "UPDATE bookings SET status=$3, accept_latency_ms=$4, auto_accepted=$5, \
                 rule_matched=$6, updated_at=now() WHERE tenant_id=$1 AND spx_id=$2",
            )
            .bind(tenant_id)
            .bind(spx_id)
            .bind(update.status)
            .bind(update.latency_ms)
            .bind(update.auto_accepted)
            .bind(update.rule_matched)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
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

/// `/bookings/live`: pending bookings by default (or `filter.status` if set), newest first.
/// Uses the `idx_bookings_live_covering` index for the (typical) unfiltered/status-only case;
/// `spx_id`/date-range filters add extra predicates the planner evaluates after that index scan
/// (no additional index exists for those — acceptable at this table's expected volume, per the
/// design doc's "Parity dulu, optimasi kedua" scoping).
/// `limit`/`offset` are the caller's job to clamp to a sane range (the route layer does this).
pub async fn list_live(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
    filter: &BookingFilter,
) -> Result<Vec<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let mut qb = QueryBuilder::new(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings WHERE tenant_id = ",
    );
    qb.push_bind(tenant_id);
    qb.push(" AND status = ");
    qb.push_bind(filter.status.unwrap_or("pending"));
    if let Some(spx_id) = &filter.spx_id {
        qb.push(" AND spx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(spx_id)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(from) = filter.from {
        qb.push(" AND created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = filter.to {
        qb.push(" AND created_at <= ");
        qb.push_bind(to);
    }
    qb.push(" ORDER BY created_at DESC LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);
    let rows = qb
        .build_query_as::<crate::models::Booking>()
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(rows)
}

/// `/bookings/history`: terminal bookings (`accepted`/`failed` by default, or narrowed to just
/// `filter.status` if set), newest first. Uses the `idx_bookings_created_brin` BRIN index for
/// the time-ordered scan; same filter-cost caveat as `list_live` for `spx_id`/date-range.
pub async fn list_history(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
    filter: &BookingFilter,
) -> Result<Vec<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let mut qb = QueryBuilder::new(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings WHERE tenant_id = ",
    );
    qb.push_bind(tenant_id);
    match filter.status {
        Some(status) => {
            qb.push(" AND status = ");
            qb.push_bind(status);
        }
        None => {
            qb.push(" AND status IN ('accepted', 'failed')");
        }
    }
    if let Some(spx_id) = &filter.spx_id {
        qb.push(" AND spx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(spx_id)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(from) = filter.from {
        qb.push(" AND created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = filter.to {
        qb.push(" AND created_at <= ");
        qb.push_bind(to);
    }
    qb.push(" ORDER BY created_at DESC LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);
    let rows = qb
        .build_query_as::<crate::models::Booking>()
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(rows)
}

/// `/bookings/:id/detail`: single row by its own `id` (not `spx_id` — the route's `:id` path
/// param is the DB primary key, matching every other `/:id/...` route in this crate).
pub async fn get_detail(
    pool: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, crate::models::Booking>(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Looks up a booking by its SPX platform id (`spx_id`) rather than the internal UUID row id —
/// what a quick-accept HMAC token carries (Fase 6e), since neither the WhatsApp notification nor
/// the token format ever carries TOWER's internal row id. Tenant-scoped exactly like
/// `get_detail` (same return type, same `begin_tenant_tx` + `WHERE tenant_id = $1` pattern).
///
/// `(tenant_id, account_id, spx_id)` is UNIQUE per migration 0020's
/// `bookings_tenant_account_spx_id_unique` constraint, but `(tenant_id, spx_id)` alone is NOT —
/// the same `spx_id` can legitimately exist under two different sibling accounts within one
/// tenant (that's the entire reason migration 0020 replaced the old, narrower
/// `bookings_tenant_spx_id_unique` constraint). `LIMIT 1` with no `ORDER BY` is therefore an
/// intentional, accepted simplification for this specific use case: a WhatsApp quick-accept link
/// is fired for one specific account's ticket at send-time, so whichever row Postgres returns
/// first is a correct answer for the common case. If a same-`spx_id`-across-accounts collision
/// ever becomes a real ambiguity for a caller, that caller needs a variant that also takes
/// `account_id` — this function intentionally does not attempt that disambiguation itself.
pub async fn get_by_spx_id(
    pool: &PgPool,
    tenant_id: Uuid,
    spx_id: &str,
) -> Result<Option<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, crate::models::Booking>(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings WHERE tenant_id = $1 AND spx_id = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}
