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

use chrono::{TimeZone, Utc};
use serde_json::Value;
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

/// Optional filter conditions for `list_live`/`list_history` — the first dynamic-WHERE-clause
/// pattern in this crate. `status` is `&'static str` because callers must validate against the
/// real 3-value vocabulary BEFORE constructing this (see `api-gateway`'s `parse_status_filter`)
/// — this type intentionally cannot represent an invalid status, so validation can't be
/// forgotten at a call site. Every new field beyond the original 4 is `Option<T>`; `None` means
/// "no filter", same convention.
#[derive(Debug, Clone, Default)]
pub struct BookingFilter {
    pub status: Option<&'static str>,
    pub spx_id: Option<String>,
    /// Exact-or-prefix match on `spx_tx_id` (the "Booking Number" display column, GENERATED from
    /// `raw_data->>'booking_name'` with a fallback to `spx_id` — see migration
    /// `0021_bookings_spx_derived_columns.sql`). Same `LIKE`-prefix/escaped/bound convention as
    /// `spx_id` above, added for the `/tickets` "Nama Booking" search field.
    pub booking_name: Option<String>,
    /// Exact-or-prefix match on `spx_request_id` (GENERATED from `raw_data`'s `request_id`/
    /// `requestId`/`req_id` keys — see migration `0021_bookings_spx_derived_columns.sql`). Same
    /// `LIKE`-prefix/escaped/bound convention as `booking_name` above, added for the `/tickets`
    /// "ID Request" search field.
    pub request_id: Option<String>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub auto_accepted: Option<bool>,
    pub accept_reason: Option<String>,
    pub vehicle_type: Option<String>,
    pub trip_type: Option<i32>,
    pub booking_type: Option<BookingTypeFilter>,
    pub origin_station: Option<String>,
    pub dest_station: Option<String>,
    pub weight_min: Option<f64>,
    pub weight_max: Option<f64>,
    pub cod: Option<bool>,
    pub pickup_from: Option<chrono::DateTime<chrono::Utc>>,
    pub pickup_to: Option<chrono::DateTime<chrono::Utc>>,
    pub deadline_from: Option<chrono::DateTime<chrono::Utc>>,
    pub deadline_to: Option<chrono::DateTime<chrono::Utc>>,
    pub sort: SortKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookingTypeFilter {
    Coc,
    Reguler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    NewestFirst,
    DeadlineSoonest,
}

/// Appends every `Some` filter field in `f` to `qb` as `AND ...` clauses, then the `ORDER BY`.
/// Shared by `list_live` and `list_history` so the two endpoints' filter behavior can never drift
/// apart — this is the SINGLE place new filter dimensions get wired into SQL.
fn push_common_filters(qb: &mut QueryBuilder<sqlx::Postgres>, f: &BookingFilter) {
    if let Some(spx_id) = &f.spx_id {
        qb.push(" AND spx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(spx_id)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(booking_name) = &f.booking_name {
        qb.push(" AND spx_tx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(booking_name)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(request_id) = &f.request_id {
        qb.push(" AND spx_request_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(request_id)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(from) = f.from {
        qb.push(" AND created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = f.to {
        qb.push(" AND created_at <= ");
        qb.push_bind(to);
    }
    if let Some(auto_accepted) = f.auto_accepted {
        qb.push(" AND auto_accepted = ");
        qb.push_bind(auto_accepted);
    }
    if let Some(reason) = &f.accept_reason {
        // A loss to a rival agency is tagged one of two ways depending on which code path
        // caught it — `accept_reason` when the bot actively raced and lost (dispatch.rs),
        // `drift_reason` when the periodic reconciliation sweep found the booking silently
        // vanished from SPX's active pool with no rule ever matching it at all
        // (expire_stale_bookings). Same COALESCE priority as the frontend's
        // `failureReasonFromRaw` (`drift_reason ?? accept_reason`) — see also `summary()`
        // below, which must stay in lockstep with this filter.
        qb.push(" AND COALESCE(raw_data->>'accept_reason', raw_data->>'drift_reason') = ");
        qb.push_bind(reason.clone());
    }
    if let Some(vehicle_type) = &f.vehicle_type {
        qb.push(" AND spx_vehicle_type = ");
        qb.push_bind(vehicle_type.clone());
    }
    if let Some(trip_type) = f.trip_type {
        qb.push(" AND spx_trip_type = ");
        qb.push_bind(trip_type);
    }
    if let Some(booking_type) = f.booking_type {
        qb.push(" AND is_coc = ");
        qb.push_bind(matches!(booking_type, BookingTypeFilter::Coc));
    }
    if let Some(origin) = &f.origin_station {
        qb.push(" AND spx_origin_station = ");
        qb.push_bind(origin.clone());
    }
    if let Some(dest) = &f.dest_station {
        qb.push(" AND spx_dest_station = ");
        qb.push_bind(dest.clone());
    }
    if let Some(weight_min) = f.weight_min {
        qb.push(" AND weight >= ");
        qb.push_bind(weight_min);
    }
    if let Some(weight_max) = f.weight_max {
        qb.push(" AND weight <= ");
        qb.push_bind(weight_max);
    }
    if let Some(cod) = f.cod {
        if cod {
            qb.push(" AND cod_amount > 0");
        } else {
            qb.push(" AND cod_amount = 0");
        }
    }
    if let Some(pickup_from) = f.pickup_from {
        qb.push(" AND spx_pickup_time >= ");
        qb.push_bind(pickup_from);
    }
    if let Some(pickup_to) = f.pickup_to {
        qb.push(" AND spx_pickup_time <= ");
        qb.push_bind(pickup_to);
    }
    if let Some(deadline_from) = f.deadline_from {
        qb.push(" AND spx_deadline_at >= ");
        qb.push_bind(deadline_from);
    }
    if let Some(deadline_to) = f.deadline_to {
        qb.push(" AND spx_deadline_at <= ");
        qb.push_bind(deadline_to);
    }
    match f.sort {
        SortKey::NewestFirst => qb.push(" ORDER BY created_at DESC"),
        SortKey::DeadlineSoonest => qb.push(" ORDER BY spx_deadline_at ASC NULLS LAST"),
    };
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
         spx_request_id, spx_onsite_id, spx_tx_id, spx_vehicle_type, spx_deadline_at, \
         spx_pickup_time, spx_trip_type, \
         created_at, updated_at FROM bookings WHERE tenant_id = ",
    );
    qb.push_bind(tenant_id);
    qb.push(" AND status = ");
    qb.push_bind(filter.status.unwrap_or("pending"));
    push_common_filters(&mut qb, filter);
    qb.push(" LIMIT ");
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
         spx_request_id, spx_onsite_id, spx_tx_id, spx_vehicle_type, spx_deadline_at, \
         spx_pickup_time, spx_trip_type, \
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
    push_common_filters(&mut qb, filter);
    qb.push(" LIMIT ");
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
         spx_request_id, spx_onsite_id, spx_tx_id, spx_vehicle_type, spx_deadline_at, \
         spx_pickup_time, spx_trip_type, \
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
         spx_request_id, spx_onsite_id, spx_tx_id, spx_vehicle_type, spx_deadline_at, \
         spx_pickup_time, spx_trip_type, \
         created_at, updated_at FROM bookings WHERE tenant_id = $1 AND spx_id = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct BookingSummary {
    pub incoming_today: i64,
    pub accepted_auto_today: i64,
    pub accepted_manual_today: i64,
    pub taken_by_other_today: i64,
    pub latency_p99_ms: Option<f64>,
}

/// "Today" is fixed WIB (UTC+7, no DST) — matches spx_client::booking's format_times convention
/// (this codebase's only existing timezone precedent) and this design doc's own decision (no
/// per-tenant timezone column exists; TOWER is single-region).
fn wib_midnight_utc_today() -> chrono::DateTime<chrono::Utc> {
    let wib = chrono::FixedOffset::east_opt(7 * 3600).expect("valid +7 offset");
    let now_wib = Utc::now().with_timezone(&wib);
    let midnight_wib = now_wib
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("valid midnight");
    wib.from_local_datetime(&midnight_wib)
        .single()
        .expect("unambiguous")
        .with_timezone(&Utc)
}

/// `/bookings/summary`: today's (WIB) counters for the dashboard header — incoming, auto/manual
/// accepted, taken-by-other, and the p99 accept latency among today's auto-accepts.
pub async fn summary(pool: &PgPool, tenant_id: Uuid) -> Result<BookingSummary, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let today_start = wib_midnight_utc_today();
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, Option<f64>)>(
        "SELECT \
            COUNT(*) FILTER (WHERE created_at >= $2), \
            COUNT(*) FILTER (WHERE created_at >= $2 AND status = 'accepted' AND auto_accepted = true), \
            COUNT(*) FILTER (WHERE created_at >= $2 AND status = 'accepted' AND auto_accepted = false), \
            COUNT(*) FILTER (WHERE created_at >= $2 AND status = 'failed' AND COALESCE(raw_data->>'accept_reason', raw_data->>'drift_reason') = 'taken_by_other'), \
            percentile_cont(0.99) WITHIN GROUP (ORDER BY accept_latency_ms) \
                FILTER (WHERE created_at >= $2 AND auto_accepted = true AND accept_latency_ms IS NOT NULL) \
         FROM bookings WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .bind(today_start)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(BookingSummary {
        incoming_today: row.0,
        accepted_auto_today: row.1,
        accepted_manual_today: row.2,
        taken_by_other_today: row.3,
        latency_p99_ms: row.4,
    })
}

/// Distinct, non-null vehicle types seen for this tenant — backs the vehicle-type filter
/// dropdown (no separate lookup table exists; `spx_vehicle_type` is a generated column derived
/// from each booking's own `raw_data`, per Task 1).
pub async fn list_vehicle_types(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let types: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT spx_vehicle_type FROM bookings \
         WHERE tenant_id = $1 AND spx_vehicle_type IS NOT NULL ORDER BY spx_vehicle_type",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(types)
}
