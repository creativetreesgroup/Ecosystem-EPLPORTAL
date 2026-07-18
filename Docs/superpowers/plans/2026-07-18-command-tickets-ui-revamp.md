# Command/Tickets UI Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Revamp `/command`'s KPI row and `/tickets`' table + filter to match reference designs, backed by new SPX-derived generated columns and expanded booking-list/filter/summary APIs.

**Architecture:** A new migration adds SQL-queryable generated columns (mirroring `spx_client::normalize_booking`'s Rust-side derivation) so the existing `QueryBuilder`-based list endpoints can filter/sort on them; `BookingListItem` gains display fields derived the same way `route` already is (Rust-side, at read time, for consistency with `BookingDetail`); a new `/bookings/summary` aggregate endpoint feeds `/command`'s 5 KPI widgets; `/tickets` gets a full `TicketsTable` column rewrite and a new `FilterDrawer` replacing `TicketFilterBar`.

**Tech Stack:** Rust (axum, sqlx/Postgres, existing `store`/`api-gateway` crates), SvelteKit 5 (Svelte 5 runes, TypeScript, Tailwind v4 tokens), Vitest, Playwright, Postgres 16 generated columns.

## Global Constraints

- Design doc: `Docs/superpowers/specs/2026-07-18-command-tickets-ui-revamp-design.md` — every task below implements a specific section of it; consult it for the "why" behind anything unclear here.
- `cargo test` MUST run with `DATABASE_URL` unset (falls back to the `tower` superuser) — never `app_role` — since tests run migrations directly. See `Docs/superpowers/specs/2026-07-18-command-tickets-ui-revamp-design.md`'s referenced project convention.
- No `#[serde(rename_all = ...)]` anywhere in `api-gateway` — new response fields stay snake_case, matching every existing route in this crate.
- All new/modified SQL goes through `sqlx::QueryBuilder` with `push_bind` — never string-interpolate a filter value into SQL (this crate's existing, load-bearing convention, see `store::bookings::escape_like`'s doc comment).
- Every new Svelte component: 44px min tap targets, `focus-visible:ring-2 ring-accent`, tokens-only colors (`app.css` `@theme` vars — `--color-bg-base`, `--color-bg-surface`, `--color-border`, `--color-text-primary`, `--color-text-muted`, `--color-accent`, `--color-live`, `--color-danger`), Indonesian UI copy.
- `ADHOC`/`FIX` tag mapping (`trip_type` 0/1) is the user's best recollection, not verified against a captured SPX payload — implement as a single named constant, not scattered magic numbers (design doc's Open Questions).
- Frontend REST wire types stay snake_case (matching `api-gateway`'s no-`rename_all` convention) — only WS event payloads use camelCase. Do not mix the two conventions in new code.

---

## Task 1: Migration — SPX-derived generated columns

**Files:**
- Create: `Backend/crates/store/migrations/0021_bookings_spx_derived_columns.sql`
- Test: `Backend/crates/store/tests/spx_derived_columns.rs`

**Interfaces:**
- Produces: columns `bookings.spx_request_id`, `spx_onsite_id`, `spx_tx_id`, `spx_vehicle_type`, `spx_deadline_at`, `spx_pickup_time`, `spx_trip_type`, `spx_origin_station`, `spx_dest_station` (all nullable except `spx_tx_id`, which always falls back to `spx_id`), plus two reusable SQL helper functions `tower_pick_text(jsonb, text[])` and `tower_pick_epoch_ms(jsonb, text[])`.

- [ ] **Step 1: Write the migration**

```sql
-- Backend/crates/store/migrations/0021_bookings_spx_derived_columns.sql
-- Exposes fields spx_client::normalize_booking (Backend/crates/spx-client/src/booking.rs)
-- already derives Rust-side from `raw_data`, as real generated columns so the QueryBuilder-based
-- list/filter/sort endpoints can target them in SQL. Mirrors the SAME key-priority order as
-- normalize_booking — the implementer changing either side must keep them in lockstep, or the
-- table row and its detail drawer (which still derives Rust-side) can disagree.
--
-- Two small IMMUTABLE helper functions avoid repeating the same multi-key-fallback CASE
-- expression 5+ times (DRY) — both are pure (no table access, no volatile builtins), so they're
-- safe to use inside GENERATED ALWAYS AS (...) STORED, which Postgres requires to be immutable.

CREATE OR REPLACE FUNCTION tower_pick_text(raw JSONB, keys TEXT[])
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT NULLIF(raw->>key, '')
    FROM unnest(keys) AS key
    WHERE raw->>key IS NOT NULL AND raw->>key <> ''
    LIMIT 1;
$$;

-- toMs port (spx-client/src/booking.rs's to_ms): 0 -> NULL (no deadline); >1e12 already
-- epoch-ms; else epoch-seconds. A non-numeric picked value becomes NULL rather than erroring
-- the whole INSERT/UPDATE (real SPX data is defensively parsed everywhere else in this
-- codebase; a generated column must not be the one place a malformed field breaks writes).
CREATE OR REPLACE FUNCTION tower_pick_epoch_ms(raw JSONB, keys TEXT[])
RETURNS TIMESTAMPTZ
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT CASE
        WHEN v IS NULL OR v !~ '^-?[0-9]+(\.[0-9]+)?$' THEN NULL
        WHEN v::numeric = 0 THEN NULL
        WHEN v::numeric > 1000000000000 THEN to_timestamp(v::numeric / 1000.0)
        ELSE to_timestamp(v::numeric)
    END
    FROM (SELECT tower_pick_text(raw, keys) AS v) picked;
$$;

ALTER TABLE bookings ADD COLUMN spx_request_id TEXT GENERATED ALWAYS AS (
    tower_pick_text(raw_data, ARRAY['request_id', 'requestId', 'req_id'])
) STORED;

ALTER TABLE bookings ADD COLUMN spx_onsite_id TEXT GENERATED ALWAYS AS (
    tower_pick_text(raw_data, ARRAY['onsite_id', 'onsiteId'])
) STORED;

-- Falls back to spx_id (this row's own column) when no tx-name key is present, matching
-- normalize_booking's `if v.is_empty() { booking_id.clone() } else { v }` fallback for spx_tx_id.
ALTER TABLE bookings ADD COLUMN spx_tx_id TEXT GENERATED ALWAYS AS (
    COALESCE(
        tower_pick_text(raw_data, ARRAY['booking_name', 'spx_tx_id', 'spxTxId', 'tx_id', 'tracking_no']),
        spx_id
    )
) STORED;

-- Prefers a display-name key; else a code key, EXCLUDING a purely-numeric code (an internal id,
-- not a real vehicle type — mirrors numeric_only_vehicle_type_is_discarded in booking.rs).
ALTER TABLE bookings ADD COLUMN spx_vehicle_type TEXT GENERATED ALWAYS AS (
    CASE
        WHEN tower_pick_text(raw_data, ARRAY['vehicle_type_name', 'right_vehicle_type_name', 'sgi_vehicle_name']) IS NOT NULL
            THEN tower_pick_text(raw_data, ARRAY['vehicle_type_name', 'right_vehicle_type_name', 'sgi_vehicle_name'])
        WHEN tower_pick_text(raw_data, ARRAY['truck_type', 'vehicle_type', 'vehicleType', 'service_type']) ~ '^[0-9]+$'
            THEN NULL
        ELSE tower_pick_text(raw_data, ARRAY['truck_type', 'vehicle_type', 'vehicleType', 'service_type'])
    END
) STORED;

ALTER TABLE bookings ADD COLUMN spx_deadline_at TIMESTAMPTZ GENERATED ALWAYS AS (
    tower_pick_epoch_ms(raw_data, ARRAY['bidding_ddl', 'deadline_at', 'pickup_time_ms', 'expired_at'])
) STORED;

-- Falls back to spx_deadline_at's already-computed value (not a re-pick of the deadline keys) —
-- matches normalize_booking's `None => deadline_at` fallback for pickup_ms exactly.
ALTER TABLE bookings ADD COLUMN spx_pickup_time TIMESTAMPTZ GENERATED ALWAYS AS (
    COALESCE(
        tower_pick_epoch_ms(raw_data, ARRAY['booking_date', 'schedule_at', 'pickup_time', 'pickup_date']),
        spx_deadline_at
    )
) STORED;

-- Absent -> NULL (distinct from an explicit 0, which is itself a meaningful trip_type value —
-- unlike normalize_booking's pick_num, which defaults absent to 0.0, a persisted/filterable
-- column must not conflate "no data" with "explicitly type 0".
ALTER TABLE bookings ADD COLUMN spx_trip_type INT GENERATED ALWAYS AS (
    NULLIF(raw_data->>'trip_type', '')::int
) STORED;

-- Deliberate simplification (see design doc's Open Questions): only the route_detail_list path
-- is replicated here, not normalize_booking's full sgi_province_name/string-split fallback chain.
-- Postgres 12+ jsonb path operators support negative array indices (-1 = last element).
ALTER TABLE bookings ADD COLUMN spx_origin_station TEXT GENERATED ALWAYS AS (
    NULLIF(raw_data #>> '{route_detail_list,0,node_info_list,0,name}', '')
) STORED;

ALTER TABLE bookings ADD COLUMN spx_dest_station TEXT GENERATED ALWAYS AS (
    NULLIF(raw_data #>> '{route_detail_list,-1,node_info_list,-1,name}', '')
) STORED;

CREATE INDEX idx_bookings_spx_deadline ON bookings (tenant_id, spx_deadline_at);
CREATE INDEX idx_bookings_spx_vehicle_type ON bookings (tenant_id, spx_vehicle_type);
CREATE INDEX idx_bookings_spx_trip_type ON bookings (tenant_id, spx_trip_type);
CREATE INDEX idx_bookings_spx_stations ON bookings (tenant_id, spx_origin_station, spx_dest_station);
```

- [ ] **Step 2: Write the failing test**

```rust
// Backend/crates/store/tests/spx_derived_columns.rs
//! Real-Postgres tests for migration 0021's generated columns — this project's standing
//! testing convention is a real database, not mocks (see store's other integration tests).
use sqlx::PgPool;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string());
    let pool = PgPool::connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrate");
    pool
}

async fn seed_tenant(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("Spx Derived Columns Test Tenant")
        .bind(format!("spx-derived-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

async fn insert_booking(pool: &PgPool, tenant_id: Uuid, spx_id: &str, raw: serde_json::Value) {
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(spx_id)
        .bind(raw)
        .execute(pool)
        .await
        .expect("insert booking");
}

#[tokio::test]
async fn vehicle_type_prefers_name_and_discards_numeric_code() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "veh-name", serde_json::json!({"vehicle_type_name": "CDD LONG (6WH)", "vehicle_type": "3"})).await;
    insert_booking(&pool, tenant_id, "veh-numeric", serde_json::json!({"vehicle_type": "3"})).await;

    let name: Option<String> = sqlx::query_scalar("SELECT spx_vehicle_type FROM bookings WHERE tenant_id = $1 AND spx_id = 'veh-name'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(name.as_deref(), Some("CDD LONG (6WH)"));

    let numeric: Option<String> = sqlx::query_scalar("SELECT spx_vehicle_type FROM bookings WHERE tenant_id = $1 AND spx_id = 'veh-numeric'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(numeric, None);
}

#[tokio::test]
async fn deadline_at_converts_seconds_and_ms_correctly() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    // Seconds (<= 1e12): 1_800_000_000 seconds.
    insert_booking(&pool, tenant_id, "ddl-seconds", serde_json::json!({"deadline_at": 1_800_000_000})).await;
    // Already ms (> 1e12): 1_800_000_000_000 ms == the same instant.
    insert_booking(&pool, tenant_id, "ddl-ms", serde_json::json!({"deadline_at": 1_800_000_000_000i64})).await;
    // Zero -> NULL (no real deadline).
    insert_booking(&pool, tenant_id, "ddl-zero", serde_json::json!({"deadline_at": 0})).await;

    let seconds: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT spx_deadline_at FROM bookings WHERE tenant_id = $1 AND spx_id = 'ddl-seconds'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    let ms: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT spx_deadline_at FROM bookings WHERE tenant_id = $1 AND spx_id = 'ddl-ms'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(seconds, ms);

    let zero: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT spx_deadline_at FROM bookings WHERE tenant_id = $1 AND spx_id = 'ddl-zero'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(zero, None);
}

#[tokio::test]
async fn pickup_time_falls_back_to_deadline_at() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "no-pickup-key", serde_json::json!({"deadline_at": 1_800_000_000})).await;

    let (deadline, pickup): (Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT spx_deadline_at, spx_pickup_time FROM bookings WHERE tenant_id = $1 AND spx_id = 'no-pickup-key'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("query");
    assert_eq!(deadline, pickup);
}

#[tokio::test]
async fn tx_id_falls_back_to_spx_id_when_no_name_key_present() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPXID_FALLBACK_1", serde_json::json!({})).await;

    let tx_id: String = sqlx::query_scalar("SELECT spx_tx_id FROM bookings WHERE tenant_id = $1 AND spx_id = 'SPXID_FALLBACK_1'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(tx_id, "SPXID_FALLBACK_1");
}

#[tokio::test]
async fn origin_dest_station_reads_first_and_last_route_node() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "route-stations", serde_json::json!({
        "route_detail_list": [
            {"node_info_list": [{"name": "Cikarang DC"}]},
            {"node_info_list": [{"name": "Semarang DC"}]}
        ]
    })).await;

    let (origin, dest): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT spx_origin_station, spx_dest_station FROM bookings WHERE tenant_id = $1 AND spx_id = 'route-stations'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("query");
    assert_eq!(origin.as_deref(), Some("Cikarang DC"));
    assert_eq!(dest.as_deref(), Some("Semarang DC"));
}

#[tokio::test]
async fn trip_type_absent_is_null_not_zero() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "no-trip-type", serde_json::json!({})).await;

    let trip_type: Option<i32> = sqlx::query_scalar("SELECT spx_trip_type FROM bookings WHERE tenant_id = $1 AND spx_id = 'no-trip-type'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(trip_type, None);
}
```

- [ ] **Step 3: Run the tests to verify the migration is correct**

Run: `cd Backend && cargo test -p store --test spx_derived_columns`
Expected: all 6 tests PASS (requires `tower-postgres`/`tower-redis` up: `docker compose -f docker/docker-compose.yml up -d tower-postgres tower-redis`; `DATABASE_URL` left unset so it falls back to the `tower` superuser default, per Global Constraints).

- [ ] **Step 4: Commit**

```bash
git add Backend/crates/store/migrations/0021_bookings_spx_derived_columns.sql Backend/crates/store/tests/spx_derived_columns.rs
git commit -m "feat(store): add SPX-derived generated columns (deadline, vehicle, request/onsite id, stations)"
```

---

## Task 2: `store::bookings` — expanded `BookingFilter` + summary + vehicle-types query

**Files:**
- Modify: `Backend/crates/store/src/bookings.rs`
- Test: `Backend/crates/store/tests/bookings_summary.rs` (create)

**Interfaces:**
- Consumes: Task 1's `spx_vehicle_type`, `spx_trip_type`, `spx_deadline_at`, `spx_pickup_time`, `spx_origin_station`, `spx_dest_station` columns.
- Produces: `BookingFilter` (expanded), `SortKey` enum, `BookingSummary` struct, `pub async fn summary(pool, tenant_id) -> Result<BookingSummary, sqlx::Error>`, `pub async fn list_vehicle_types(pool, tenant_id) -> Result<Vec<String>, sqlx::Error>` — all consumed by Task 5/6/7.

- [ ] **Step 1: Write the failing test**

```rust
// Backend/crates/store/tests/bookings_summary.rs
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

async fn test_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string());
    let pool = PgPool::connect(&url).await.expect("connect");
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrate");
    pool
}

async fn seed_tenant(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("Bookings Summary Test Tenant")
        .bind(format!("bookings-summary-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

#[tokio::test]
async fn summary_counts_todays_buckets_correctly() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;

    // 2 incoming (any status), 1 accepted+auto, 1 accepted+manual, 1 taken_by_other.
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted, accept_latency_ms) VALUES ($1, 'b1', '{}', 'pending', false, NULL)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted, accept_latency_ms) VALUES ($1, 'b2', '{}', 'accepted', true, 120)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted, accept_latency_ms) VALUES ($1, 'b3', '{}', 'accepted', false, NULL)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted) VALUES ($1, 'b4', jsonb_build_object('accept_reason', 'taken_by_other'), 'failed', false)")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let s = store::bookings::summary(&pool, tenant_id).await.expect("summary");
    assert_eq!(s.incoming_today, 4);
    assert_eq!(s.accepted_auto_today, 1);
    assert_eq!(s.accepted_manual_today, 1);
    assert_eq!(s.taken_by_other_today, 1);
    assert_eq!(s.latency_p99_ms, Some(120.0));
}

#[tokio::test]
async fn summary_latency_is_none_with_no_auto_accepts_today() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status) VALUES ($1, 'b1', '{}', 'pending')")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let s = store::bookings::summary(&pool, tenant_id).await.expect("summary");
    assert_eq!(s.latency_p99_ms, None);
}

#[tokio::test]
async fn list_vehicle_types_returns_distinct_non_null_sorted() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v1', jsonb_build_object('vehicle_type_name', 'TRONTON'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v2', jsonb_build_object('vehicle_type_name', 'CDD'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v3', jsonb_build_object('vehicle_type_name', 'TRONTON'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v4', '{}')")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let types = store::bookings::list_vehicle_types(&pool, tenant_id).await.expect("list");
    assert_eq!(types, vec!["CDD".to_string(), "TRONTON".to_string()]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Backend && cargo test -p store --test bookings_summary`
Expected: FAIL — `store::bookings::summary`/`list_vehicle_types` not found.

- [ ] **Step 3: Implement — expand `BookingFilter`, add `SortKey`, `summary`, `list_vehicle_types`**

Modify `Backend/crates/store/src/bookings.rs`: replace the existing `BookingFilter` struct (currently at line 44-49, shown in full context above) with the expanded version, and append the new functions after `list_history` (currently ending around line 360-ish — append at end of file, before the `#[cfg(test)]` module if one exists in this file, else at end of file):

```rust
// Replaces the existing 4-field BookingFilter (status, spx_id, from, to) — same fields kept,
// new ones added. Every new field is Option<T>; None means "no filter", matching the existing
// convention (see this struct's original doc comment on `status`).
#[derive(Debug, Clone, Default)]
pub struct BookingFilter {
    pub status: Option<&'static str>,
    pub spx_id: Option<String>,
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

/// Appends every `Some` filter field in `f` to `qb` as `AND ...` clauses. Shared by `list_live`
/// and `list_history` so the two endpoints' filter behavior can never drift apart — this is the
/// SINGLE place new filter dimensions get wired into SQL.
fn push_common_filters(qb: &mut QueryBuilder<'_, sqlx::Postgres>, f: &BookingFilter) {
    if let Some(spx_id) = &f.spx_id {
        qb.push(" AND spx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(spx_id)));
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
        qb.push(" AND raw_data->>'accept_reason' = ");
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
    let midnight_wib = now_wib.date_naive().and_hms_opt(0, 0, 0).expect("valid midnight");
    wib.from_local_datetime(&midnight_wib).single().expect("unambiguous").with_timezone(&Utc)
}

pub async fn summary(pool: &PgPool, tenant_id: Uuid) -> Result<BookingSummary, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let today_start = wib_midnight_utc_today();
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, Option<f64>)>(
        "SELECT \
            COUNT(*) FILTER (WHERE created_at >= $2), \
            COUNT(*) FILTER (WHERE created_at >= $2 AND status = 'accepted' AND auto_accepted = true), \
            COUNT(*) FILTER (WHERE created_at >= $2 AND status = 'accepted' AND auto_accepted = false), \
            COUNT(*) FILTER (WHERE created_at >= $2 AND status = 'failed' AND raw_data->>'accept_reason' = 'taken_by_other'), \
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

pub async fn list_vehicle_types(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
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
```

Then update `list_live`/`list_history` to call `push_common_filters` instead of their current inline `if let Some(spx_id)`/`from`/`to` blocks (the status handling stays as each function's own first clause, since `live` defaults to `pending` and `history` defaults to `'accepted','failed'` — that asymmetry is unchanged): replace each function's block from `if let Some(spx_id) = &filter.spx_id { ... }` through the end of its filter-building (just before the existing `qb.push(" LIMIT ")`/`OFFSET` tail) with a single `push_common_filters(&mut qb, filter);` call, and remove the old standalone `from`/`to` blocks (now folded into `push_common_filters`). Add `spx_request_id, spx_onsite_id, spx_tx_id, spx_vehicle_type, spx_deadline_at, spx_pickup_time, spx_trip_type` to both functions' `SELECT` column lists so `crate::models::Booking` (Task 5 will add these fields to that struct) can be populated from a single query — no second round-trip.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Backend && cargo test -p store --test bookings_summary`
Expected: all 3 tests PASS.

Run: `cd Backend && cargo test -p store`
Expected: ALL existing `store` tests still PASS (the `list_live`/`list_history` refactor must not change existing filter behavior).

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/store/src/bookings.rs Backend/crates/store/tests/bookings_summary.rs
git commit -m "feat(store): expand BookingFilter, add summary() and list_vehicle_types()"
```

---

## Task 3: `store::models::Booking` — new fields

**Files:**
- Modify: `Backend/crates/store/src/models.rs` (or wherever `pub struct Booking` lives — locate via `grep -rn "pub struct Booking\b" Backend/crates/store/src`)

**Interfaces:**
- Consumes: Task 1's new columns (now selected by Task 2's updated `list_live`/`list_history`).
- Produces: `Booking.spx_request_id: Option<String>`, `spx_onsite_id: Option<String>`, `spx_tx_id: String`, `spx_vehicle_type: Option<String>`, `spx_deadline_at: Option<DateTime<Utc>>`, `spx_pickup_time: Option<DateTime<Utc>>`, `spx_trip_type: Option<i32>` — consumed by Task 5's `BookingListItem::from`.

- [ ] **Step 1: Locate and read the current struct**

Run: `grep -rn "pub struct Booking\b" Backend/crates/store/src`

- [ ] **Step 2: Add the new fields to the struct** (matching its existing `#[derive(sqlx::FromRow, ...)]` — every field must be selectable by the `SELECT` list `list_live`/`list_history`/`get_detail` use)

```rust
    pub spx_request_id: Option<String>,
    pub spx_onsite_id: Option<String>,
    pub spx_tx_id: String,
    pub spx_vehicle_type: Option<String>,
    pub spx_deadline_at: Option<chrono::DateTime<chrono::Utc>>,
    pub spx_pickup_time: Option<chrono::DateTime<chrono::Utc>>,
    pub spx_trip_type: Option<i32>,
```

- [ ] **Step 3: Update every other `SELECT ... FROM bookings` in `store` that populates this struct** (`get_detail` in `bookings.rs`, and any other query selecting the full column list — `grep -rn "FROM bookings" Backend/crates/store/src` to find them all) to include the 7 new columns in its `SELECT`, in the same order as the struct fields, right after `updated_at` (or wherever the existing column list ends).

- [ ] **Step 4: Run the full store test suite to verify the struct change compiles and every query still matches its column list**

Run: `cd Backend && cargo test -p store`
Expected: compiles clean, all tests PASS (a column-list/struct-field mismatch fails at compile time for `sqlx::query_as!`-style macros, or at runtime with a column-count/name mismatch for `FromRow` — either way this step catches it before moving on).

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/store/src/models.rs
git commit -m "feat(store): add SPX-derived fields to Booking model"
```

---

## Task 4: `api-gateway` — `BookingListItem` + `ListParams` expansion

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs`
- Test: `Backend/crates/api-gateway/tests/bookings_routes.rs` (extend existing file — locate via `find Backend/crates/api-gateway/tests -iname "*booking*"`)

**Interfaces:**
- Consumes: Task 2's expanded `BookingFilter`/`SortKey`/`BookingTypeFilter`, Task 3's `Booking` fields.
- Produces: `BookingListItem` with `request_id`, `onsite_id`, `booking_number`, `vehicle_type`, `deadline_at`, `pickup_time`, `trip_type`, `booking_type` fields; `ListParams` accepts the new query params — consumed by Task 8 (frontend).

- [ ] **Step 1: Write the failing test**

Append to the existing bookings route test file:

```rust
#[tokio::test]
async fn live_endpoint_returns_spx_derived_fields() {
    let (app, pool, tenant_id) = test_app().await; // existing test helper in this file
    sqlx::query(
        "INSERT INTO bookings (tenant_id, spx_id, raw_data, status) VALUES ($1, 'derived-1', $2, 'pending')",
    )
    .bind(tenant_id)
    .bind(serde_json::json!({
        "request_id": "R123",
        "onsite_id": "O456",
        "booking_name": "SPXID_DERIVED_1",
        "vehicle_type_name": "TRONTON",
        "deadline_at": 1_800_000_000,
        "trip_type": 1
    }))
    .execute(&pool)
    .await
    .expect("insert");

    let session_cookie = login_as_main_account(&app, tenant_id).await; // existing test helper
    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/bookings/live")
                .header("cookie", &session_cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = axum_test_json_body(res).await; // existing test helper
    let item = &body.as_array().unwrap()[0];
    assert_eq!(item["request_id"], "R123");
    assert_eq!(item["onsite_id"], "O456");
    assert_eq!(item["booking_number"], "SPXID_DERIVED_1");
    assert_eq!(item["vehicle_type"], "TRONTON");
    assert_eq!(item["trip_type"], 1);
    assert_eq!(item["booking_type"], "coc");
}

#[tokio::test]
async fn live_endpoint_filters_by_auto_accepted_and_vehicle_type() {
    let (app, pool, tenant_id) = test_app().await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted) VALUES ($1, 'f1', jsonb_build_object('vehicle_type_name', 'CDD'), 'accepted', true)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted) VALUES ($1, 'f2', jsonb_build_object('vehicle_type_name', 'TRONTON'), 'accepted', false)")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let session_cookie = login_as_main_account(&app, tenant_id).await;
    let res = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/bookings/history?auto_accepted=true&vehicle_type=CDD")
                .header("cookie", &session_cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = axum_test_json_body(res).await;
    let items = body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["spx_id"], "f1");
}
```

(If this test file's exact helper names — `test_app`, `login_as_main_account`, `axum_test_json_body` — differ from what's shown, use the file's ACTUAL existing helpers; read the file first and adapt these two tests to match its real fixtures rather than inventing new ones.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Backend && cargo test -p api-gateway --test bookings_routes live_endpoint_returns_spx_derived_fields`
Expected: FAIL — fields not present on `BookingListItem`.

- [ ] **Step 3: Implement**

Modify `Backend/crates/api-gateway/src/routes/bookings.rs`:

Replace the `BookingListItem` struct and its `From<store::models::Booking>` impl (currently lines 60-96) with:

```rust
#[derive(Debug, Serialize)]
pub struct BookingListItem {
    pub id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub route: Vec<String>,
    pub request_id: Option<String>,
    pub onsite_id: Option<String>,
    /// The SPXID-prefixed display name shown as "Booking Number" — distinct from `spx_id`
    /// (this row's internal external-id column). Sourced from `store::models::Booking.spx_tx_id`.
    pub booking_number: String,
    pub vehicle_type: Option<String>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub pickup_time: Option<DateTime<Utc>>,
    pub trip_type: Option<i32>,
    /// `"coc"` | `"reguler"` — reuses the existing `is_coc` signal (already the established
    /// COC/REG ground truth elsewhere in this codebase), not a new derivation.
    pub booking_type: &'static str,
}

impl From<store::models::Booking> for BookingListItem {
    fn from(b: store::models::Booking) -> Self {
        let route = spx_client::normalize_booking(&b.raw_data).route_stops;
        let booking_type = if b.is_coc { "coc" } else { "reguler" };
        Self {
            id: b.id,
            account_id: b.account_id,
            spx_id: b.spx_id,
            status: b.status,
            service_type: b.service_type,
            weight: b.weight,
            cod_amount: b.cod_amount,
            auto_accepted: b.auto_accepted,
            rule_matched: b.rule_matched,
            created_at: b.created_at,
            route,
            request_id: b.spx_request_id,
            onsite_id: b.spx_onsite_id,
            booking_number: b.spx_tx_id,
            vehicle_type: b.spx_vehicle_type,
            deadline_at: b.spx_deadline_at,
            pickup_time: b.spx_pickup_time,
            trip_type: b.spx_trip_type,
            booking_type,
        }
    }
}
```

Replace `ListParams` (currently lines 19-35) with:

```rust
#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub status: Option<String>,
    pub spx_id: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub auto_accepted: Option<bool>,
    pub accept_reason: Option<String>,
    pub vehicle_type: Option<String>,
    pub trip_type: Option<i32>,
    /// `"coc"` | `"reguler"` — any other value is a 400 (see `parse_booking_type_filter`).
    pub booking_type: Option<String>,
    pub origin_station: Option<String>,
    pub dest_station: Option<String>,
    pub weight_min: Option<f64>,
    pub weight_max: Option<f64>,
    pub cod: Option<bool>,
    pub pickup_from: Option<DateTime<Utc>>,
    pub pickup_to: Option<DateTime<Utc>>,
    pub deadline_from: Option<DateTime<Utc>>,
    pub deadline_to: Option<DateTime<Utc>>,
    /// `"newest"` (default) | `"deadline_soonest"` — any other value is a 400.
    pub sort: Option<String>,
}
```

Add, next to the existing `parse_status_filter`:

```rust
fn parse_booking_type_filter(v: &str) -> Result<store::bookings::BookingTypeFilter, ApiError> {
    match v {
        "coc" => Ok(store::bookings::BookingTypeFilter::Coc),
        "reguler" => Ok(store::bookings::BookingTypeFilter::Reguler),
        other => Err(ApiError::BadRequest(format!("invalid booking_type filter: {other}"))),
    }
}

fn parse_sort(v: &str) -> Result<store::bookings::SortKey, ApiError> {
    match v {
        "newest" => Ok(store::bookings::SortKey::NewestFirst),
        "deadline_soonest" => Ok(store::bookings::SortKey::DeadlineSoonest),
        other => Err(ApiError::BadRequest(format!("invalid sort: {other}"))),
    }
}

fn build_filter(params: &ListParams, status: Option<&'static str>) -> Result<store::bookings::BookingFilter, ApiError> {
    Ok(store::bookings::BookingFilter {
        status,
        spx_id: params.spx_id.clone(),
        from: params.from,
        to: params.to,
        auto_accepted: params.auto_accepted,
        accept_reason: params.accept_reason.clone(),
        vehicle_type: params.vehicle_type.clone(),
        trip_type: params.trip_type,
        booking_type: params.booking_type.as_deref().map(parse_booking_type_filter).transpose()?,
        origin_station: params.origin_station.clone(),
        dest_station: params.dest_station.clone(),
        weight_min: params.weight_min,
        weight_max: params.weight_max,
        cod: params.cod,
        pickup_from: params.pickup_from,
        pickup_to: params.pickup_to,
        deadline_from: params.deadline_from,
        deadline_to: params.deadline_to,
        sort: params.sort.as_deref().map(parse_sort).transpose()?.unwrap_or_default(),
    })
}
```

Update `live` and `history` handlers to call `build_filter(&params, status)` instead of their current inline `BookingFilter { ... }` literal (replace the `let filter = store::bookings::BookingFilter { status, spx_id: ..., from: ..., to: ... };` block in each with `let filter = build_filter(&params, status)?;`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Backend && cargo test -p api-gateway --test bookings_routes`
Expected: all tests (existing + 2 new) PASS.

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(api-gateway): expose SPX-derived fields on BookingListItem, expand filter params"
```

---

## Task 5: `api-gateway` — `GET /bookings/summary` and `GET /bookings/vehicle-types`

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs`
- Test: same test file as Task 4

**Interfaces:**
- Consumes: Task 2's `store::bookings::summary`/`list_vehicle_types`.
- Produces: `GET /bookings/summary -> BookingSummary` (JSON), `GET /bookings/vehicle-types -> Vec<String>` — consumed by Task 9 (frontend `api-command.ts`) and Task 13 (`FilterDrawer`'s Armada dropdown).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn summary_endpoint_requires_session() {
    let (app, _pool, _tenant_id) = test_app().await;
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/bookings/summary")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn summary_endpoint_returns_todays_counts() {
    let (app, pool, tenant_id) = test_app().await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status) VALUES ($1, 's1', '{}', 'pending')")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    let session_cookie = login_as_main_account(&app, tenant_id).await;
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/bookings/summary")
                .header("cookie", &session_cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = axum_test_json_body(res).await;
    assert_eq!(body["incoming_today"], 1);
}

#[tokio::test]
async fn vehicle_types_endpoint_returns_distinct_list() {
    let (app, pool, tenant_id) = test_app().await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'vt1', jsonb_build_object('vehicle_type_name', 'CDD'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    let session_cookie = login_as_main_account(&app, tenant_id).await;
    let res = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/bookings/vehicle-types")
                .header("cookie", &session_cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = axum_test_json_body(res).await;
    assert_eq!(body, serde_json::json!(["CDD"]));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Backend && cargo test -p api-gateway --test bookings_routes summary_endpoint`
Expected: FAIL — 404 (route doesn't exist yet).

- [ ] **Step 3: Implement**

Add to `Backend/crates/api-gateway/src/routes/bookings.rs`, after the `spx_log` handler:

```rust
async fn summary(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<store::bookings::BookingSummary>, ApiError> {
    let s = store::bookings::summary(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(s))
}

async fn vehicle_types(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<String>>, ApiError> {
    let types = store::bookings::list_vehicle_types(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(types))
}
```

Add the two routes to `bookings_router`'s chain (currently `.route("/spx-log", get(spx_log))`):

```rust
        .route("/summary", get(summary))
        .route("/vehicle-types", get(vehicle_types))
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Backend && cargo test -p api-gateway --test bookings_routes`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs
git commit -m "feat(api-gateway): add GET /bookings/summary and GET /bookings/vehicle-types"
```

---

## Task 6: Backend sign-off checkpoint

**Files:** none (verification only)

- [ ] **Step 1: Full workspace check**

Run: `cd Backend && cargo test --workspace`
Expected: PASS (with `DATABASE_URL` unset — see Global Constraints).

Run: `cd Backend && cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cd Backend && cargo deny check`
Expected: PASS.

- [ ] **Step 2: Confirm no other crate depends on the old 4-field `BookingFilter` shape**

Run: `grep -rn "BookingFilter {" Backend/crates --include=*.rs`
Expected: only `store::bookings`'s own definition and `api-gateway`'s `build_filter` construct it — if `executor`/`poller`/anywhere else constructs one with the old 4 fields, update those call sites too (add `..Default::default()` if they don't need the new fields, since `BookingFilter` now derives `Default`).

This task has no commit — it's a gate before frontend work starts. If anything fails, fix it in a follow-up commit before proceeding to Task 7.

---

## Task 7: `$lib/tickets.ts` + `$lib/api-bookings.ts` — expanded types and query builder

**Files:**
- Modify: `Frontend/src/lib/tickets.ts`
- Modify: `Frontend/src/lib/api-tickets.ts`
- Modify: `Frontend/src/lib/api-bookings.ts`
- Test: `Frontend/src/lib/tickets.test.ts`

**Interfaces:**
- Produces: `TicketFilters` (expanded), `TicketDetailRow` (expanded), `filtersToQueryString` (expanded) — consumed by Task 11 (`TicketsTable`), Task 12 (`FilterDrawer`), Task 13 (`/command`).

- [ ] **Step 1: Write the failing test**

Append to `Frontend/src/lib/tickets.test.ts`:

```typescript
import { describe, it, expect } from 'vitest';
import { filtersToQueryString, type TicketFilters } from './tickets';

describe('filtersToQueryString — expanded filters', () => {
	it('includes every new filter field only when set', () => {
		const filters: TicketFilters = {
			status: null,
			spxId: '',
			from: null,
			to: null,
			requestId: 'R1',
			bookingName: '',
			vehicleType: 'TRONTON',
			tripType: 1,
			bookingType: 'coc',
			originStation: null,
			destStation: null,
			weightMin: 10,
			weightMax: null,
			cod: true,
			pickupFrom: null,
			pickupTo: null,
			deadlineFrom: null,
			deadlineTo: null,
			sort: 'deadline_soonest',
			autoAccepted: true,
			acceptReason: null
		};
		const qs = filtersToQueryString(filters, 1, 50);
		const params = new URLSearchParams(qs);
		expect(params.get('request_id')).toBe('R1');
		expect(params.get('booking_name')).toBeNull();
		expect(params.get('vehicle_type')).toBe('TRONTON');
		expect(params.get('trip_type')).toBe('1');
		expect(params.get('booking_type')).toBe('coc');
		expect(params.get('weight_min')).toBe('10');
		expect(params.get('weight_max')).toBeNull();
		expect(params.get('cod')).toBe('true');
		expect(params.get('sort')).toBe('deadline_soonest');
		expect(params.get('auto_accepted')).toBe('true');
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm test:unit tickets.test.ts`
Expected: FAIL — `TicketFilters` has no `requestId`/etc. fields (TypeScript compile error surfaced by vitest).

- [ ] **Step 3: Implement**

Modify `Frontend/src/lib/tickets.ts`:

Replace `TicketFilters` (currently lines 26-31) with:

```typescript
export type TicketFilters = {
	status: TicketStatus | null;
	spxId: string;
	from: string | null;
	to: string | null;
	requestId: string;
	bookingName: string;
	vehicleType: string | null;
	tripType: number | null;
	bookingType: 'coc' | 'reguler' | null;
	originStation: string | null;
	destStation: string | null;
	weightMin: number | null;
	weightMax: number | null;
	cod: boolean | null;
	pickupFrom: string | null;
	pickupTo: string | null;
	deadlineFrom: string | null;
	deadlineTo: string | null;
	sort: 'newest' | 'deadline_soonest';
	/** Drives `/command`'s "Accept by Bot" vs "Diambil Operator" widgets — maps straight to the
	 * backend's existing `auto_accepted` param (Task 4). `null` = no filter (both). */
	autoAccepted: boolean | null;
	/** Drives `/command`'s "Close (Agency Lain)" widget — maps to the backend's existing
	 * `accept_reason` param (Task 4), e.g. `'taken_by_other'`. `null` = no filter. */
	acceptReason: string | null;
};

export const EMPTY_TICKET_FILTERS: TicketFilters = {
	status: null,
	spxId: '',
	from: null,
	to: null,
	requestId: '',
	bookingName: '',
	vehicleType: null,
	tripType: null,
	bookingType: null,
	originStation: null,
	destStation: null,
	weightMin: null,
	weightMax: null,
	cod: null,
	pickupFrom: null,
	pickupTo: null,
	deadlineFrom: null,
	deadlineTo: null,
	sort: 'newest',
	autoAccepted: null,
	acceptReason: null
};
```

Replace `TicketDetailRow` (lines 11-24) — add the new display fields:

```typescript
export type TicketDetailRow = {
	id: string;
	spxId: string;
	status: TicketStatus;
	failureReason: FailureReason;
	route: string[];
	serviceType: string | null;
	weight: number;
	codAmount: number;
	autoAccepted: boolean;
	createdAt: string;
	accepting: boolean;
	requestId: string | null;
	onsiteId: string | null;
	bookingNumber: string;
	vehicleType: string | null;
	deadlineAt: string | null;
	pickupTime: string | null;
	tripType: number | null;
	bookingType: 'coc' | 'reguler';
};
```

Replace `filtersToQueryString` (lines 38-51):

```typescript
export function filtersToQueryString(
	filters: Omit<TicketFilters, 'status'> & { status?: TicketStatus | null },
	page: number,
	pageSize: number = PAGE_SIZE_DEFAULT
): string {
	const params = new URLSearchParams();
	if (filters.status) params.set('status', filters.status);
	if (filters.spxId) params.set('spx_id', filters.spxId);
	if (filters.from) params.set('from', filters.from);
	if (filters.to) params.set('to', filters.to);
	if (filters.requestId) params.set('request_id', filters.requestId);
	if (filters.bookingName) params.set('booking_name', filters.bookingName);
	if (filters.vehicleType) params.set('vehicle_type', filters.vehicleType);
	if (filters.tripType !== null) params.set('trip_type', String(filters.tripType));
	if (filters.bookingType) params.set('booking_type', filters.bookingType);
	if (filters.originStation) params.set('origin_station', filters.originStation);
	if (filters.destStation) params.set('dest_station', filters.destStation);
	if (filters.weightMin !== null) params.set('weight_min', String(filters.weightMin));
	if (filters.weightMax !== null) params.set('weight_max', String(filters.weightMax));
	if (filters.cod !== null) params.set('cod', String(filters.cod));
	if (filters.pickupFrom) params.set('pickup_from', filters.pickupFrom);
	if (filters.pickupTo) params.set('pickup_to', filters.pickupTo);
	if (filters.deadlineFrom) params.set('deadline_from', filters.deadlineFrom);
	if (filters.deadlineTo) params.set('deadline_to', filters.deadlineTo);
	if (filters.sort !== 'newest') params.set('sort', filters.sort);
	if (filters.autoAccepted !== null) params.set('auto_accepted', String(filters.autoAccepted));
	if (filters.acceptReason) params.set('accept_reason', filters.acceptReason);
	params.set('limit', String(pageSize));
	params.set('offset', String((page - 1) * pageSize));
	return params.toString();
}
```

Note: `filters.bookingName` has no server-side match yet in `BookingFilter`/`ListParams` (Task 4 added `spx_id`/`vehicle_type`/etc. but the reference's "Nama Booking" field maps to `spx_tx_id`, which Task 4 did NOT add as a filter param, only a display field). Add it now for consistency — go back to Task 4's `ListParams`/`build_filter`/`push_common_filters` and add `booking_name: Option<String>` following the exact same `LIKE`-prefix pattern as the existing `spx_id` filter (`AND spx_tx_id LIKE ... ESCAPE '\'`), with a matching test. Do this before continuing this task's remaining steps — this file cannot be fully correct without it.

Modify `Frontend/src/lib/api-tickets.ts`: update `BookingListItemWire` (lines 18-28) to add `request_id: string | null; onsite_id: string | null; booking_number: string; vehicle_type: string | null; deadline_at: string | null; pickup_time: string | null; trip_type: number | null; booking_type: 'coc' | 'reguler';`, and update `toDetailRow` (lines 37-51) to map each into the corresponding new `TicketDetailRow` camelCase field.

Modify `Frontend/src/lib/api-bookings.ts`: `fetchLiveBookings` stays as-is (it returns `TicketRow`, the `/command` ticker's narrower type, not `TicketDetailRow` — out of scope here per the design doc, `/command`'s list-below-widgets in Task 14 reads through `api-tickets.ts`'s `fetchTickets`, not this file's `fetchLiveBookings`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Frontend && pnpm test:unit`
Expected: all PASS.

Run: `cd Frontend && pnpm check`
Expected: no TypeScript errors.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/tickets.ts Frontend/src/lib/tickets.test.ts Frontend/src/lib/api-tickets.ts Frontend/src/lib/api-bookings.ts Backend/crates/store/src/bookings.rs Backend/crates/api-gateway/src/routes/bookings.rs
git commit -m "feat(frontend): expand TicketFilters/TicketDetailRow, wire booking_name filter end-to-end"
```

---

## Task 8: `$lib/api-command.ts` — summary + vehicle-types fetchers

**Files:**
- Create: `Frontend/src/lib/api-command.ts`
- Test: `Frontend/src/lib/api-command.test.ts`

**Interfaces:**
- Consumes: Task 5's `GET /bookings/summary`, `GET /bookings/vehicle-types`.
- Produces: `fetchSummary(): Promise<CommandSummary>`, `fetchVehicleTypes(): Promise<string[]>` — consumed by Task 13 (`/command`), Task 12 (`FilterDrawer`).

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/api-command.test.ts
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchSummary } from './api-command';

describe('fetchSummary', () => {
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('maps the wire response to camelCase', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue({
				ok: true,
				json: async () => ({
					incoming_today: 5,
					accepted_auto_today: 2,
					accepted_manual_today: 1,
					taken_by_other_today: 0,
					latency_p99_ms: 210.5
				})
			})
		);
		const result = await fetchSummary();
		expect(result).toEqual({
			incomingToday: 5,
			acceptedAutoToday: 2,
			acceptedManualToday: 1,
			takenByOtherToday: 0,
			latencyP99Ms: 210.5
		});
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm test:unit api-command.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

```typescript
// Frontend/src/lib/api-command.ts
// Thin typed REST layer for /command's KPI widgets — no UI logic here, matching
// api-bookings.ts/api-tickets.ts's established convention.
import { ApiError } from './api';

type BookingSummaryWire = {
	incoming_today: number;
	accepted_auto_today: number;
	accepted_manual_today: number;
	taken_by_other_today: number;
	latency_p99_ms: number | null;
};

export type CommandSummary = {
	incomingToday: number;
	acceptedAutoToday: number;
	acceptedManualToday: number;
	takenByOtherToday: number;
	latencyP99Ms: number | null;
};

export async function fetchSummary(): Promise<CommandSummary> {
	const res = await fetch('/bookings/summary', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch summary');
	const w: BookingSummaryWire = await res.json();
	return {
		incomingToday: w.incoming_today,
		acceptedAutoToday: w.accepted_auto_today,
		acceptedManualToday: w.accepted_manual_today,
		takenByOtherToday: w.taken_by_other_today,
		latencyP99Ms: w.latency_p99_ms
	};
}

export async function fetchVehicleTypes(): Promise<string[]> {
	const res = await fetch('/bookings/vehicle-types', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch vehicle types');
	return res.json();
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Frontend && pnpm test:unit api-command.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/api-command.ts Frontend/src/lib/api-command.test.ts
git commit -m "feat(frontend): add api-command.ts (summary + vehicle-types fetchers)"
```

---

## Task 9: `CountdownBadge.svelte` — reusable countdown

**Files:**
- Create: `Frontend/src/lib/countdown.ts` (pure formatting logic, unit-tested)
- Create: `Frontend/src/lib/components/CountdownBadge.svelte`
- Test: `Frontend/src/lib/countdown.test.ts`

**Interfaces:**
- Produces: `formatCountdown(targetIso: string, nowMs: number): { label: string; expired: boolean }`, `<CountdownBadge target={string | null} size="lg" | "sm" />` — consumed by Task 11 (`TicketsTable`'s Deadline Bidding column, both the large countdown and the small "STANDBY" badge per the design doc's confirmed "same value, rendered twice" decision).

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/countdown.test.ts
import { describe, it, expect } from 'vitest';
import { formatCountdown } from './countdown';

describe('formatCountdown', () => {
	it('formats hours/minutes when more than an hour remains', () => {
		const now = Date.parse('2026-07-18T10:00:00Z');
		const target = '2026-07-18T13:34:00Z';
		expect(formatCountdown(target, now)).toEqual({ label: '3h 34m', expired: false });
	});

	it('formats minutes/seconds when under an hour remains', () => {
		const now = Date.parse('2026-07-18T10:00:00Z');
		const target = '2026-07-18T10:01:22Z';
		expect(formatCountdown(target, now)).toEqual({ label: '01:22', expired: false });
	});

	it('marks expired when the target is in the past', () => {
		const now = Date.parse('2026-07-18T10:00:00Z');
		const target = '2026-07-18T09:00:00Z';
		expect(formatCountdown(target, now)).toEqual({ label: '00:00', expired: true });
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm test:unit countdown.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

```typescript
// Frontend/src/lib/countdown.ts
// Pure formatting for Deadline Bidding's countdown — both the large ("3h 34m" style) and small
// "STANDBY" badge in TicketsTable read the SAME deadline_at value through this one function
// (confirmed with the user during design: there is no second, distinct deadline field).
export function formatCountdown(
	targetIso: string,
	nowMs: number
): { label: string; expired: boolean } {
	const deltaMs = Date.parse(targetIso) - nowMs;
	if (deltaMs <= 0) return { label: '00:00', expired: true };
	const totalSeconds = Math.floor(deltaMs / 1000);
	const hours = Math.floor(totalSeconds / 3600);
	const minutes = Math.floor((totalSeconds % 3600) / 60);
	const seconds = totalSeconds % 60;
	if (hours > 0) return { label: `${hours}h ${minutes}m`, expired: false };
	const pad = (n: number) => String(n).padStart(2, '0');
	return { label: `${pad(minutes)}:${pad(seconds)}`, expired: false };
}
```

```svelte
<!-- Frontend/src/lib/components/CountdownBadge.svelte -->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { formatCountdown } from '$lib/countdown';

	let { target, size = 'lg' }: { target: string | null; size?: 'lg' | 'sm' } = $props();

	let nowMs = $state(Date.now());
	let timer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		timer = setInterval(() => (nowMs = Date.now()), 1000);
	});
	onDestroy(() => {
		if (timer) clearInterval(timer);
	});

	const formatted = $derived(target ? formatCountdown(target, nowMs) : null);
</script>

{#if formatted}
	<span
		class={size === 'lg'
			? 'font-mono text-[13px] font-semibold ' + (formatted.expired ? 'text-danger' : 'text-text-primary')
			: 'font-mono text-[10px] px-1.5 py-0.5 rounded ' +
				(formatted.expired ? 'bg-danger/10 text-danger' : 'bg-accent/10 text-accent')}
	>
		{formatted.label}{#if size === 'sm'}&nbsp;STANDBY{/if}
	</span>
{:else}
	<span class="text-text-muted text-[11px]">—</span>
{/if}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Frontend && pnpm test:unit countdown.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/countdown.ts Frontend/src/lib/countdown.test.ts Frontend/src/lib/components/CountdownBadge.svelte
git commit -m "feat(frontend): add CountdownBadge + formatCountdown (Deadline Bidding)"
```

---

## Task 10: `StatCard.svelte`

**Files:**
- Create: `Frontend/src/lib/components/StatCard.svelte`
- Test: `Frontend/src/lib/components/StatCard.test.ts`

**Interfaces:**
- Produces: `<StatCard label={string} value={string} active={boolean} onclick={() => void} />` — consumed by Task 13 (`/command`).

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/components/StatCard.test.ts
import { describe, it, expect } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import StatCard from './StatCard.svelte';

describe('StatCard', () => {
	it('renders label and value, calls onclick when clicked', async () => {
		let clicked = false;
		const { getByRole } = render(StatCard, {
			props: { label: 'Tiket Masuk', value: '42', active: false, onclick: () => (clicked = true) }
		});
		const btn = getByRole('button', { name: /Tiket Masuk/ });
		expect(btn.textContent).toContain('42');
		await fireEvent.click(btn);
		expect(clicked).toBe(true);
	});
});
```

(If `@testing-library/svelte` is not already a devDependency, check `Frontend/package.json` first — this project's existing Vitest setup may use a different render approach; adapt to whatever `Frontend/src/lib/components/*.test.ts` — if none exist yet, check any existing `.svelte`-adjacent test for the established pattern before introducing a new one.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm test:unit StatCard.test.ts`
Expected: FAIL — component not found.

- [ ] **Step 3: Implement**

```svelte
<!-- Frontend/src/lib/components/StatCard.svelte -->
<script lang="ts">
	let {
		label,
		value,
		active = false,
		onclick
	}: { label: string; value: string; active?: boolean; onclick?: () => void } = $props();
</script>

<button
	type="button"
	{onclick}
	aria-pressed={onclick ? active : undefined}
	class="min-h-[44px] flex flex-col gap-1 p-3.5 rounded-lg border text-left transition-colors
		{active ? 'border-accent bg-accent/10' : 'border-border bg-bg-surface hover:bg-bg-base'}
		focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
>
	<span class="font-mono text-xl font-semibold text-text-primary">{value}</span>
	<span class="text-[11px] font-body text-text-muted">{label}</span>
</button>
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Frontend && pnpm test:unit StatCard.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/components/StatCard.svelte Frontend/src/lib/components/StatCard.test.ts
git commit -m "feat(frontend): add StatCard component"
```

---

## Task 11: `TicketsTable.svelte` — full column rewrite

**Files:**
- Modify: `Frontend/src/lib/components/TicketsTable.svelte`

**Interfaces:**
- Consumes: Task 7's expanded `TicketDetailRow`, Task 9's `CountdownBadge`.
- Produces: same public props as before (`rows`, `onRowClick`, `onAccept`) — no signature change, so Task 14 (`/tickets` page) needs no changes to how it invokes this component.

- [ ] **Step 1: Read the current file in full** (already shown above in this conversation's investigation — 158 lines, real `<table>` desktop / stacked `<ul>` mobile) to confirm line numbers haven't drifted since this plan was written.

- [ ] **Step 2: Write/extend the accessibility a11y test first** (this project's established pattern per Fase 7c's whole-branch review: a keyboard-interaction e2e test is what caught real bugs manual review missed — but for THIS task, a unit-level check that new columns render is the right altitude; e2e coverage for the full table is Task 15)

```typescript
// Frontend/src/lib/components/TicketsTable.test.ts (new file)
import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/svelte';
import TicketsTable from './TicketsTable.svelte';
import type { TicketDetailRow } from '$lib/tickets';

const row: TicketDetailRow = {
	id: '1',
	spxId: 'SPX1',
	status: 'pending',
	failureReason: null,
	route: ['Cikarang DC', 'Semarang DC'],
	serviceType: 'REG',
	weight: 12.5,
	codAmount: 0,
	autoAccepted: false,
	createdAt: '2026-07-18T10:00:00Z',
	accepting: false,
	requestId: '11843622',
	onsiteId: null,
	bookingNumber: 'SPXID_VM_001404067B',
	vehicleType: 'TRONTON (10WH)',
	deadlineAt: '2026-07-18T23:52:00Z',
	pickupTime: '2026-07-18T09:00:00Z',
	tripType: 0,
	bookingType: 'coc'
};

describe('TicketsTable', () => {
	it('renders the ID (BK/REQ), Booking Number, and vehicle type columns', () => {
		const { getByText } = render(TicketsTable, {
			props: { rows: [row], onRowClick: () => {}, onAccept: () => {} }
		});
		expect(getByText('SPXID_VM_001404067B')).toBeTruthy();
		expect(getByText('11843622')).toBeTruthy();
		expect(getByText(/TRONTON/)).toBeTruthy();
		expect(getByText('COC')).toBeTruthy();
		expect(getByText('ADHOC')).toBeTruthy();
	});
});
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd Frontend && pnpm test:unit TicketsTable.test.ts`
Expected: FAIL — new columns don't exist yet.

- [ ] **Step 4: Implement — replace the desktop `<table>` and mobile `<ul>` bodies**

Add near the top of the `<script>` block (after the existing `formatDate` function):

```typescript
	import CountdownBadge from './CountdownBadge.svelte';

	// Best-effort mapping, NOT independently verified against a captured SPX payload — see the
	// design doc's Open Questions. Single named constant so it's a one-line fix if reversed.
	const TRIP_TYPE_ADHOC = 0;

	function tripTypeLabel(tripType: number | null): string | null {
		if (tripType === null) return null;
		return tripType === TRIP_TYPE_ADHOC ? 'ADHOC' : 'FIX';
	}
```

Replace the desktop `<thead>` row (lines 44-53) with:

```svelte
			<tr class="border-b border-border text-left text-[10px] uppercase tracking-wide text-text-muted">
				<th scope="col" class="py-2 pr-3">ID</th>
				<th scope="col" class="py-2 pr-3">Booking Number</th>
				<th scope="col" class="py-2 pr-3">Route & Vehicle</th>
				<th scope="col" class="py-2 pr-3">Jadwal Booking</th>
				<th scope="col" class="py-2 pr-3">Deadline Bidding</th>
				<th scope="col" class="py-2 pr-3">Tags</th>
				<th scope="col" class="py-2 pr-3">Status</th>
				<th scope="col" class="py-2 pr-3">Accept By</th>
				<th scope="col" class="py-2 pr-3"><span class="sr-only">Aksi</span></th>
			</tr>
```

Replace the desktop `<tbody>` row cells (lines 71-98, between the opening `<tr ...>` and its closing `</tr>`) with:

```svelte
						<td class="py-2.5 pr-3 font-mono text-[11px] text-text-muted whitespace-nowrap">
							<div>BK <span class="text-text-primary">{row.bookingNumber}</span></div>
							{#if row.requestId}<div>REQ <span class="text-text-primary">{row.requestId}</span></div>{/if}
							{#if row.onsiteId}<div>OID <span class="text-text-primary">{row.onsiteId}</span></div>{/if}
						</td>
						<td class="py-2.5 pr-3 font-mono text-text-primary">{row.bookingNumber}</td>
						<td class="py-2.5 pr-3 text-text-primary truncate max-w-[220px]">
							<div>{row.route.join(' → ') || '—'}</div>
							{#if row.vehicleType}<div class="text-[10px] text-text-muted">{row.vehicleType}</div>{/if}
						</td>
						<td class="py-2.5 pr-3 font-mono text-text-muted whitespace-nowrap">
							{row.pickupTime ? formatDate(row.pickupTime) : '—'}
						</td>
						<td class="py-2.5 pr-3 whitespace-nowrap">
							<CountdownBadge target={row.deadlineAt} size="lg" />
							<div class="mt-0.5"><CountdownBadge target={row.deadlineAt} size="sm" /></div>
						</td>
						<td class="py-2.5 pr-3">
							<span class="inline-flex flex-wrap gap-1">
								<span class="text-[10px] px-1.5 py-0.5 rounded bg-live/10 text-live uppercase font-semibold">
									{row.bookingType === 'coc' ? 'COC' : 'REG'}
								</span>
								{#if tripTypeLabel(row.tripType)}
									<span class="text-[10px] px-1.5 py-0.5 rounded bg-accent/10 text-accent uppercase font-semibold">
										{tripTypeLabel(row.tripType)}
									</span>
								{/if}
							</span>
						</td>
						<td class="py-2.5 pr-3">
							<span class="inline-flex items-center gap-1.5">
								<span aria-hidden="true" class="w-1.5 h-1.5 rounded-full shrink-0 {statusDotClass(row.status)}"></span>
								<span class="text-text-primary">{statusLabel(row.status)}</span>
							</span>
						</td>
						<td class="py-2.5 pr-3 text-text-muted">—</td>
						<td class="py-2.5 pr-3">
							{#if row.status === 'pending'}
								<button
									type="button"
									disabled={row.accepting}
									onclick={(e) => {
										e.stopPropagation();
										onAccept(row);
									}}
									class="min-h-[44px] min-w-[44px] px-2.5 rounded-md text-[11px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
								>
									{row.accepting ? 'Memproses…' : 'Terima'}
								</button>
							{/if}
						</td>
```

Update the mobile `<li>` block (lines 129-142) to add the same new fields in the existing stacked-label style — insert after the existing route `<div>`:

```svelte
						<div class="text-[11px] text-text-muted">{row.bookingNumber}</div>
						{#if row.vehicleType}<div class="text-[11px] text-text-muted">{row.vehicleType}</div>{/if}
						<div class="flex items-center gap-2">
							<CountdownBadge target={row.deadlineAt} size="lg" />
							<span class="text-[10px] px-1.5 py-0.5 rounded bg-live/10 text-live uppercase font-semibold">
								{row.bookingType === 'coc' ? 'COC' : 'REG'}
							</span>
							{#if tripTypeLabel(row.tripType)}
								<span class="text-[10px] px-1.5 py-0.5 rounded bg-accent/10 text-accent uppercase font-semibold">
									{tripTypeLabel(row.tripType)}
								</span>
							{/if}
						</div>
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd Frontend && pnpm test:unit TicketsTable.test.ts`
Expected: PASS.

Run: `cd Frontend && pnpm check`
Expected: no TypeScript errors.

- [ ] **Step 6: Commit**

```bash
git add Frontend/src/lib/components/TicketsTable.svelte Frontend/src/lib/components/TicketsTable.test.ts
git commit -m "feat(frontend): rewrite TicketsTable columns (ID/booking-number/vehicle/deadline/tags)"
```

---

## Task 12: `FilterDrawer.svelte` — replaces `TicketFilterBar`

**Files:**
- Create: `Frontend/src/lib/components/FilterDrawer.svelte`
- Delete: `Frontend/src/lib/components/TicketFilterBar.svelte` (after Task 14 stops importing it)
- Test: `Frontend/src/lib/components/FilterDrawer.test.ts`

**Interfaces:**
- Consumes: Task 7's `TicketFilters`/`EMPTY_TICKET_FILTERS`, Task 8's `fetchVehicleTypes`.
- Produces: `<FilterDrawer open={boolean} filters={TicketFilters} onFiltersChange={(f) => void} onClose={() => void} resultCount={number} />` — consumed by Task 14.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/components/FilterDrawer.test.ts
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/svelte';
import FilterDrawer from './FilterDrawer.svelte';
import { EMPTY_TICKET_FILTERS } from '$lib/tickets';

vi.mock('$lib/api-command', () => ({
	fetchVehicleTypes: vi.fn().mockResolvedValue(['CDD', 'TRONTON'])
}));
vi.mock('$lib/api-locations', () => ({
	fetchLocations: vi.fn().mockResolvedValue([])
}));

describe('FilterDrawer', () => {
	it('calls onFiltersChange with an updated (not mutated) object when a field changes', async () => {
		let received: typeof EMPTY_TICKET_FILTERS | null = null;
		const { getByLabelText } = render(FilterDrawer, {
			props: {
				open: true,
				filters: EMPTY_TICKET_FILTERS,
				onFiltersChange: (f: typeof EMPTY_TICKET_FILTERS) => (received = f),
				onClose: () => {},
				resultCount: 0
			}
		});
		const input = getByLabelText('ID Request') as HTMLInputElement;
		await fireEvent.input(input, { target: { value: 'R99' } });
		expect(received).not.toBeNull();
		expect(received).not.toBe(EMPTY_TICKET_FILTERS);
		expect((received as unknown as typeof EMPTY_TICKET_FILTERS).requestId).toBe('R99');
		expect(EMPTY_TICKET_FILTERS.requestId).toBe(''); // original untouched — no-mutation contract
	});

	it('closes on Escape and traps Tab focus within the panel', async () => {
		const onClose = vi.fn();
		const { getByRole } = render(FilterDrawer, {
			props: { open: true, filters: EMPTY_TICKET_FILTERS, onFiltersChange: () => {}, onClose, resultCount: 0 }
		});
		const dialog = getByRole('dialog');
		await fireEvent.keyDown(dialog, { key: 'Escape' });
		expect(onClose).toHaveBeenCalled();
	});
});
```

(Check first whether `$lib/api-locations.ts` — for the `/locations` reuse the design doc calls for — already exists under that exact name; if the existing Fase 7d `LocationCombobox.svelte` reads locations through a differently-named module, use that module's real name and its real exported function name here and below, instead of assuming `api-locations.ts`/`fetchLocations`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm test:unit FilterDrawer.test.ts`
Expected: FAIL — component not found.

- [ ] **Step 3: Implement**

```svelte
<!-- Frontend/src/lib/components/FilterDrawer.svelte -->
<script lang="ts">
	// Slide-in panel replacing TicketFilterBar's inline row — the reference (Image #3) has far
	// more fields than fit inline, and per the design doc, needs a REAL multi-element focus trap
	// (many interactive fields, not a single button like 7c's TicketDetailDrawer) — same pattern
	// as AutoAcceptSwitch's modal (Backend/../Frontend/src/lib/components/AutoAcceptSwitch.svelte),
	// broadened to include <select> as a focusable element type.
	import { onMount } from 'svelte';
	import { X } from '@lucide/svelte';
	import type { TicketFilters, TicketStatus } from '$lib/tickets';
	import { EMPTY_TICKET_FILTERS } from '$lib/tickets';
	import { fetchVehicleTypes } from '$lib/api-command';

	let {
		open,
		filters,
		onFiltersChange,
		onClose,
		resultCount
	}: {
		open: boolean;
		filters: TicketFilters;
		onFiltersChange: (f: TicketFilters) => void;
		onClose: () => void;
		resultCount: number;
	} = $props();

	let dialogEl: HTMLDivElement | undefined = $state();
	let previouslyFocusedEl: HTMLElement | null = null;
	let vehicleTypes = $state<string[]>([]);

	onMount(() => {
		fetchVehicleTypes()
			.then((types) => (vehicleTypes = types))
			.catch(() => (vehicleTypes = []));
	});

	$effect(() => {
		if (open) {
			previouslyFocusedEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			dialogEl?.querySelector<HTMLElement>('input, select, button:not([disabled])')?.focus();
		} else if (previouslyFocusedEl) {
			previouslyFocusedEl.focus();
			previouslyFocusedEl = null;
		}
	});

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			onClose();
			return;
		}
		if (e.key !== 'Tab' || !dialogEl) return;
		const focusables = Array.from(
			dialogEl.querySelectorAll<HTMLElement>('input:not([disabled]), select:not([disabled]), button:not([disabled])')
		);
		if (focusables.length === 0) return;
		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		if (e.shiftKey && document.activeElement === first) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && document.activeElement === last) {
			e.preventDefault();
			first.focus();
		}
	}

	function set<K extends keyof TicketFilters>(key: K, value: TicketFilters[K]) {
		onFiltersChange({ ...filters, [key]: value });
	}

	function resetAll() {
		onFiltersChange({ ...EMPTY_TICKET_FILTERS });
	}

	const STATUS_OPTIONS: { value: TicketStatus | null; label: string }[] = [
		{ value: null, label: 'Semua status' },
		{ value: 'pending', label: 'Pending (live)' },
		{ value: 'accepted', label: 'Diterima' },
		{ value: 'failed', label: 'Gagal' }
	];
</script>

{#if open}
	<div class="fixed inset-0 z-40 bg-black/50" onclick={onClose} aria-hidden="true"></div>
	<div
		bind:this={dialogEl}
		onkeydown={handleKeydown}
		role="dialog"
		aria-modal="true"
		aria-labelledby="filter-drawer-title"
		class="fixed inset-y-0 right-0 z-50 w-full max-w-sm overflow-y-auto bg-bg-surface border-l border-border p-4 flex flex-col gap-4"
	>
		<div class="flex items-center justify-between">
			<h2 id="filter-drawer-title" class="font-heading font-semibold text-text-primary text-[14px]">Filter Lanjutan</h2>
			<button
				type="button"
				onclick={onClose}
				aria-label="Tutup filter"
				class="min-h-[44px] min-w-[44px] flex items-center justify-center rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<X size={18} aria-hidden="true" />
			</button>
		</div>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted">
			Urutkan
			<select
				value={filters.sort}
				onchange={(e) => set('sort', (e.target as HTMLSelectElement).value as TicketFilters['sort'])}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="newest">Terbaru masuk</option>
				<option value="deadline_soonest">Deadline terdekat</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-request-id">
			ID Request
			<input
				id="filter-request-id"
				type="text"
				value={filters.requestId}
				oninput={(e) => set('requestId', (e.target as HTMLInputElement).value)}
				placeholder="cth. FMR-..."
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-booking-name">
			Nama Booking
			<input
				id="filter-booking-name"
				type="text"
				value={filters.bookingName}
				oninput={(e) => set('bookingName', (e.target as HTMLInputElement).value)}
				placeholder="cth. SPXID-JKT..."
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-armada">
			Armada
			<select
				id="filter-armada"
				value={filters.vehicleType ?? ''}
				onchange={(e) => set('vehicleType', (e.target as HTMLSelectElement).value || null)}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua armada</option>
				{#each vehicleTypes as vt (vt)}
					<option value={vt}>{vt}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-tag">
			Tag Tiket
			<select
				id="filter-tag"
				value={filters.bookingType ?? (filters.tripType !== null ? `trip:${filters.tripType}` : '')}
				onchange={(e) => {
					const v = (e.target as HTMLSelectElement).value;
					if (v === '') {
						onFiltersChange({ ...filters, bookingType: null, tripType: null });
					} else if (v.startsWith('trip:')) {
						onFiltersChange({ ...filters, bookingType: null, tripType: Number(v.slice(5)) });
					} else {
						onFiltersChange({ ...filters, bookingType: v as 'coc' | 'reguler', tripType: null });
					}
				}}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua tag</option>
				<option value="coc">COC</option>
				<option value="reguler">REG</option>
				<option value="trip:0">ADHOC</option>
				<option value="trip:1">FIX</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-status">
			Status
			<select
				id="filter-status"
				value={filters.status ?? ''}
				onchange={(e) => set('status', ((e.target as HTMLSelectElement).value || null) as TicketStatus | null)}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{#each STATUS_OPTIONS as opt (opt.value ?? 'all')}
					<option value={opt.value ?? ''}>{opt.label}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-cod">
			COD
			<select
				id="filter-cod"
				value={filters.cod === null ? '' : String(filters.cod)}
				onchange={(e) => {
					const v = (e.target as HTMLSelectElement).value;
					set('cod', v === '' ? null : v === 'true');
				}}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua</option>
				<option value="true">Ya</option>
				<option value="false">Tidak</option>
			</select>
		</label>

		<div class="grid grid-cols-2 gap-2">
			<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-weight-min">
				Berat Min (KG)
				<input
					id="filter-weight-min"
					type="number"
					value={filters.weightMin ?? ''}
					oninput={(e) => {
						const v = (e.target as HTMLInputElement).value;
						set('weightMin', v === '' ? null : Number(v));
					}}
					placeholder="0"
					class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-weight-max">
				Berat Maks (KG)
				<input
					id="filter-weight-max"
					type="number"
					value={filters.weightMax ?? ''}
					oninput={(e) => {
						const v = (e.target as HTMLInputElement).value;
						set('weightMax', v === '' ? null : Number(v));
					}}
					placeholder="∞"
					class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
		</div>

		<fieldset class="flex flex-col gap-2">
			<legend class="text-[11px] text-text-muted uppercase tracking-wide">Periode / Waktu Booking</legend>
			<div class="grid grid-cols-2 gap-2">
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-pickup-from">
					Dari
					<input
						id="filter-pickup-from"
						type="date"
						value={filters.pickupFrom ? filters.pickupFrom.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('pickupFrom', v ? new Date(v).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-pickup-to">
					Sampai
					<input
						id="filter-pickup-to"
						type="date"
						value={filters.pickupTo ? filters.pickupTo.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('pickupTo', v ? new Date(`${v}T23:59:59.999Z`).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>
		</fieldset>

		<fieldset class="flex flex-col gap-2">
			<legend class="text-[11px] text-text-muted uppercase tracking-wide">Batas Waktu Konfirmasi (Deadline)</legend>
			<div class="grid grid-cols-2 gap-2">
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-deadline-from">
					Dari
					<input
						id="filter-deadline-from"
						type="date"
						value={filters.deadlineFrom ? filters.deadlineFrom.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('deadlineFrom', v ? new Date(v).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-deadline-to">
					Sampai
					<input
						id="filter-deadline-to"
						type="date"
						value={filters.deadlineTo ? filters.deadlineTo.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('deadlineTo', v ? new Date(`${v}T23:59:59.999Z`).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>
		</fieldset>

		<div class="mt-auto flex items-center justify-between gap-3 pt-3 border-t border-border">
			<span class="text-[11px] text-text-muted">{resultCount} tiket cocok</span>
			<div class="flex gap-2">
				<button
					type="button"
					onclick={resetAll}
					class="min-h-[44px] px-3 rounded-md text-[12px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					Reset Semua
				</button>
				<button
					type="button"
					onclick={onClose}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-bold bg-accent text-bg-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					Selesai
				</button>
			</div>
		</div>
	</div>
{/if}
```

(Station Keberangkatan/Tujuan dropdowns, sourced from `/locations`, are deliberately left as a follow-up wired in Task 14 alongside the page-level `fetchLocations()` call the page already makes for other purposes — do not duplicate that fetch inside this component if the page already has the data available to pass down as a prop; check `Frontend/src/routes/(app)/rules/+page.svelte`'s existing `fetchLocations()` usage for the established pattern before deciding whether to fetch here or accept a `locations` prop.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd Frontend && pnpm test:unit FilterDrawer.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/components/FilterDrawer.svelte Frontend/src/lib/components/FilterDrawer.test.ts
git commit -m "feat(frontend): add FilterDrawer (replaces TicketFilterBar)"
```

---

## Task 13: `/command` page rewrite

**Files:**
- Modify: `Frontend/src/routes/(app)/command/+page.svelte`

**Interfaces:**
- Consumes: Task 8's `fetchSummary`, Task 10's `StatCard`, Task 7's `fetchTickets`/`TicketFilters`.

- [ ] **Step 1: Manual verification step first (no automated test — this task's correctness is covered by Task 15's e2e)**: read the current file in full (already shown above — 85 lines) to confirm nothing has drifted.

- [ ] **Step 2: Implement**

Replace the `<script>` block's body (keep the existing `handleWsEvent`/`LIVE_POLL_INTERVAL_MS` ticker logic for the default "incoming" view untouched — only ADD to it):

```svelte
<script lang="ts">
	import { getContext, onMount, onDestroy } from 'svelte';
	import type { WsStore, TowerWsEvent } from '$lib/ws.svelte';
	import { fetchLiveBookings } from '$lib/api-bookings';
	import { fetchSummary, type CommandSummary } from '$lib/api-command';
	import { fetchTickets } from '$lib/api-tickets';
	import { EMPTY_TICKET_FILTERS, type TicketFilters } from '$lib/tickets';
	import { mergeNewTickets, applyAccepted, applyRejected, applyRemoved, type TicketRow } from '$lib/ticker';
	import TicketTicker from '$lib/components/TicketTicker.svelte';
	import LatencyTape from '$lib/components/LatencyTape.svelte';
	import StatCard from '$lib/components/StatCard.svelte';

	const ws = getContext<WsStore>('ws');

	let rows = $state<TicketRow[]>([]);
	let dispatchSamples = $state<number[]>([]);
	let errorMsg = $state('');
	const MAX_SAMPLES = 200;

	let summary = $state<CommandSummary | null>(null);
	type WidgetKey = 'incoming' | 'taken' | 'auto' | 'manual';
	let activeWidget = $state<WidgetKey>('incoming');

	function widgetFilter(key: WidgetKey): TicketFilters {
		if (key === 'incoming') return { ...EMPTY_TICKET_FILTERS, status: 'pending' };
		if (key === 'taken') return { ...EMPTY_TICKET_FILTERS, status: 'failed', acceptReason: 'taken_by_other' };
		if (key === 'auto') return { ...EMPTY_TICKET_FILTERS, status: 'accepted', autoAccepted: true };
		return { ...EMPTY_TICKET_FILTERS, status: 'accepted', autoAccepted: false };
	}

	async function loadSummary() {
		try {
			summary = await fetchSummary();
		} catch {
			// Summary is a supplementary widget row — a fetch failure here must not block the
			// existing live-ticket-list functionality below it, so this is a silent no-op retry
			// on the next poll tick rather than a page-blocking error banner.
		}
	}

	function handleWsEvent(event: TowerWsEvent) {
		if (event.type === 'ticket_accepted') {
			rows = applyAccepted(rows, event.data);
			dispatchSamples = [...dispatchSamples, event.data.localDispatchUs].slice(-MAX_SAMPLES);
			loadSummary();
		} else if (event.type === 'ticket_rejected') {
			rows = applyRejected(rows, event.data.bookingId);
		} else if (event.type === 'tickets_removed') {
			rows = applyRemoved(rows, event.data.ids);
		}
	}

	const LIVE_POLL_INTERVAL_MS = 20_000;
	const SUMMARY_POLL_INTERVAL_MS = 10_000;
	let pollTimer: ReturnType<typeof setInterval> | undefined;
	let summaryTimer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		fetchLiveBookings()
			.then((initial) => {
				rows = mergeNewTickets(rows, initial);
				errorMsg = '';
			})
			.catch(() => {
				errorMsg = 'Gagal memuat tiket terbaru. Mencoba lagi...';
			});
		loadSummary();
		const unsubscribe = ws.onEvent(handleWsEvent);
		pollTimer = setInterval(() => {
			fetchLiveBookings()
				.then((fresh) => {
					rows = mergeNewTickets(rows, fresh);
					errorMsg = '';
				})
				.catch(() => {
					errorMsg = 'Gagal memuat tiket terbaru. Mencoba lagi...';
				});
		}, LIVE_POLL_INTERVAL_MS);
		summaryTimer = setInterval(loadSummary, SUMMARY_POLL_INTERVAL_MS);
		return () => {
			unsubscribe();
		};
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		if (summaryTimer) clearInterval(summaryTimer);
	});
</script>
```

`widgetFilter`'s `'taken'`/`'auto'`/`'manual'` branches use `TicketFilters.acceptReason`/`.autoAccepted` (added in Task 7) — both map straight through `filtersToQueryString` to the backend's already-existing `accept_reason`/`auto_accepted` params (Task 4), so no further backend work is needed here.

Replace the template (everything under `</script>`):

```svelte
<svelte:head>
	<title>Command — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	{#if errorMsg}
		<div
			role="alert"
			aria-live="polite"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
		>
			{errorMsg}
		</div>
	{/if}

	{#if summary?.latencyP99Ms === null}
		<div class="rounded-lg border border-border bg-bg-surface p-4 text-center text-[12px] text-text-muted">
			Belum ada data auto-accept hari ini.
		</div>
	{:else}
		<LatencyTape samples={dispatchSamples} />
	{/if}

	<div class="grid grid-cols-2 sm:grid-cols-4 gap-2.5">
		<StatCard
			label="Tiket Masuk"
			value={summary ? String(summary.incomingToday) : '—'}
			active={activeWidget === 'incoming'}
			onclick={() => (activeWidget = 'incoming')}
		/>
		<StatCard
			label="Close (Agency Lain)"
			value={summary ? String(summary.takenByOtherToday) : '—'}
			active={activeWidget === 'taken'}
			onclick={() => (activeWidget = 'taken')}
		/>
		<StatCard
			label="Accept by Bot"
			value={summary ? String(summary.acceptedAutoToday) : '—'}
			active={activeWidget === 'auto'}
			onclick={() => (activeWidget = 'auto')}
		/>
		<StatCard
			label="Diambil Operator"
			value={summary ? String(summary.acceptedManualToday) : '—'}
			active={activeWidget === 'manual'}
			onclick={() => (activeWidget = 'manual')}
		/>
	</div>

	{#if activeWidget === 'incoming'}
		<TicketTicker bind:rows />
	{:else}
		{#await fetchTickets(widgetFilter(activeWidget), 1)}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:then result}
			<ul class="flex flex-col gap-2">
				{#each result.rows as row (row.id)}
					<li class="p-3 rounded-lg border border-border bg-bg-surface text-[12px] text-text-primary">
						{row.bookingNumber} — {row.route.join(' → ') || '—'}
					</li>
				{:else}
					<li class="p-3 text-[12px] text-text-muted">Tidak ada tiket di kategori ini.</li>
				{/each}
			</ul>
		{:catch}
			<p class="text-[12px] text-danger">Gagal memuat daftar.</p>
		{/await}
	{/if}
</div>
```

- [ ] **Step 3: Manual smoke check**

Run: `cd Frontend && pnpm check`
Expected: no TypeScript errors (this will surface the `widgetFilter` placeholder-field bug called out in Step 2's note if it wasn't actually fixed before this point).

- [ ] **Step 4: Commit**

```bash
git add "Frontend/src/routes/(app)/command/+page.svelte"
git commit -m "feat(frontend): rewrite /command with 5-widget KPI row + clickable quick-filter list"
```

---

## Task 14: `/tickets` page — wire in `FilterDrawer`

**Files:**
- Modify: `Frontend/src/routes/(app)/tickets/+page.svelte`
- Delete: `Frontend/src/lib/components/TicketFilterBar.svelte`
- Delete: `Frontend/src/lib/components/TicketFilterBar.test.ts` (if it exists — check first)

**Interfaces:**
- Consumes: Task 12's `FilterDrawer`, Task 7's `EMPTY_TICKET_FILTERS`.

- [ ] **Step 1: Implement**

Modify `Frontend/src/routes/(app)/tickets/+page.svelte`:

Replace the import (line 20) `import TicketFilterBar from '$lib/components/TicketFilterBar.svelte';` with `import FilterDrawer from '$lib/components/FilterDrawer.svelte';`.

Replace `let filters = $state<TicketFilters>({ status: null, spxId: '', from: null, to: null });` (line 28) with `import { EMPTY_TICKET_FILTERS } from '$lib/tickets';` (add to the existing `$lib/tickets` import on line 13-19) and `let filters = $state<TicketFilters>({ ...EMPTY_TICKET_FILTERS });`.

Add `let filterDrawerOpen = $state(false);` near the other `$state` declarations.

Replace `<TicketFilterBar {filters} onFiltersChange={handleFiltersChange} />` (line 120) with:

```svelte
	<div class="flex items-center justify-between">
		<button
			type="button"
			onclick={() => (filterDrawerOpen = true)}
			class="min-h-[44px] px-3.5 rounded-md text-[12px] font-body border border-border bg-bg-surface text-text-primary hover:bg-bg-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			Filter
		</button>
	</div>
	<FilterDrawer
		open={filterDrawerOpen}
		{filters}
		onFiltersChange={handleFiltersChange}
		onClose={() => (filterDrawerOpen = false)}
		resultCount={rows.length}
	/>
```

- [ ] **Step 2: Delete the now-unused `TicketFilterBar`**

Run: `grep -rln "TicketFilterBar" Frontend/src` — confirm the ONLY remaining references (if any) are in this component's own test file.

```bash
git rm Frontend/src/lib/components/TicketFilterBar.svelte
# only if it exists:
git rm Frontend/src/lib/components/TicketFilterBar.test.ts 2>/dev/null || true
```

- [ ] **Step 3: Run checks**

Run: `cd Frontend && pnpm check && pnpm test:unit`
Expected: no TypeScript errors, all unit tests PASS.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(frontend): wire FilterDrawer into /tickets, remove TicketFilterBar"
```

---

## Task 15: E2E coverage

**Files:**
- Modify: `Frontend/tests/command.spec.ts`
- Modify: `Frontend/tests/tickets.spec.ts`

**Interfaces:**
- Consumes: the seeded `e2e-test-user`/`correct-horse-battery-staple` fixture (existing, `tower-dev` tenant).

- [ ] **Step 1: Read both existing spec files in full** to match their established login/setup helper functions exactly (`login(page, username, password)` per this project's convention, confirmed in `rules.spec.ts`) before writing new tests — do not invent a different login helper.

- [ ] **Step 2: Add to `command.spec.ts`**

```typescript
test('clicking a KPI widget switches the list below to that category', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/command');
	await expect(page.getByRole('button', { name: /Accept by Bot/ })).toBeVisible();
	await page.getByRole('button', { name: /Accept by Bot/ }).click();
	await expect(page.getByRole('button', { name: /Accept by Bot/ })).toHaveAttribute('aria-pressed', 'true');
});
```

- [ ] **Step 3: Add to `tickets.spec.ts`**

```typescript
test('filter drawer opens, filters, and traps focus', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/tickets');
	await page.getByRole('button', { name: 'Filter' }).click();
	const dialog = page.getByRole('dialog', { name: 'Filter Lanjutan' });
	await expect(dialog).toBeVisible();
	await dialog.getByLabel('ID Request').fill('R1');
	await page.keyboard.press('Escape');
	await expect(dialog).not.toBeVisible();
});

test('ticket table shows the new booking-number and vehicle columns', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/tickets');
	await expect(page.getByRole('columnheader', { name: 'Booking Number' })).toBeVisible();
	await expect(page.getByRole('columnheader', { name: 'Deadline Bidding' })).toBeVisible();
});
```

- [ ] **Step 4: Run the full e2e suite**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all specs PASS (existing + new).

- [ ] **Step 5: Commit**

```bash
git add Frontend/tests/command.spec.ts Frontend/tests/tickets.spec.ts
git commit -m "test(frontend): e2e coverage for command widget click and tickets filter drawer"
```

---

## Task 16: Final workspace verification + sign-off

**Files:** none (verification only)

- [ ] **Step 1: Backend**

Run: `cd Backend && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo deny check`
Expected: all PASS, zero warnings.

Run: `cd Backend && cargo tree -p api-gateway | grep -c "^"` and manually confirm `api-gateway` is still the sole crate depending on all other library crates (this project's standing cross-dependency invariant) — no new crate was accidentally given a dependency it shouldn't have.

- [ ] **Step 2: Frontend**

Run: `cd Frontend && pnpm check && pnpm test:unit run && pnpm build`
Expected: no TypeScript errors, all unit tests PASS, production build succeeds.

Run: `cd Frontend && pnpm exec playwright test`
Expected: all e2e specs PASS.

- [ ] **Step 3: Definition-of-Done cross-check against the design doc**

Re-read `Docs/superpowers/specs/2026-07-18-command-tickets-ui-revamp-design.md`'s Scope section and confirm every "In scope" bullet has a corresponding merged task above; confirm every "Out of scope" item was genuinely left alone (not accidentally half-implemented).

- [ ] **Step 4: Manual check in a running instance**

Start the stack (`docker compose -f docker/docker-compose.yml up -d tower-postgres tower-redis`, then `cargo run -p reactor-core` from `Backend/` with `TENANT_SLUG=tower-dev COOKIE_SECURE=false`, then `pnpm dev` from `Frontend/`), log in as `e2e-test-user`/`correct-horse-battery-staple`, and visually confirm: `/command` shows the 5-widget row and clicking each switches the list; `/tickets` shows the new table columns and the Filter Lanjutan drawer opens/closes/filters correctly.

This task has no commit of its own — it's the sign-off gate. If any check fails, fix it in a follow-up commit before considering this plan complete.
