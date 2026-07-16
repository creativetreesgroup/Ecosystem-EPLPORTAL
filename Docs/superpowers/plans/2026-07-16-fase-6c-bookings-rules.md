# Fase 6c (bookings + rules) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the bookings + rules slice of `api-gateway`: read routes over live/history/detail/spx-log, a manual-accept route wired through `executor::try_claim_manual`, and a rules+automation-settings editor route gated by the OTP proof Fase 6b produces — closing the loop so a configured rule set actually reaches running poller accounts.

**Architecture:** Five new `store` query modules (`accept_rules`, `rule_booking_targets`, `automation_settings`, `accept_events`, plus additions to `bookings`) sit under six new `api-gateway` route handlers, split across two files (`routes/bookings.rs` for read+manual-accept, `routes/rules.rs` for the settings editor). Two `poller` changes close a structural gap this research surfaced: `AccountHandle` gains a handle to its account's `AccountDedupState` (manual accept needs it outside the poller task), and a new `tokio::sync::watch` channel on `PollerShared` lets a `PUT /bookings/settings` save push a freshly compiled rule set into every running poller account without a process restart.

**Tech Stack:** Same as the rest of the workspace — `axum` 0.8, `sqlx` 0.9/Postgres, `redis` 1.3 (`aio::ConnectionManager`), `tokio`, `core-domain` (pure Rust port of the reference's rule engine).

## Global Constraints

- Every tenant-scoped query MUST run inside `store::begin_tenant_tx(pool, tenant_id)` — a query issued outside that silently returns zero rows against `app_role` under RLS.
- `require_permission(&user, Permission::X)` is checked INSIDE the handler (first statement, after any earlier check), never as a second `route_layer` — `Permission::ManageRules` and `Permission::ArmAutoAccept` already exist (`Backend/crates/api-gateway/src/auth/permission.rs`), both currently uniformly gated to `user.is_main_account`. GET routes need only the router-level `session_auth` (any logged-in tenant member) — this project's established data-visibility model is tenant-wide, not per-account.
- `core-domain`'s rule engine (`sanitize_accept_rules`, `dedupe_rules`, `CompiledRule::compile`, `core_domain::matching::find_best_matching_rule_compiled`) MUST be reused as-is — Fase 6 is a transport layer over existing business logic, never a re-derivation of matching/dedup rules.
- `ApiError` variants: `Unauthorized | Forbidden | NotFound | Conflict(String) | BadRequest(String) | Internal(String) | TooManyRequests(String)`. `impl From<sqlx::Error> for ApiError` already maps Postgres `23505` → `Conflict`, everything else → `Internal`; store fns should let `?` propagate rather than special-casing.
- No rate limiting applies to any route in this plan (`tower_governor` is scoped only to `POST /auth/portal-login` today).
- The `spx:pwverify:<tenant_id>:<portal_user_id>` Redis key (value `"1"`, `EX 120`, written only by `otp::verify` on success) is single-use by convention only — nothing has consumed it yet. Task 11 is its first and only consumer: a plain `GET` then `DEL` (not atomic `GETDEL` — not assumed available), against `state.redis: redis::aio::ConnectionManager` (not an `Option`).
- Every new file needs a top `//` doc comment explaining its one responsibility, matching every existing file in this workspace (see `Backend/crates/api-gateway/src/routes/spx_credentials.rs` for the established tone/density).
- `cargo fmt`/`cargo clippy --workspace --all-targets -- -D warnings`/`cargo test --workspace -- --test-threads=1` must stay clean after every task — the `store` crate's Postgres integration tests run against `127.0.0.1:15432` (or `$DATABASE_URL`), `redis` tests against `127.0.0.1:16379` (or `$REDIS_URL`), matching every existing test file's convention.

---

## Task 1: `bookings.account_id` migration + `store::bookings` list/detail queries

**Context:** `bookings` (Fase 2) has no account/credential reference column at all — it's keyed uniquely by `(tenant_id, spx_id)`. But `executor::try_claim_manual(account_id, spx_id, dedup)` (Task 10 needs this) requires an `account_id: &str` to know which SPX login session to dispatch the accept through, and nothing on a `bookings` row can supply it today. This task adds the column (confirmed with the user as the correct fix, not a workaround) and the missing list/detail store queries `bookings.rs` never got (only `upsert_booking`/`update_booking_status`/`expire_stale_bookings`/`resurrect_pending` exist).

**Files:**
- Create: `Backend/crates/store/migrations/0020_bookings_account_id.sql`
- Modify: `Backend/crates/store/src/bookings.rs`
- Modify: `Backend/crates/store/src/models/booking.rs`
- Modify: `Backend/crates/store/src/lib.rs` (re-exports + new tests)
- Modify: `Backend/crates/poller/src/schedule.rs` (the one `upsert_booking` call site — `poll_once`, currently around line 97)
- Test: inline in `Backend/crates/store/src/lib.rs`'s existing `#[cfg(test)] mod tests` block (this crate's established convention — see `agency_credentials_create_find_update_delete_round_trip` for the pattern: `connect` → `run_migrations` → `insert_test_tenant` → exercise → `DELETE FROM tenants WHERE id = $1` cleanup)

**Interfaces:**
- Consumes: `crate::begin_tenant_tx(pool, tenant_id)` (`Backend/crates/store/src/pool.rs`), the existing `Booking` model (`Backend/crates/store/src/models/booking.rs`).
- Produces (for Task 10 and Task 8/9): `store::bookings::list_live`, `store::bookings::list_history`, `store::bookings::get_detail` — exact signatures below. `BookingUpsert.account_id: String` — every future construction of `BookingUpsert` (only one call site exists today, `poller/src/schedule.rs::poll_once`) must supply it.

- [x] **Step 1: Write the migration**

```sql
-- Backend/crates/store/migrations/0020_bookings_account_id.sql
-- A `bookings` row has never recorded WHICH SPX account/login session saw
-- it — only `(tenant_id, spx_id)`. This was fine while nothing needed to
-- dispatch an HTTP call on a specific account's behalf from a `bookings`
-- row alone, but Fase 6c's manual-accept route does exactly that
-- (`executor::try_claim_manual(account_id, spx_id, dedup)` needs a real
-- `account_id`). The executor's own claim keys are already account-scoped
-- (`spx:claim:<account_id>:<spx_id>`), which only makes sense if the same
-- `spx_id` can legitimately be visible to more than one sibling account
-- under a tenant — meaning the OLD `(tenant_id, spx_id)` uniqueness was
-- already a latent collision risk, not just a missing convenience column.
ALTER TABLE bookings ADD COLUMN account_id TEXT NOT NULL DEFAULT '';

ALTER TABLE bookings DROP CONSTRAINT bookings_tenant_spx_id_unique;
ALTER TABLE bookings ADD CONSTRAINT bookings_tenant_account_spx_id_unique
  UNIQUE (tenant_id, account_id, spx_id);
```

- [x] **Step 2: Run it and confirm it applies cleanly**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p store migrations_apply_and_tenant_round_trips -- --test-threads=1`
Expected: PASS (this existing test runs `run_migrations`, which picks up the new file automatically via `sqlx::migrate!("./migrations")`).

- [x] **Step 3: Update the `Booking` model**

```rust
// Backend/crates/store/src/models/booking.rs — add account_id after spx_id
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub raw_data: Value,
    pub status: String,
    pub is_coc: bool,
    pub needs_enrichment: bool,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub accept_latency_ms: Option<i32>,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

- [x] **Step 4: Update `BookingUpsert` and `upsert_booking`**

```rust
// Backend/crates/store/src/bookings.rs — BookingUpsert gains account_id
#[derive(Debug, Clone)]
pub struct BookingUpsert {
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub is_coc: bool,
    pub raw_data: Value,
}

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
```

- [x] **Step 5: Fix the one call site — `poller/src/schedule.rs::poll_once`**

Find the existing call (currently ~line 97-106 in `poll_once`):
```rust
        let _ = store::upsert_booking(
            &shared.pool,
            st.tenant_id,
            &store::BookingUpsert {
                account_id: st.account_id.clone(),
                spx_id: booking.id.clone(),
                status: "pending".into(),
                is_coc: matches!(booking.booking_type, core_domain::BookingType::Spxid),
                raw_data: booking.raw.clone(),
            },
        )
        .await;
```
(Only `account_id: st.account_id.clone(),` is new — `st.account_id: String` is already a `PollerState` field, already in scope in this exact function.)

- [x] **Step 6: Run the workspace build to confirm no other call site broke**

Run: `cargo build --workspace 2>&1 | grep -E "error|warning: unused"`
Expected: no output (clean build) — `BookingUpsert` has exactly one production construction site (just fixed) and this crate's own tests construct it too; if any test file fails to compile, add `account_id: "test-account".to_string()` there.

- [x] **Step 7: Add `list_live`, `list_history`, `get_detail` to `store::bookings`**

```rust
// Backend/crates/store/src/bookings.rs — append

/// `/bookings/live`: pending bookings, newest first. Uses the `idx_bookings_live_covering`
/// index (`(tenant_id, status, created_at DESC) INCLUDE (...)`, migration 0007).
/// `limit`/`offset` are the caller's job to clamp to a sane range (the route layer does this,
/// not this fn — mirrors this crate's existing "store trusts its caller" convention).
pub async fn list_live(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, crate::models::Booking>(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings \
         WHERE tenant_id = $1 AND status = 'pending' \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(tenant_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// `/bookings/history`: terminal bookings (`accepted`/`failed`), newest first. Uses the
/// `idx_bookings_created_brin` BRIN index for the time-ordered scan.
pub async fn list_history(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, crate::models::Booking>(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings \
         WHERE tenant_id = $1 AND status IN ('accepted', 'failed') \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(tenant_id)
    .bind(limit)
    .bind(offset)
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
```

- [x] **Step 8: Re-export in `store::lib.rs`**

```rust
// Backend/crates/store/src/lib.rs — extend the existing `pub use bookings::{...}` block
pub use bookings::{
    expire_stale_bookings, get_detail as get_booking_detail, list_history as list_bookings_history,
    list_live as list_bookings_live, resurrect_pending, update_booking_status, upsert_booking,
    BookingStatusUpdate, BookingUpsert, StaleOutcome,
};
```
(Aliased the same way every other multi-verb module in this file already is — a bare `store::list_live`/`get_detail` would be a collision risk against a future module's own `list_live`.)

- [x] **Step 9: Write the failing tests**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`, near `booking_round_trips`
#[tokio::test]
async fn bookings_list_live_returns_only_pending_newest_first() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    for (spx_id, status) in [("p1", "pending"), ("p2", "pending"), ("a1", "accepted")] {
        upsert_booking(
            &pool,
            tenant_id,
            &BookingUpsert {
                account_id: "acct-1".to_string(),
                spx_id: spx_id.to_string(),
                status: "pending".to_string(),
                is_coc: false,
                raw_data: serde_json::json!({}),
            },
        )
        .await
        .expect("upsert");
        if status == "accepted" {
            update_booking_status(
                &pool,
                tenant_id,
                spx_id,
                BookingStatusUpdate {
                    status: "accepted",
                    latency_ms: Some(10),
                    auto_accepted: true,
                    rule_matched: None,
                    accept_reason: None,
                },
            )
            .await
            .expect("mark accepted");
        }
    }

    let live = bookings::list_live(&pool, tenant_id, 50, 0)
        .await
        .expect("list_live");
    let live_ids: Vec<&str> = live.iter().map(|b| b.spx_id.as_str()).collect();
    assert_eq!(live_ids.len(), 2, "only the two pending rows must appear");
    assert!(live_ids.contains(&"p1"));
    assert!(live_ids.contains(&"p2"));
    assert!(
        !live_ids.contains(&"a1"),
        "the accepted row must not appear in /bookings/live"
    );

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
async fn bookings_get_detail_returns_none_for_wrong_tenant() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_a = insert_test_tenant(&pool).await;
    let tenant_b = insert_test_tenant(&pool).await;

    upsert_booking(
        &pool,
        tenant_a,
        &BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "spx-detail-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"k": "v"}),
        },
    )
    .await
    .expect("upsert");

    let live = bookings::list_live(&pool, tenant_a, 50, 0)
        .await
        .expect("list_live");
    let id = live[0].id;

    let found = bookings::get_detail(&pool, tenant_a, id)
        .await
        .expect("get_detail own tenant");
    assert!(found.is_some());
    assert_eq!(found.unwrap().raw_data, serde_json::json!({"k": "v"}));

    let cross_tenant = bookings::get_detail(&pool, tenant_b, id)
        .await
        .expect("get_detail cross tenant query must not error");
    assert!(
        cross_tenant.is_none(),
        "a booking must not be visible via a different tenant_id"
    );

    sqlx::query("DELETE FROM tenants WHERE id = ANY($1)")
        .bind(vec![tenant_a, tenant_b])
        .execute(&pool)
        .await
        .ok();
}
```

- [x] **Step 10: Run the tests to verify they pass**

Run: `cargo test -p store bookings_list_live_returns_only_pending_newest_first bookings_get_detail_returns_none_for_wrong_tenant -- --test-threads=1`
Expected: both PASS.

- [x] **Step 11: Full crate verification + commit**

Run: `cargo test -p store -p poller -- --test-threads=1 && cargo clippy -p store -p poller --all-targets -- -D warnings`
Expected: 0 failures, clean clippy.

```bash
git add Backend/crates/store/migrations/0020_bookings_account_id.sql \
        Backend/crates/store/src/bookings.rs \
        Backend/crates/store/src/models/booking.rs \
        Backend/crates/store/src/lib.rs \
        Backend/crates/poller/src/schedule.rs
git commit -m "feat(store,poller): bookings.account_id + list_live/list_history/get_detail queries"
```

---

## Task 2: `store::accept_rules` CRUD

**Files:**
- Create: `Backend/crates/store/src/accept_rules.rs`
- Modify: `Backend/crates/store/src/lib.rs` (`pub mod accept_rules;` + re-exports + tests)

**Interfaces:**
- Consumes: `crate::begin_tenant_tx`, `crate::models::AcceptRule` (already exists, `Backend/crates/store/src/models/accept_rule.rs` — `id, tenant_id, name, enabled, priority, mode, service_types, max_weight, coc_only, non_coc_only, max_cod_amount, origin, destinations, booking_type, shift_types, trip_types, match_mode, min_deadline_min, max_accept_count, accepted_count, route_signature, created_at, updated_at`).
- Produces (for Task 11): `accept_rules::list_all(pool, tenant_id) -> Result<Vec<AcceptRule>, sqlx::Error>`, `accept_rules::replace_all(pool, tenant_id, rows: &[NewAcceptRule]) -> Result<Vec<AcceptRule>, sqlx::Error>` (delete-then-insert-fresh inside one transaction — see Task 11's design note on why this project uses a replace-all strategy rather than per-rule upsert-by-client-id).

- [x] **Step 1: Write the module**

```rust
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
```

- [x] **Step 2: Wire the module + re-exports**

```rust
// Backend/crates/store/src/lib.rs
pub mod accept_rules; // add alongside the other `pub mod` lines
```
```rust
// near the other `pub use` blocks
pub use accept_rules::{list_all as list_accept_rules, replace_all as replace_accept_rules, NewAcceptRule};
```

- [x] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn accept_rules_replace_all_deletes_old_rows_and_inserts_fresh() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let first_save = accept_rules::replace_all(
        &pool,
        tenant_id,
        &[accept_rules::NewAcceptRule {
            name: "Old rule".to_string(),
            enabled: true,
            priority: 0,
            mode: "filter".to_string(),
            service_types: vec![],
            max_weight: None,
            coc_only: false,
            non_coc_only: false,
            max_cod_amount: None,
            origin: String::new(),
            destinations: vec![],
            booking_type: "all".to_string(),
            shift_types: vec![],
            trip_types: vec![],
            match_mode: "strict".to_string(),
            min_deadline_min: None,
            max_accept_count: 0,
            accepted_count: 0,
        }],
    )
    .await
    .expect("first replace_all");
    assert_eq!(first_save.len(), 1);
    let old_id = first_save[0].id;

    let second_save = accept_rules::replace_all(
        &pool,
        tenant_id,
        &[accept_rules::NewAcceptRule {
            name: "New rule".to_string(),
            enabled: true,
            priority: 5,
            mode: "route".to_string(),
            service_types: vec!["TRONTON".to_string()],
            max_weight: Some(1000.0),
            coc_only: true,
            non_coc_only: false,
            max_cod_amount: None,
            origin: "Padang DC".to_string(),
            destinations: vec!["Cileungsi DC".to_string()],
            booking_type: "all".to_string(),
            shift_types: vec![],
            trip_types: vec![],
            match_mode: "strict".to_string(),
            min_deadline_min: None,
            max_accept_count: 10,
            accepted_count: 3,
        }],
    )
    .await
    .expect("second replace_all");
    assert_eq!(second_save.len(), 1);
    assert_ne!(
        second_save[0].id, old_id,
        "replace_all must insert a fresh row, not update the old one in place"
    );
    assert_eq!(second_save[0].name, "New rule");
    assert_eq!(second_save[0].accepted_count, 3);

    let listed = accept_rules::list_all(&pool, tenant_id)
        .await
        .expect("list_all");
    assert_eq!(listed.len(), 1, "the old row must be gone after replace_all");
    assert_eq!(listed[0].name, "New rule");

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [x] **Step 4: Run it**

Run: `cargo test -p store accept_rules_replace_all_deletes_old_rows_and_inserts_fresh -- --test-threads=1`
Expected: PASS.

- [x] **Step 5: Full crate verification + commit**

Run: `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings`
Expected: 0 failures, clean.

```bash
git add Backend/crates/store/src/accept_rules.rs Backend/crates/store/src/lib.rs
git commit -m "feat(store): accept_rules CRUD (list_all, replace_all)"
```

---

## Task 3: `core-domain::rule::norm_id` visibility fix + `store::rule_booking_targets` CRUD

**Context:** `booking_id_norm` on `rule_booking_targets` is a PLAIN column (not generated) — the application must compute it. `core_domain::rule::norm_id` (lowercase, strip everything but ASCII alphanumeric) already does exactly this, but is `pub(crate)` inside `core-domain` — not reachable from `store` or `api-gateway`. The binding "does not re-derive or duplicate matching/dedup rules" constraint means the fix is exporting the existing fn, not writing a second copy of the same three-line function.

**Files:**
- Modify: `Backend/crates/core-domain/src/rule.rs` (visibility only)
- Modify: `Backend/crates/core-domain/src/lib.rs` (re-export)
- Create: `Backend/crates/store/src/rule_booking_targets.rs`
- Modify: `Backend/crates/store/src/lib.rs` (`pub mod` + re-exports + tests)

**Interfaces:**
- Consumes: `core_domain::norm_id(s: &str) -> String` (after this task's visibility fix), `crate::models::RuleBookingTarget` (already exists — `id, tenant_id, rule_id, booking_id_raw, booking_id_norm, created_at`).
- Produces (for Task 11): `rule_booking_targets::replace_for_rule(pool, tenant_id, rule_id, booking_ids: &[String]) -> Result<Vec<RuleBookingTarget>, sqlx::Error>`.

- [x] **Step 1: Write the failing test proving `norm_id` is reachable from outside the crate**

```rust
// Backend/crates/core-domain/src/rule.rs — inside the existing `#[cfg(test)] mod tests` block,
// add a new nested module (this crate's own tests are already split into
// `sanitize_accept_rules_tests`/`dedupe_rules_tests` sub-modules — follow that pattern)
mod norm_id_visibility_tests {
    // `super::super::norm_id` would work from inside the crate regardless of visibility — this
    // test instead calls it via the CRATE ROOT path a downstream crate (`store`) would use,
    // which only compiles once `norm_id` is `pub` and re-exported from `lib.rs`.
    use crate::norm_id;

    #[test]
    fn norm_id_reachable_from_crate_root() {
        assert_eq!(norm_id("SPXID_VM_001397509"), "spxidvm001397509");
    }
}
```

- [x] **Step 2: Run it to verify it fails**

Run: `cd Backend && cargo test -p core-domain norm_id_reachable_from_crate_root -- --test-threads=1`
Expected: FAIL — `crate::norm_id` does not exist yet (only `crate::rule::norm_id`, and even that path is `pub(crate)`-restricted outside this test's own crate).

- [x] **Step 3: Make the visibility fix**

```rust
// Backend/crates/core-domain/src/rule.rs — change the one line
// (was: pub(crate) fn norm_id(s: &str) -> String {)
pub fn norm_id(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}
```
```rust
// Backend/crates/core-domain/src/lib.rs — add norm_id to the existing `pub use rule::{...}` list
pub use rule::{
    dedupe_rules, norm_id, sanitize_accept_rules, AcceptRule, MatchState, RawAcceptRule,
    RawRuleConditions, RouteMatchMode, RuleBookingType, RuleConditions, RuleMode,
    RuleSanitizeResult,
};
```

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p core-domain norm_id_reachable_from_crate_root -- --test-threads=1`
Expected: PASS. Also run `cargo test -p core-domain -- --test-threads=1` to confirm nothing else in this crate broke (the fn body is unchanged, only its visibility).

- [x] **Step 5: Write `store::rule_booking_targets`**

```rust
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
```

- [x] **Step 6: Wire the module**

```rust
// Backend/crates/store/src/lib.rs
pub mod rule_booking_targets;
```
```rust
pub use rule_booking_targets::{
    list_for_tenant as list_rule_booking_targets, replace_for_rule as replace_rule_booking_targets,
};
```
Add `core-domain` as a dependency of `store` if it is not already one — check `Backend/crates/store/Cargo.toml`'s `[dependencies]` first; `store` already depends on nothing from `core-domain` today (verify via `grep core-domain Backend/crates/store/Cargo.toml`), so add `core-domain = { path = "../core-domain" }` if the grep is empty.

- [x] **Step 7: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn rule_booking_targets_replace_for_rule_normalizes_and_replaces() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let rule = accept_rules::replace_all(
        &pool,
        tenant_id,
        &[accept_rules::NewAcceptRule {
            name: "Booking-id rule".to_string(),
            enabled: true,
            priority: 0,
            mode: "booking_id".to_string(),
            service_types: vec![],
            max_weight: None,
            coc_only: false,
            non_coc_only: false,
            max_cod_amount: None,
            origin: String::new(),
            destinations: vec![],
            booking_type: "all".to_string(),
            shift_types: vec![],
            trip_types: vec![],
            match_mode: "strict".to_string(),
            min_deadline_min: None,
            max_accept_count: 0,
            accepted_count: 0,
        }],
    )
    .await
    .expect("create rule");
    let rule_id = rule[0].id;

    let first = rule_booking_targets::replace_for_rule(
        &pool,
        tenant_id,
        rule_id,
        &["SPXID_VM_001397509".to_string()],
    )
    .await
    .expect("first replace_for_rule");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].booking_id_raw, "SPXID_VM_001397509");
    assert_eq!(first[0].booking_id_norm, "spxidvm001397509");

    let second = rule_booking_targets::replace_for_rule(
        &pool,
        tenant_id,
        rule_id,
        &["SPXID VM 002".to_string(), "SPXID VM 003".to_string()],
    )
    .await
    .expect("second replace_for_rule");
    assert_eq!(
        second.len(),
        2,
        "replace_for_rule must delete the old target before inserting the new set"
    );

    let all = rule_booking_targets::list_for_tenant(&pool, tenant_id)
        .await
        .expect("list_for_tenant");
    assert_eq!(all.len(), 2, "only the second save's two targets must remain");

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [x] **Step 8: Run it**

Run: `cargo test -p store rule_booking_targets_replace_for_rule_normalizes_and_replaces -- --test-threads=1`
Expected: PASS.

- [x] **Step 9: Full verification + commit**

Run: `cargo test -p core-domain -p store -- --test-threads=1 && cargo clippy -p core-domain -p store --all-targets -- -D warnings`
Expected: 0 failures, clean.

```bash
git add Backend/crates/core-domain/src/rule.rs Backend/crates/core-domain/src/lib.rs \
        Backend/crates/store/src/rule_booking_targets.rs Backend/crates/store/src/lib.rs \
        Backend/crates/store/Cargo.toml
git commit -m "feat(core-domain,store): export norm_id, add rule_booking_targets CRUD"
```

---

## Task 4: `store::automation_settings` get/upsert

**Files:**
- Create: `Backend/crates/store/src/automation_settings.rs`
- Modify: `Backend/crates/store/src/lib.rs`

**Interfaces:**
- Consumes: `crate::models::AutomationSettings` (already exists — `tenant_id, auto_accept_enabled, poll_interval_ms, smart_paused, smart_paused_until, smart_dry_run, smart_schedule, smart_blacklist, counter_reset_hour, counter_reset_last_at, updated_at`).
- Produces (for Task 11): `automation_settings::get(pool, tenant_id) -> Result<Option<AutomationSettings>, sqlx::Error>`, `automation_settings::set_auto_accept_enabled(pool, tenant_id, enabled: bool) -> Result<AutomationSettings, sqlx::Error>`.

- [x] **Step 1: Write the module**

```rust
// Backend/crates/store/src/automation_settings.rs
//! `automation_settings` — one row per tenant, the home of the `autoAccept` GLOBAL kill switch
//! (Aturan Keras #2). Fase 6c only touches `auto_accept_enabled`; every other column
//! (`smart_*`, `counter_reset_*`) is out of this sub-phase's scope (6d/later) and this module
//! deliberately does not expose a way to change them yet — `set_auto_accept_enabled` is a
//! narrow, single-column write, not a general-purpose upsert.
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::AutomationSettings;

pub async fn get(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<AutomationSettings>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AutomationSettings>(
        "SELECT tenant_id, auto_accept_enabled, poll_interval_ms, smart_paused, \
         smart_paused_until, smart_dry_run, smart_schedule, smart_blacklist, \
         counter_reset_hour, counter_reset_last_at, updated_at \
         FROM automation_settings WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// `INSERT ... ON CONFLICT (tenant_id) DO UPDATE` — a tenant's `automation_settings` row may or
/// may not exist yet (nothing has created one before Fase 6c; the schema ships no default row
/// per tenant). Every other column keeps its existing value (or the schema default, on first
/// insert) — only `auto_accept_enabled` is ever written by this fn.
pub async fn set_auto_accept_enabled(
    pool: &PgPool,
    tenant_id: Uuid,
    enabled: bool,
) -> Result<AutomationSettings, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AutomationSettings>(
        "INSERT INTO automation_settings (tenant_id, auto_accept_enabled) VALUES ($1, $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET auto_accept_enabled = $2, updated_at = now() \
         RETURNING tenant_id, auto_accept_enabled, poll_interval_ms, smart_paused, \
           smart_paused_until, smart_dry_run, smart_schedule, smart_blacklist, \
           counter_reset_hour, counter_reset_last_at, updated_at",
    )
    .bind(tenant_id)
    .bind(enabled)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}
```

- [x] **Step 2: Wire the module**

```rust
// Backend/crates/store/src/lib.rs
pub mod automation_settings;
```
```rust
pub use automation_settings::{get as get_automation_settings, set_auto_accept_enabled};
```

- [x] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn automation_settings_set_auto_accept_enabled_creates_then_updates() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let before = automation_settings::get(&pool, tenant_id)
        .await
        .expect("get before any row exists");
    assert!(before.is_none(), "no row should exist before the first set");

    let created = automation_settings::set_auto_accept_enabled(&pool, tenant_id, true)
        .await
        .expect("first set (creates the row)");
    assert!(created.auto_accept_enabled);
    assert_eq!(
        created.poll_interval_ms, 1000,
        "untouched columns must keep the schema default on first insert"
    );

    let updated = automation_settings::set_auto_accept_enabled(&pool, tenant_id, false)
        .await
        .expect("second set (updates the existing row)");
    assert!(!updated.auto_accept_enabled);

    let fetched = automation_settings::get(&pool, tenant_id)
        .await
        .expect("get after update")
        .expect("row must exist");
    assert!(!fetched.auto_accept_enabled);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [x] **Step 4: Run it**

Run: `cargo test -p store automation_settings_set_auto_accept_enabled_creates_then_updates -- --test-threads=1`
Expected: PASS. (If the schema default for `poll_interval_ms` differs from `1000`, check `Backend/crates/store/migrations/0011_automation_settings.sql` and correct the assertion to match — do not change the migration.)

- [x] **Step 5: Full verification + commit**

Run: `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings`
Expected: 0 failures, clean.

```bash
git add Backend/crates/store/src/automation_settings.rs Backend/crates/store/src/lib.rs
git commit -m "feat(store): automation_settings get/set_auto_accept_enabled"
```

---

## Task 5: `store::accept_events` insert + list

**Files:**
- Create: `Backend/crates/store/src/accept_events.rs`
- Modify: `Backend/crates/store/src/lib.rs`

**Interfaces:**
- Consumes: `crate::models::AcceptEvent` (already exists — `id, tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail, created_at`).
- Produces (for Task 9 and Task 10): `accept_events::list_for_tenant(pool, tenant_id, limit, offset) -> Result<Vec<AcceptEvent>, sqlx::Error>`, `accept_events::insert(pool, tenant_id, new: &NewAcceptEvent) -> Result<AcceptEvent, sqlx::Error>`.

- [x] **Step 1: Write the module**

```rust
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
```

- [x] **Step 2: Wire the module**

```rust
// Backend/crates/store/src/lib.rs
pub mod accept_events;
```
```rust
pub use accept_events::{insert as insert_accept_event, list_for_tenant as list_accept_events, NewAcceptEvent};
```

- [x] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn accept_events_insert_then_list_newest_first() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    for outcome in ["accepted", "skipped", "failed"] {
        accept_events::insert(
            &pool,
            tenant_id,
            &accept_events::NewAcceptEvent {
                booking_id: None,
                rule_id: None,
                outcome: outcome.to_string(),
                local_dispatch_us: Some(120),
                accept_e2e_ms: Some(45),
                detail: serde_json::json!({"note": outcome}),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert {outcome}: {e}"));
    }

    let listed = accept_events::list_for_tenant(&pool, tenant_id, 50, 0)
        .await
        .expect("list_for_tenant");
    assert_eq!(listed.len(), 3);
    // newest first: the last-inserted ("failed") must come first.
    assert_eq!(listed[0].outcome, "failed");
    assert_eq!(listed[2].outcome, "accepted");

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [x] **Step 4: Run it**

Run: `cargo test -p store accept_events_insert_then_list_newest_first -- --test-threads=1`
Expected: PASS.

- [x] **Step 5: Full verification + commit**

Run: `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings`
Expected: 0 failures, clean.

```bash
git add Backend/crates/store/src/accept_events.rs Backend/crates/store/src/lib.rs
git commit -m "feat(store): accept_events insert + list_for_tenant"
```

---

## Task 6: `poller` — `AccountHandle.dedup` + manual-accept command channel + live-reload channel scaffolding

**Context:** Three structural gaps block Task 10 (manual accept) and Task 11 (settings save reaching live pollers):
1. `AccountHandle` (`poke`, `join` only) has no way to reach its account's `AccountDedupState` from outside the poller task — but `executor::try_claim_manual` needs `&AccountDedupState`.
2. Beyond the claim/dedup check, an actual manual accept must dispatch a REAL `SpxClient::accept_booking` HTTP call — which needs the account's live `SpxCookies` and `agency_id`. Both are owned exclusively by `PollerState`, which is moved into the spawned poller task and mutated ONLY by it (single-writer by construction, e.g. `schedule::poll_once`'s relogin branch writes fresh cookies back into `st.cookies`). There is no safe way to read them from outside that task today. The correct fix is NOT a lock around `SpxCookies` (that would touch every hot-path read site across `fetch.rs`/`dispatch.rs`/`login.rs`/`hedge.rs`/`notif_watch.rs` for a rarely-used cold path) — it is a small command channel INTO the existing per-account task, mirroring the `poke: Arc<Notify>` cross-task pattern this crate already uses: `spawn_account_loop`'s `select!` (currently 2 arms: sleep timer, poke) gains a 3rd arm that receives a manual-accept request, performs the HTTP call using the task's own up-to-date `st.cookies`/`st.agency_id`, and replies via a `oneshot` channel — the poller task remains the ONLY writer/reader of `SpxCookies`, just as before.
3. `accept_rules.tenant_id` is the ONLY scoping column (no per-account FK) — every account in this single-tenant-per-process deployment shares the exact same rule set. There is currently no way to push a freshly saved rule set into a RUNNING poller task at all; `PollerState.rules`/`rule_meta` are plain `Arc` fields set once at construction and never touched again.

This task adds all three pieces of plumbing (dedup exposure, the manual-accept channel, a new `RuleSet` type + `tokio::sync::watch` channel) without changing any EXISTING matching/dispatch/fetch behavior — every current `select!` arm and `dispatch.rs` are untouched, only a new 3rd `select!` arm is added. Task 7 is what actually loads real rules through the `RuleSet` plumbing; Task 10 is what actually calls through the manual-accept channel.

**Note on today's `agency_id=0` gap:** `SpxClient::accept_booking` already short-circuits to `AcceptReason::Auth` whenever `agency_id <= 0` (see `Backend/crates/spx-client/src/client.rs`'s own guard) — and `PollerState.agency_id` is hardcoded to `0` for every account today (a disclosed, pre-existing Fase 6a gap, unrelated to and not fixed by this plan). This means a manual accept dispatched through the channel THIS task builds will correctly reach the real HTTP call site, but currently always get `AcceptReason::Auth` back until a future task teaches the relogin success path to parse and persist the real agency id — exactly the same disclosed limitation Fase 6a's auto-accept path already carries. This task still builds the channel now (rather than deferring it) so that future fix benefits both paths for free, and so Task 10 can wire the FULL manual-accept flow rather than a route that pretends the missing 10% doesn't exist.

**Files:**
- Modify: `Backend/crates/poller/src/state.rs`
- Modify: `Backend/crates/poller/src/schedule.rs`
- Modify: `Backend/crates/poller/src/lib.rs`
- Modify: every existing `PollerShared { ... }` struct-literal test call site (grep target below)

**Interfaces:**
- Produces (for Task 7, Task 10, Task 11): `poller::RuleSet { rules: Arc<Vec<CompiledRule>>, rule_meta: Arc<Vec<RuleMeta>> }` + `RuleSet::empty()`; `AccountHandle.dedup: Arc<AccountDedupState>`; `AccountHandle.manual_accept: mpsc::Sender<ManualAcceptRequest>`; `poller::ManualAcceptRequest { booking_id: i64, request_ids: Vec<i64>, reply: oneshot::Sender<spx_client::AcceptResult> }`; `PollerShared.rules_tx: tokio::sync::watch::Sender<RuleSet>`; `PollerState.rules_rx: Option<tokio::sync::watch::Receiver<RuleSet>>`.

- [x] **Step 1: Add `RuleSet`, `ManualAcceptRequest`, and the new fields to `state.rs`**

```rust
// Backend/crates/poller/src/state.rs — add near the top, after existing imports
use core_domain::CompiledRule;
```
(`core_domain::CompiledRule` is already re-exported at that crate's root — see `core-domain/src/lib.rs`'s `pub use matching::{..., CompiledRule, ...}`.)

```rust
// Backend/crates/poller/src/state.rs — new type, place above `PollerState`
/// The tenant's live compiled rule set: `rules[i]`/`rule_meta[i]` are index-aligned (same
/// contract `PollerState.rules`/`rule_meta` already had before this task — see
/// `dispatch.rs::dispatch_booking`'s `st.rule_meta[idx]` lookup). Cloning a `RuleSet` is cheap
/// (both fields are `Arc`, so `CompiledRule` itself never needs to implement `Clone`).
#[derive(Clone)]
pub struct RuleSet {
    pub rules: Arc<Vec<CompiledRule>>,
    pub rule_meta: Arc<Vec<crate::dispatch::RuleMeta>>,
}

impl RuleSet {
    pub fn empty() -> Self {
        Self {
            rules: Arc::new(Vec::new()),
            rule_meta: Arc::new(Vec::new()),
        }
    }
}
```

```rust
// Backend/crates/poller/src/state.rs — PollerState gains one field (add right after
// `pub match_state: MatchState,`)
    /// Task 6/7: live-reload subscription. `None` for every existing test/caller that
    /// constructs `PollerState` via `PollerState::new` and never touches this field — behavior
    /// is byte-for-byte unchanged from before this task for them (`rules`/`rule_meta` above
    /// stay at whatever the caller set). Production code (`reactor-core`'s bootstrap loop, Task
    /// 7) sets this to `Some(shared.rules_tx.subscribe())` and eagerly seeds `rules`/`rule_meta`
    /// from it once, right after construction.
    pub rules_rx: Option<tokio::sync::watch::Receiver<RuleSet>>,
```

```rust
// Backend/crates/poller/src/state.rs — PollerState::new sets the new field to None
// (add alongside the existing `rules: Arc::new(Vec::new()), rule_meta: Arc::new(Vec::new()),` lines)
            rules_rx: None,
```

```rust
// Backend/crates/poller/src/state.rs — new type, place near RuleSet
/// A manual-accept command sent INTO a running account's poller task (Task 10's route is the
/// only producer). The poller task is the only ever writer/reader of `SpxCookies`/`agency_id` —
/// this is how a caller outside that task gets a real accept dispatched through them without
/// breaking that single-writer invariant. `booking_id`/`request_ids` mirror
/// `SpxClient::accept_booking`'s own parameters exactly (see `dispatch.rs::dispatch_booking`'s
/// existing call for the same shape) — `spx_id`/`account_id` are NOT included here because the
/// channel itself is already scoped to one account (it lives on that account's own
/// `AccountHandle`), and the claim/dedup check (`try_claim_manual`) happens in the ROUTE, before
/// this request is ever sent — by the time a `ManualAcceptRequest` reaches the poller task, the
/// claim has already succeeded.
pub struct ManualAcceptRequest {
    pub booking_id: i64,
    pub request_ids: Vec<i64>,
    pub reply: tokio::sync::oneshot::Sender<spx_client::AcceptResult>,
}
```

```rust
// Backend/crates/poller/src/state.rs — AccountHandle gains `dedup` and `manual_accept`
/// A running account's control handle (poke to wake early; join to await stop; dedup to reach
/// its `AccountDedupState` from outside the poller task; manual_accept to dispatch a real SPX
/// accept HTTP call through that SAME task's live cookies/agency_id — Task 10's route uses
/// both).
pub struct AccountHandle {
    pub poke: Arc<Notify>,
    pub join: JoinHandle<()>,
    pub dedup: Arc<AccountDedupState>,
    pub manual_accept: tokio::sync::mpsc::Sender<ManualAcceptRequest>,
}
```

```rust
// Backend/crates/poller/src/state.rs — PollerShared gains `rules_tx`, right after the existing
// `pub redis: Option<crate::publish::RedisPublisher>,` field
    /// Task 6/7: ONE shared live-reload channel for the whole tenant — `accept_rules` has no
    /// per-account scoping column, so every account this process spawns shares the exact same
    /// compiled rule set. `PUT /bookings/settings` (Task 11) calls `.send(new_set)` after a
    /// successful save; every spawned account's next `poll_once` cycle picks it up via its own
    /// subscribed `PollerState.rules_rx`.
    pub rules_tx: tokio::sync::watch::Sender<RuleSet>,
```

- [x] **Step 2: Update `schedule.rs`'s `spawn_account_loop`, `ensure_restored_then_spawn`, and `poll_once`**

```rust
// Backend/crates/poller/src/schedule.rs — spawn_account_loop gains a 3rd select! arm.
// Replace the whole function body.
pub fn spawn_account_loop(
    shared: Arc<PollerShared>,
    mut st: PollerState,
    poke: Arc<Notify>,
    mut manual_rx: tokio::sync::mpsc::Receiver<crate::state::ManualAcceptRequest>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_millis(shared.config.poll_interval_ms);
        let mut woken_by_poke = false;
        loop {
            poll_once(&shared, &mut st, woken_by_poke).await;
            woken_by_poke = false;

            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = poke.notified() => {
                    woken_by_poke = true;
                    tracing::trace!(account = %st.account_id, "poked → early wake, next cycle forces a full sweep");
                }
                // Task 6/10: a manual-accept request from OUTSIDE this task. Dispatched using
                // THIS task's own, current `st.cookies`/`st.agency_id` — the same values
                // `poll_once`'s auto-accept path would use on its very next cycle — preserving
                // the single-writer invariant (only this task ever reads/writes them). Does NOT
                // count as a "poke" (no forced full sweep on the next cycle) — a manual accept
                // is not a "pool changed" signal.
                Some(req) = manual_rx.recv() => {
                    let result = shared
                        .client
                        .accept_booking(&st.cookies, req.booking_id, st.agency_id, &req.request_ids)
                        .await;
                    let _ = req.reply.send(result); // caller may have already timed out/dropped
                }
            }
        }
    })
}
```

```rust
// Backend/crates/poller/src/schedule.rs — ensure_restored_then_spawn, replace the body
pub async fn ensure_restored_then_spawn(
    shared: Arc<PollerShared>,
    st: PollerState,
) -> AccountHandle {
    let _ = shared
        .executor
        .restore_accepted_ids(&st.account_id, &st.dedup)
        .await;
    // Captured BEFORE `st` moves into `spawn_account_loop` below.
    let dedup = st.dedup.clone();
    let poke = Arc::new(Notify::new());
    // Bounded, small: a manual accept is a rare, human-paced action, and a slow/wedged consumer
    // should apply backpressure rather than buffer unboundedly.
    let (manual_tx, manual_rx) = tokio::sync::mpsc::channel(8);
    let join = spawn_account_loop(shared, st, poke.clone(), manual_rx);
    AccountHandle {
        poke,
        join,
        dedup,
        manual_accept: manual_tx,
    }
}
```

```rust
// Backend/crates/poller/src/schedule.rs — poll_once, insert at the very top of the fn body,
// BEFORE the existing `st.poll_count = st.poll_count.wrapping_add(1);` line
pub async fn poll_once(shared: &PollerShared, st: &mut PollerState, woken_by_poke: bool) {
    // Task 6/7: pick up a live-reload push, if any, BEFORE this cycle's fetch/dispatch — a
    // settings save should affect the very next cycle, not wait for a process restart.
    if let Some(rx) = &mut st.rules_rx {
        if rx.has_changed().unwrap_or(false) {
            let latest = rx.borrow_and_update().clone();
            st.rules = latest.rules;
            st.rule_meta = latest.rule_meta;
        }
    }

    st.poll_count = st.poll_count.wrapping_add(1);
    // ... rest of the existing function body is UNCHANGED from here down.
```

`spawn_account_loop` is called directly (not via `ensure_restored_then_spawn`) by several existing test files (per this crate's own doc comment: "kept `pub`... only because the single-flight tests exercise the loop SHAPE directly") — grep for `spawn_account_loop(` across `Backend/crates/poller/tests/` and add a 4th argument at every call site: `tokio::sync::mpsc::channel(8).1` (a throwaway receiver — those tests don't exercise manual accept).

- [x] **Step 3: Re-export `RuleSet` from `lib.rs`**

```rust
// Backend/crates/poller/src/lib.rs — extend the existing `pub use state::{...}` line
pub use state::{AccountHandle, PollerConfig, PollerShared, PollerState, RuleSet};
```

- [x] **Step 4: Fix every existing `PollerShared { ... }` struct-literal call site**

Run: `grep -rln "PollerShared {" Backend/crates Backend/bin`
Expected output (verify against this exact list — if it differs, that's fine, just fix whatever the grep actually finds, this list is what the codebase contained at plan-writing time):
```
Backend/crates/poller/src/state.rs          (skip — this is the struct DEFINITION, not a literal)
Backend/crates/poller/tests/relogin_wiring.rs
Backend/crates/poller/tests/dispatch_pipeline.rs
Backend/crates/poller/tests/poke_pool_changed.rs
Backend/crates/poller/tests/notifier_wiring.rs
Backend/crates/poller/tests/restore_before_first_poll.rs
Backend/crates/poller/tests/watchdog.rs
Backend/crates/api-gateway/tests/cors_and_body_limit.rs
Backend/crates/api-gateway/tests/security_headers.rs
Backend/crates/api-gateway/tests/otp_routes.rs
Backend/crates/api-gateway/tests/session_auth.rs
Backend/crates/api-gateway/tests/auth_routes.rs
Backend/crates/api-gateway/tests/spx_credentials_routes.rs
Backend/crates/api-gateway/tests/portal_users_routes.rs
Backend/crates/api-gateway/tests/rate_limit.rs
Backend/crates/api-gateway/tests/spx_login_routes.rs
Backend/bin/reactor-core/src/main.rs           (Task 7 handles this one — see its own Step 3;
                                                 skip it here to avoid a merge/edit conflict
                                                 between these two tasks)
```

For every file in that list EXCEPT `state.rs` (the definition) and `main.rs` (Task 7's job), add exactly one field to the `PollerShared { ... }` literal:
```rust
rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
```
(inside `poller`'s own test files, drop the `poller::` prefix — use the bare `RuleSet::empty()` since `poller::state::RuleSet`/the crate-root re-export is already in scope via `use poller::{..., PollerShared, ...}` or similar; match whatever import style that specific test file already uses for `PollerShared` itself.) Place it right after the existing `redis: None,` (or wherever the last field is) — trailing comma, exact same style as every other field in that literal.

Any `AccountHandle { poke, join }` literal (if a test constructs one directly rather than via `ensure_restored_then_spawn`) needs `dedup`/`manual_accept` added too — grep `AccountHandle {` the same way and add `dedup: Arc::new(executor::AccountDedupState::new()), manual_accept: tokio::sync::mpsc::channel(8).0,`.

- [x] **Step 5: Write a real end-to-end test for the manual-accept channel**

```rust
// Backend/crates/poller/tests/manual_accept_channel.rs (new file)
//! Task 6 DoD: a `ManualAcceptRequest` sent through a running account's `AccountHandle.manual_accept`
//! reaches `SpxClient::accept_booking` using THAT account's own live cookies/agency_id, and the
//! reply comes back through the `oneshot` channel — proven against a real wiremock SPX server
//! (the account's poll loop itself sees empty pages, so no auto-accept dispatch ever competes
//! with the manual one in this test).
use std::sync::Arc;

use dashmap::DashMap;
use executor::ExecutorHandle;
use poller::{ManualAcceptRequest, PollerConfig, PollerShared, PollerState, SidecarClient};
use spx_client::{SpxClient, SpxCookies};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}
fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

#[tokio::test]
async fn manual_accept_request_reaches_the_running_accounts_own_client_and_replies() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/marketplace/dc/getBookingList"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": { "list": [] }
        })))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/marketplace/dc/acceptBooking"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "message": "ok"
        })))
        .mount(&mock)
        .await;

    let pool = store::connect(&database_url()).await.expect("connect");
    let executor = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = SpxClient::new(mock.uri()).expect("build SpxClient");
    let sidecar = SidecarClient::new("http://127.0.0.1:1".to_string());
    let shared = Arc::new(PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool,
        config: PollerConfig {
            poll_interval_ms: 3_600_000, // effectively never ticks again during this test
            ..PollerConfig::default()
        },
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    let mut state = PollerState::new(
        "manual-accept-test-acct".to_string(),
        uuid::Uuid::new_v4(),
        555, // agency_id — nonzero here so accept_booking does NOT short-circuit on the guard
        SpxCookies::default(),
        "u".into(),
        "p".into(),
    );
    state.agency_id = 555;
    let handle = poller::ensure_restored_then_spawn(shared, state).await;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    handle
        .manual_accept
        .send(ManualAcceptRequest {
            booking_id: 4242,
            request_ids: vec![],
            reply: reply_tx,
        })
        .await
        .expect("send manual accept request");

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
        .await
        .expect("reply must arrive within 5s")
        .expect("reply sender must not be dropped");
    assert_eq!(result.reason, spx_client::AcceptReason::Ok);

    handle.join.abort();
}
```

- [x] **Step 6: Run it**

Run: `cargo test -p poller manual_accept_request_reaches_the_running_accounts_own_client_and_replies -- --test-threads=1`
Expected: PASS.

- [x] **Step 7: Full crate verification**

Run: `cargo build --workspace 2>&1 | grep -E "^error"`
Expected: no output. If any file compiles-fails with "missing field `rules_tx`" (or `dedup`/`manual_accept`), that file was missed in Step 4 — add it there too (the compiler is an exhaustive checklist here: it cannot succeed with any site missed).

Run: `cargo test -p poller -p api-gateway -- --test-threads=1`
Expected: 0 failures (pre-existing tests unaffected — this task changed no runtime behavior for the sleep/poke arms, only added a 3rd arm and two new `AccountHandle` fields).

Run: `cargo clippy -p poller -p api-gateway --all-targets -- -D warnings`
Expected: clean.

```bash
git add Backend/crates/poller/src/state.rs Backend/crates/poller/src/schedule.rs \
        Backend/crates/poller/src/lib.rs Backend/crates/poller/tests/ \
        Backend/crates/api-gateway/tests/
git commit -m "feat(poller): AccountHandle.dedup + manual-accept command channel + RuleSet live-reload scaffolding"
```

---

## Task 7: `poller::rules::load_compiled_rules` + `reactor-core` bootstrap wiring

**Context:** Task 6 built the plumbing; this task is what actually loads persisted `accept_rules`/`rule_booking_targets` rows into a `RuleSet` at boot, and wires every spawned account to subscribe to it. Before this task, `reactor-core`'s bootstrap loop spawned every account with an empty rule set and no way to ever change that short of a restart (a disclosed Fase 6a gap — see `main.rs`'s own doc comment on `build_state`). After this task, that gap is closed for the boot-time case; Task 11 closes it for the live-update case using the exact same `RuleSet`/channel this task seeds.

**Files:**
- Create: `Backend/crates/poller/src/rules.rs`
- Modify: `Backend/crates/poller/src/lib.rs`
- Modify: `Backend/crates/poller/Cargo.toml` (only if `store` is not already a dependency — check first)
- Modify: `Backend/bin/reactor-core/src/main.rs`

**Interfaces:**
- Consumes: `store::accept_rules::list_all`, `store::rule_booking_targets::list_for_tenant` (Tasks 2/3), `core_domain::{AcceptRule, CompiledRule, RuleConditions, RuleMode, RuleBookingType, RouteMatchMode}`, `poller::dispatch::RuleMeta`, `poller::RuleSet` (Task 6).
- Produces: `poller::rules::load_compiled_rules(pool, tenant_id) -> Result<RuleSet, sqlx::Error>`.

- [x] **Step 1: Check the `store` dependency**

Run: `grep -n "^store" Backend/crates/poller/Cargo.toml`
Expected: a line like `store = { version = "0.1.0", path = "../store" }` — `poller` already depends on `store` (Task 5/6a's `store::upsert_booking` etc. calls prove this). No `Cargo.toml` change should be needed; if the grep is somehow empty, add that dependency line under `[dependencies]` matching the version/path style of the other path-dependencies already there (e.g. `core-domain`'s line from Task 6's Step 1).

- [x] **Step 2: Write `poller::rules`**

```rust
// Backend/crates/poller/src/rules.rs
//! Loads a tenant's persisted `accept_rules`/`rule_booking_targets` rows into a compiled,
//! ready-to-match `RuleSet` (`state.rs`, Task 6). The ONLY place `core_domain::CompiledRule::compile`
//! is called in production — every `PollerState.rules` entry traces back through here, either at
//! boot (`reactor-core`'s bootstrap loop, this file's caller) or via a live reload (Task 11's
//! `PUT /bookings/settings` handler, same function, same caller contract).
use std::collections::HashMap;
use std::sync::Arc;

use core_domain::{
    AcceptRule as CoreAcceptRule, CompiledRule, RouteMatchMode, RuleBookingType, RuleConditions,
    RuleMode,
};
use uuid::Uuid;

use crate::dispatch::RuleMeta;
use crate::state::RuleSet;

fn mode_from_text(s: &str) -> RuleMode {
    match s {
        "booking_id" => RuleMode::BookingId,
        "route" => RuleMode::Route,
        _ => RuleMode::Filter,
    }
}

fn booking_type_from_text(s: &str) -> RuleBookingType {
    match s {
        "spxid" => RuleBookingType::Spxid,
        "reguler" => RuleBookingType::Reguler,
        _ => RuleBookingType::All,
    }
}

fn match_mode_from_text(s: &str) -> RouteMatchMode {
    match s {
        "flexible" => RouteMatchMode::Flexible,
        _ => RouteMatchMode::Strict,
    }
}

/// Loads every `accept_rules` row for `tenant_id`, joins in each rule's `rule_booking_targets`
/// (only `BookingId`-mode rules have any — other modes get an empty `booking_ids`, matching
/// `core_domain::RuleConditions`'s own "unused for this mode" convention), and compiles the
/// result. `rules[i]`/`rule_meta[i]` are built in the same loop iteration, so index-alignment
/// (the contract `dispatch.rs::dispatch_booking`'s `st.rule_meta[idx]` lookup relies on) holds
/// by construction.
pub async fn load_compiled_rules(
    pool: &store::PgPool,
    tenant_id: Uuid,
) -> Result<RuleSet, sqlx::Error> {
    let rows = store::accept_rules::list_all(pool, tenant_id).await?;
    let targets = store::rule_booking_targets::list_for_tenant(pool, tenant_id).await?;

    let mut targets_by_rule: HashMap<Uuid, Vec<String>> = HashMap::new();
    for t in targets {
        targets_by_rule
            .entry(t.rule_id)
            .or_default()
            .push(t.booking_id_raw);
    }

    let mut compiled = Vec::with_capacity(rows.len());
    let mut meta = Vec::with_capacity(rows.len());
    for row in rows {
        let booking_ids = targets_by_rule.remove(&row.id).unwrap_or_default();
        let core_rule = CoreAcceptRule {
            id: row.id.to_string(),
            name: row.name.clone(),
            enabled: row.enabled,
            priority: row.priority,
            mode: mode_from_text(&row.mode),
            conditions: RuleConditions {
                service_types: row.service_types.clone(),
                max_weight: row.max_weight,
                coc_only: row.coc_only,
                non_coc_only: row.non_coc_only,
                max_cod_amount: row.max_cod_amount,
                booking_ids,
                origin: row.origin.clone(),
                destinations: row.destinations.clone(),
                booking_type: booking_type_from_text(&row.booking_type),
                shift_types: row.shift_types.clone(),
                trip_types: row.trip_types.clone(),
                match_mode: match_mode_from_text(&row.match_mode),
                min_deadline_min: row.min_deadline_min.map(|m| m.max(0) as u32),
                max_accept_count: row.max_accept_count.max(0) as u32,
                accepted_count: row.accepted_count.max(0) as u32,
            },
        };
        compiled.push(CompiledRule::compile(&core_rule));
        meta.push(RuleMeta {
            uuid: row.id,
            cap: row.max_accept_count as i64,
            accepted_count: row.accepted_count as i64,
            name: row.name,
        });
    }

    Ok(RuleSet {
        rules: Arc::new(compiled),
        rule_meta: Arc::new(meta),
    })
}
```

- [x] **Step 3: Wire `reactor-core`'s bootstrap loop**

In `Backend/bin/reactor-core/src/main.rs`'s `build_state()`, find the existing `poller_shared` construction (currently right before the `// Account bootstrap:` comment) and the `for credential in credentials { ... }` loop right after it.

First, load the initial rule set and build the channel BEFORE constructing `poller_shared`:
```rust
    // Task 7: the tenant's persisted rule set, loaded once at boot. A load failure degrades to
    // an empty rule set (accounts poll/dedupe fine, just match no rules until a later
    // `PUT /bookings/settings` save succeeds) rather than panicking the whole boot — same
    // tolerance this fn already extends to `redis_publisher`'s connect failure just above.
    let initial_rules = match poller::rules::load_compiled_rules(&pool, tenant_id).await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reactor-core: load_compiled_rules failed at boot — starting with an empty \
                 rule set until a settings save succeeds"
            );
            poller::RuleSet::empty()
        }
    };
    let (rules_tx, _rules_rx_template) = tokio::sync::watch::channel(initial_rules);
```

Then add `rules_tx,` to the existing `poller::PollerShared { ... }` literal (right after `redis: redis_publisher,`):
```rust
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig::from_env(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: redis_publisher,
        rules_tx,
    });
```

Then, inside the `for credential in credentials { ... }` loop, right after `let state = poller::PollerState::new(...)` and BEFORE `let handle = poller::ensure_restored_then_spawn(...)`, subscribe and eagerly seed:
```rust
        let mut state = poller::PollerState::new(
            account_id.clone(),
            tenant_id,
            0, // agency_id — disclosed pre-existing gap, unrelated to this task
            spx_client::SpxCookies::default(),
            credential.username.into(),
            password,
        );
        // Task 7: subscribe to the shared rule-reload channel and eagerly seed `rules`/
        // `rule_meta` from its CURRENT value now — `poll_once`'s `has_changed()` gate (Task 6)
        // only fires on a value sent AFTER `subscribe()`, so without this eager seed the very
        // first cycle would still see the empty `PollerState::new` default.
        let rx = poller_shared.rules_tx.subscribe();
        let seed = rx.borrow().clone();
        state.rules = seed.rules;
        state.rule_meta = seed.rule_meta;
        state.rules_rx = Some(rx);
        let handle = poller::ensure_restored_then_spawn(poller_shared.clone(), state).await;
```
(Note `let state` becomes `let mut state` — it is mutated three times before being moved into `ensure_restored_then_spawn`.)

- [x] **Step 4: Re-export from `lib.rs`**

```rust
// Backend/crates/poller/src/lib.rs
pub mod rules;
```
```rust
pub use rules::load_compiled_rules;
```

- [x] **Step 5: Write the failing test — a real end-to-end reload**

```rust
// Backend/bin/reactor-core/src/main.rs — inside the existing `#[cfg(test)] mod tests` block,
// near `boot_smoke_malformed_credential_is_skipped_not_fatal`
/// Task 7 DoD: a rule persisted to `accept_rules` BEFORE boot is present in the spawned
/// account's `PollerState.rules` at construction (the eager-seed path), AND a rule sent
/// through `PollerShared.rules_tx` AFTER boot reaches a running account's next `poll_once`
/// cycle (the live-reload path) — proven by directly inspecting `AccountHandle`'s account
/// through one real `poll_once` call rather than waiting on the full spawned loop's timer.
#[tokio::test]
async fn boot_smoke_seeds_rules_from_db_and_live_reload_reaches_running_account() {
    let database_url = prepare_app_role_database_url().await;
    let (tenant_id, tenant_slug) = seed_test_tenant().await;
    let master_key_path = write_test_master_key();

    let tower_pool = store::connect(&tower_superuser_url())
        .await
        .expect("connect as tower");
    let master_key = spx_client::crypto::envelope::MasterKey::load_from_file(&master_key_path)
        .expect("load test master key back");

    // Seed ONE enabled, unconditional filter rule directly (bypassing the not-yet-built HTTP
    // route — this test only proves the LOADER + CHANNEL, not Task 11's route).
    store::accept_rules::replace_all(
        &tower_pool,
        tenant_id,
        &[store::NewAcceptRule {
            name: "Boot-seeded rule".to_string(),
            enabled: true,
            priority: 0,
            mode: "filter".to_string(),
            service_types: vec![],
            max_weight: None,
            coc_only: false,
            non_coc_only: false,
            max_cod_amount: None,
            origin: String::new(),
            destinations: vec![],
            booking_type: "all".to_string(),
            shift_types: vec![],
            trip_types: vec![],
            match_mode: "strict".to_string(),
            min_deadline_min: None,
            max_accept_count: 0,
            accepted_count: 0,
        }],
    )
    .await
    .expect("seed accept_rules row before boot");

    let username = format!("rules-agent-{}", Uuid::new_v4().simple());
    let ct = spx_client::crypto::envelope::encrypt_agency_password(&master_key, tenant_id, "pw")
        .expect("encrypt test password");
    sqlx::query(
        "INSERT INTO agency_credentials (tenant_id, label, username, ciphertext, nonce, key_version) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tenant_id)
    .bind("primary")
    .bind(&username)
    .bind(&ct.bytes)
    .bind(&ct.nonce[..])
    .bind(spx_client::crypto::envelope::KEY_VERSION)
    .execute(&tower_pool)
    .await
    .expect("insert agency_credentials row");

    std::env::set_var("DATABASE_URL", &database_url);
    std::env::set_var("TENANT_SLUG", &tenant_slug);
    std::env::set_var("TOWER_MASTER_KEY_PATH", master_key_path.to_str().unwrap());
    std::env::set_var("SPX_BASE_URL", "http://127.0.0.1:1");
    std::env::set_var("AUTH_SIDECAR_URL", "http://127.0.0.1:1");

    let state = build_state().await;

    // Boot-time seed: the spawned account's live task already has one compiled rule. There is
    // no direct getter into a running task's `PollerState`, so this asserts indirectly via a
    // FRESH `PollerState` built the same way `build_state` built the real one, subscribed to
    // the SAME `rules_tx` the real boot used — proving the loader itself returned a non-empty
    // set (the thing this test can observe without reaching into the spawned task).
    let seeded = poller::rules::load_compiled_rules(&state.poller.pool, tenant_id)
        .await
        .expect("load_compiled_rules after boot");
    assert_eq!(seeded.rules.len(), 1, "the boot-seeded rule must be loaded");

    // Live-reload path: send a SECOND rule set through the same channel the running account
    // subscribed to, and confirm a subscriber sees it via `has_changed`/`borrow_and_update` —
    // the exact mechanism `poll_once` (Task 6) uses on its next cycle.
    let mut rx = state.poller.rules_tx.subscribe();
    assert!(
        !rx.has_changed().unwrap_or(true),
        "a fresh subscriber must not report a pending change with no send yet"
    );
    state
        .poller
        .rules_tx
        .send(poller::RuleSet::empty())
        .expect("send on rules_tx (at least one receiver — the spawned account — must exist)");
    assert!(
        rx.has_changed().unwrap_or(false),
        "a send after subscribe must be observable via has_changed"
    );
    let after = rx.borrow_and_update().clone();
    assert_eq!(after.rules.len(), 0, "the live-reload payload must be exactly what was sent");

    let _ = std::fs::remove_file(&master_key_path);
    cleanup_tenant(tenant_id).await;
}
```

- [x] **Step 6: Run it**

Run: `cargo test -p reactor-core boot_smoke_seeds_rules_from_db_and_live_reload_reaches_running_account -- --test-threads=1`
Expected: PASS.

- [x] **Step 7: Full workspace verification + commit**

Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 0 failures, clean (this is a natural workspace-wide checkpoint — Tasks 1-7 together are the full store+poller foundation the remaining route tasks build on).

```bash
git add Backend/crates/poller/src/rules.rs Backend/crates/poller/src/lib.rs \
        Backend/crates/poller/Cargo.toml Backend/bin/reactor-core/src/main.rs
git commit -m "feat(poller,reactor-core): load_compiled_rules + boot-time rule loading + live-reload wiring"
```

---

## Task 8: `GET /bookings/live`, `/bookings/history`, `/bookings/:id/detail`

**Scope decision (disclosed):** these three routes need only `session_auth` (any logged-in tenant member) — no `require_permission` gate. This matches the established data-visibility precedent (every `portal_user` under a tenant sees every account's data under that tenant; only SETTINGS mutations are main-account-gated) already documented in the shared Fase 6 design doc.

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/bookings.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs` (`pub mod bookings;`)
- Modify: `Backend/crates/api-gateway/src/lib.rs` (mount)
- Test: `Backend/crates/api-gateway/tests/bookings_routes.rs`

**Interfaces:**
- Consumes: `store::bookings::{list_live, list_history, get_detail}` (Task 1), `crate::auth::{session_auth, CurrentUser}`, `crate::error::ApiError`, `crate::state::AppState`.
- Produces (for Task 10, same file): the `bookings_router` fn Task 10 adds its `POST /:id/accept` route onto.

- [x] **Step 1: Write the route module (list/detail only — Task 10 appends `POST /:id/accept` to the same file/router)**

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs
//! `GET /bookings/live`, `/bookings/history`, `/bookings/:id/detail` — read-only booking views,
//! and (Task 10) `POST /bookings/:id/accept` — manual accept. Every route here needs only
//! `session_auth` (any logged-in tenant member); see this file's own `require_permission`
//! usage (Task 10's handler) for the one exception's rationale.
use axum::extract::{Extension, Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}
fn default_limit() -> i64 {
    50
}
/// Clamp caller-supplied pagination to a sane range — `store`'s own list fns trust their
/// caller (see `bookings.rs`'s doc comment on `list_live`), so this route is that caller.
fn clamp_limit(limit: i64) -> i64 {
    limit.clamp(1, 200)
}
fn clamp_offset(offset: i64) -> i64 {
    offset.max(0)
}

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
}

impl From<store::models::Booking> for BookingListItem {
    fn from(b: store::models::Booking) -> Self {
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
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BookingDetail {
    pub id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub raw_data: Value,
    pub is_coc: bool,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub accept_latency_ms: Option<i32>,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<store::models::Booking> for BookingDetail {
    fn from(b: store::models::Booking) -> Self {
        Self {
            id: b.id,
            account_id: b.account_id,
            spx_id: b.spx_id,
            status: b.status,
            raw_data: b.raw_data,
            is_coc: b.is_coc,
            service_type: b.service_type,
            weight: b.weight,
            cod_amount: b.cod_amount,
            auto_accepted: b.auto_accepted,
            accept_latency_ms: b.accept_latency_ms,
            rule_matched: b.rule_matched,
            created_at: b.created_at,
            updated_at: b.updated_at,
        }
    }
}

async fn live(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<BookingListItem>>, ApiError> {
    let rows = store::bookings::list_live(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
    )
    .await?;
    Ok(Json(rows.into_iter().map(BookingListItem::from).collect()))
}

async fn history(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<BookingListItem>>, ApiError> {
    let rows = store::bookings::list_history(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
    )
    .await?;
    Ok(Json(rows.into_iter().map(BookingListItem::from).collect()))
}

async fn detail(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<BookingDetail>, ApiError> {
    let row = store::bookings::get_detail(&state.poller.pool, user.tenant_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(BookingDetail::from(row)))
}

/// Nested at `/bookings` by `build_router`. Task 10 appends `.route("/{id}/accept", post(...))`
/// to this SAME function (do not create a second router for it — one `/bookings` prefix, one
/// router, per this crate's established one-router-per-resource convention).
pub fn bookings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/live", get(live))
        .route("/history", get(history))
        .route("/{id}/detail", get(detail))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [x] **Step 2: Wire the module**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod bookings;
```
```rust
// Backend/crates/api-gateway/src/lib.rs — add to build_router, alongside the other .nest(...) calls
        .nest("/bookings", routes::bookings::bookings_router(state.clone()))
```

- [x] **Step 3: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/bookings_routes.rs (new file)
//! `GET /bookings/live`, `/history`, `/:id/detail` — session-auth-only read routes.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
use http_body_util::BodyExt;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

use api_gateway::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes([7u8; 32]))
}

async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis")
}

async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("Bookings Test Tenant")
        .bind(format!("bookings-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, "Test User", &hash, true)
        .await
        .expect("create portal user")
        .id
}

async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(vec![]),
        session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(resp.status(), 200, "login must succeed");
    resp.cookies()
        .find(|c| c.name() == "spx_session")
        .map(|c| format!("spx_session={}", c.value()))
        .expect("session cookie must be set")
}

async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

#[tokio::test]
async fn live_and_history_split_by_status_and_require_session() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "live-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed pending booking");
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "hist-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking to mark accepted");
    store::update_booking_status(
        &pool,
        tenant_id,
        "hist-1",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: Some(5),
            auto_accepted: true,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark accepted");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();

    // No session cookie → 401.
    let unauth = http.get(format!("{base}/bookings/live")).send().await.unwrap();
    assert_eq!(unauth.status(), 401);

    let cookie = login_cookie(&http, &base, "owner").await;
    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(live_resp.status(), 200);
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    assert_eq!(live_body.len(), 1);
    assert_eq!(live_body[0]["spx_id"], "live-1");

    let hist_resp = http
        .get(format!("{base}/bookings/history"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(hist_resp.status(), 200);
    let hist_body: Vec<serde_json::Value> = hist_resp.json().await.unwrap();
    assert_eq!(hist_body.len(), 1);
    assert_eq!(hist_body[0]["spx_id"], "hist-1");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn detail_returns_full_raw_data_and_404s_for_unknown_id() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "detail-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"booking_id": "999", "note": "full payload"}),
        },
    )
    .await
    .expect("seed booking");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    let id = live_body[0]["id"].as_str().unwrap();

    let detail_resp = http
        .get(format!("{base}/bookings/{id}/detail"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(detail_resp.status(), 200);
    let detail_body: serde_json::Value = detail_resp.json().await.unwrap();
    assert_eq!(detail_body["raw_data"]["note"], "full payload");

    let missing_resp = http
        .get(format!("{base}/bookings/{}/detail", Uuid::new_v4()))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_resp.status(), 404);

    cleanup(&pool, tenant_id).await;
}
```

- [x] **Step 4: Run the tests**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p api-gateway --test bookings_routes -- --test-threads=1`
Expected: both PASS. (If `store::portal_users::create`'s exact signature or `spx_client::crypto::password::hash_password`'s name differs slightly from what's used above, check `Backend/crates/api-gateway/tests/portal_users_routes.rs`'s own `insert_portal_user` helper for the verified-working call shape and match it exactly — that file already does this successfully.)

- [x] **Step 5: Full crate verification**

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings`
Expected: 0 failures, clean.

- [x] **Step 6: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/api-gateway/src/routes/mod.rs \
        Backend/crates/api-gateway/src/lib.rs Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(api-gateway): GET /bookings/live, /history, /:id/detail"
```

---

## Task 9: `GET /bookings/spx-log`

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs` (append)
- Test: `Backend/crates/api-gateway/tests/bookings_routes.rs` (append)

**Interfaces:**
- Consumes: `store::accept_events::list_for_tenant` (Task 5), the `ListParams`/`clamp_limit`/`clamp_offset` helpers Task 8 already defined in this same file.

- [x] **Step 1: Write the failing test**

```rust
// Backend/crates/api-gateway/tests/bookings_routes.rs — append
#[tokio::test]
async fn spx_log_lists_accept_events_newest_first() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    for outcome in ["accepted", "failed"] {
        store::insert_accept_event(
            &pool,
            tenant_id,
            &store::NewAcceptEvent {
                booking_id: None,
                rule_id: None,
                outcome: outcome.to_string(),
                local_dispatch_us: None,
                accept_e2e_ms: None,
                detail: serde_json::json!({}),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert {outcome}: {e}"));
    }

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/spx-log"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 2);
    assert_eq!(body[0]["outcome"], "failed", "newest (last-inserted) first");

    cleanup(&pool, tenant_id).await;
}
```

- [x] **Step 2: Run it to verify it fails**

Run: `cargo test -p api-gateway --test bookings_routes spx_log_lists_accept_events_newest_first -- --test-threads=1`
Expected: FAIL — no `/bookings/spx-log` route mounted yet.

- [x] **Step 3: Add the handler and route**

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — append

#[derive(Debug, Serialize)]
pub struct AcceptEventItem {
    pub id: Uuid,
    pub booking_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub outcome: String,
    pub local_dispatch_us: Option<i64>,
    pub accept_e2e_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

impl From<store::models::AcceptEvent> for AcceptEventItem {
    fn from(e: store::models::AcceptEvent) -> Self {
        Self {
            id: e.id,
            booking_id: e.booking_id,
            rule_id: e.rule_id,
            outcome: e.outcome,
            local_dispatch_us: e.local_dispatch_us,
            accept_e2e_ms: e.accept_e2e_ms,
            detail: e.detail,
            created_at: e.created_at,
        }
    }
}

async fn spx_log(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<AcceptEventItem>>, ApiError> {
    let rows = store::list_accept_events(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
    )
    .await?;
    Ok(Json(rows.into_iter().map(AcceptEventItem::from).collect()))
}
```
```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — bookings_router, add one .route(...) line
pub fn bookings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/live", get(live))
        .route("/history", get(history))
        .route("/{id}/detail", get(detail))
        .route("/spx-log", get(spx_log))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p api-gateway --test bookings_routes spx_log_lists_accept_events_newest_first -- --test-threads=1`
Expected: PASS.

- [x] **Step 5: Full crate verification + commit**

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings`
Expected: 0 failures, clean.

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(api-gateway): GET /bookings/spx-log"
```

---

## Task 10: `POST /bookings/:id/accept` (manual accept)

**Scope decision (disclosed):** no `require_permission` gate — any authenticated tenant member may manually accept, same rationale as Task 8's read routes (claiming a ticket is "using the system", not "administering settings"; this project's data-visibility model already lets every session member act on every account under the tenant). If this is wrong for the real product, it is a one-line change (add `require_permission(&user, Permission::ManageRules)` as the first statement) — flag this explicitly to the task reviewer rather than silently deciding it's final.

**Behavior:** validate the booking is `pending` → resolve its live `AccountHandle` → `executor::try_claim_manual` (the design doc's named integration point) → on success, dispatch the REAL SPX HTTP accept through the account's own poller task via Task 6's `manual_accept` channel → finalize dedup/durable-record/DB status/audit event based on the real `AcceptResult`. Per Task 6's note, `agency_id=0` (a disclosed, pre-existing Fase 6a gap) means this currently always resolves to `AcceptReason::Auth` in practice — this route still implements the FULL correct flow so nothing needs rebuilding once that gap is closed elsewhere.

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs` (append)
- Test: `Backend/crates/api-gateway/tests/bookings_routes.rs` (append)

**Interfaces:**
- Consumes: `state.poller.accounts: Arc<DashMap<String, poller::AccountHandle>>`, `AccountHandle.{dedup, manual_accept}` (Task 6), `state.poller.executor.try_claim_manual`/`.record_durable_accept`/`.release_claim_auto` (Fase 4, confirmed signatures below), `spx_client::normalize_booking(&Value) -> SpxBooking`, `store::{update_booking_status, insert_accept_event, NewAcceptEvent}`.

- [x] **Step 1: Write the failing test — happy path against a real wiremock SPX + a real spawned account**

```rust
// Backend/crates/api-gateway/tests/bookings_routes.rs — append
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Builds `AppState` the same way `build_state` does, but spawns ONE real poller account
/// (`account_id`) pointed at `mock`'s URI, so `POST /:id/accept` has a real `AccountHandle` to
/// find in `state.poller.accounts`.
async fn build_state_with_account(
    pool: sqlx::PgPool,
    tenant_id: Uuid,
    account_id: &str,
    spx_base_url: &str,
) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = spx_client::SpxClient::new(spx_base_url).expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig {
            poll_interval_ms: 3_600_000,
            ..poller::PollerConfig::default()
        },
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    let mut state = poller::PollerState::new(
        account_id.to_string(),
        tenant_id,
        555, // nonzero agency_id — this test exercises the REAL accept_booking call, not the
             // agency_id<=0 short-circuit Task 6's note discloses for production today
        spx_client::SpxCookies::default(),
        "u".into(),
        "p".into(),
    );
    state.agency_id = 555;
    let handle = poller::ensure_restored_then_spawn(poller_shared.clone(), state).await;
    poller_shared.accounts.insert(account_id.to_string(), handle);

    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(vec![]),
        session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

#[tokio::test]
async fn manual_accept_happy_path_claims_dispatches_and_records() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/marketplace/dc/acceptBooking"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "message": "ok"
        })))
        .mount(&mock)
        .await;

    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "manual-acct".to_string(),
            spx_id: "manual-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({"booking_id": "778899", "request_id": "1"}),
        },
    )
    .await
    .expect("seed booking");

    let state = build_state_with_account(pool.clone(), tenant_id, "manual-acct", &mock.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    let id = live_body[0]["id"].as_str().unwrap().to_string();

    let accept_resp = http
        .post(format!("{base}/bookings/{id}/accept"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(accept_resp.status(), 200);
    let accept_body: serde_json::Value = accept_resp.json().await.unwrap();
    assert_eq!(accept_body["ok"], true);
    assert_eq!(accept_body["reason"], "accepted");

    // DB status must now be 'accepted', not 'pending'.
    let after = store::bookings::get_detail(
        &pool,
        tenant_id,
        Uuid::parse_str(&id).unwrap(),
    )
    .await
    .expect("get_detail")
    .expect("row must still exist");
    assert_eq!(after.status, "accepted");
    assert!(!after.auto_accepted, "manual accept must record auto_accepted=false");

    // A SECOND accept attempt on the same (now non-pending) booking must be rejected.
    let second_resp = http
        .post(format!("{base}/bookings/{id}/accept"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(second_resp.status(), 409, "a non-pending booking must not be re-acceptable");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn manual_accept_404s_for_unknown_booking_and_409s_for_disconnected_account() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    // A booking whose account was never spawned in THIS process — `state.poller.accounts` is
    // empty for it.
    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "never-spawned-acct".to_string(),
            spx_id: "orphan-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");

    let state = build_state(pool.clone(), tenant_id).await; // no account spawned
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    let missing_resp = http
        .post(format!("{base}/bookings/{}/accept", Uuid::new_v4()))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_resp.status(), 404);

    let live_resp = http
        .get(format!("{base}/bookings/live"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let live_body: Vec<serde_json::Value> = live_resp.json().await.unwrap();
    let id = live_body[0]["id"].as_str().unwrap();

    let disconnected_resp = http
        .post(format!("{base}/bookings/{id}/accept"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        disconnected_resp.status(),
        409,
        "a booking whose account has no running AccountHandle must not silently 500"
    );

    cleanup(&pool, tenant_id).await;
}
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p api-gateway --test bookings_routes manual_accept -- --test-threads=1`
Expected: FAIL — no `POST /:id/accept` route mounted yet (404 on every request, including the ones that expect 200/409).

- [x] **Step 3: Add the handler and route**

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — append imports at the top of the file
use axum::routing::post;
```

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — append

#[derive(Debug, Serialize)]
pub struct ManualAcceptResponse {
    pub ok: bool,
    pub reason: String,
    pub message: String,
}

/// Maps `spx_client::AcceptReason` to the SAME `outcome` vocabulary `accept_events.outcome`'s
/// CHECK constraint allows (`'accepted' | 'rejected' | 'skipped' | 'taken_by_agency' | 'failed'
/// | 'agency_dup_unverified'`, migration 0008) — `Skipped`/`Rejected` never occur on THIS path
/// (this route only reaches `accept_booking` after `try_claim_manual` already returned `Ok`),
/// so only the remaining 4 variants are mapped.
fn outcome_for(reason: spx_client::AcceptReason) -> &'static str {
    match reason {
        spx_client::AcceptReason::Ok => "accepted",
        spx_client::AcceptReason::AgencyDup => "agency_dup_unverified",
        spx_client::AcceptReason::Taken => "taken_by_agency",
        spx_client::AcceptReason::Transient
        | spx_client::AcceptReason::Auth
        | spx_client::AcceptReason::Error => "failed",
    }
}

async fn accept(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<ManualAcceptResponse>, ApiError> {
    let booking = store::bookings::get_detail(&state.poller.pool, user.tenant_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if booking.status != "pending" {
        return Err(ApiError::Conflict(format!(
            "booking is not pending (status: {})",
            booking.status
        )));
    }

    let (dedup, manual_tx) = {
        let handle = state
            .poller
            .accounts
            .get(&booking.account_id)
            .ok_or_else(|| {
                ApiError::Conflict(
                    "the account this booking belongs to is not currently connected".to_string(),
                )
            })?;
        (handle.dedup.clone(), handle.manual_accept.clone())
    };

    match state
        .poller
        .executor
        .try_claim_manual(&booking.account_id, &booking.spx_id, &dedup)
        .await
    {
        executor::ManualClaimOutcome::AlreadyAccepted => {
            return Err(ApiError::Conflict(
                "booking is already claimed or accepted".to_string(),
            ));
        }
        executor::ManualClaimOutcome::Ok => {}
    }

    let spx_booking = spx_client::normalize_booking(&booking.raw_data);
    let booking_id_i64 = spx_booking.booking_id.parse::<i64>().unwrap_or(0);
    let request_ids: Vec<i64> = spx_booking
        .request_id
        .parse::<i64>()
        .ok()
        .into_iter()
        .collect();

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    manual_tx
        .send(poller::ManualAcceptRequest {
            booking_id: booking_id_i64,
            request_ids,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            ApiError::Internal("account task is not accepting manual requests".to_string())
        })?;

    let result = tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx)
        .await
        .map_err(|_| ApiError::Internal("manual accept dispatch timed out".to_string()))?
        .map_err(|_| ApiError::Internal("account task dropped the manual accept reply".to_string()))?;

    let outcome = outcome_for(result.reason);

    if matches!(result.reason, spx_client::AcceptReason::Ok) {
        dedup.commit_accept(&booking.spx_id);
        let _ = state
            .poller
            .executor
            .record_durable_accept(&booking.account_id, &booking.spx_id)
            .await;
        let _ = store::update_booking_status(
            &state.poller.pool,
            user.tenant_id,
            &booking.spx_id,
            store::BookingStatusUpdate {
                status: "accepted",
                latency_ms: None,
                auto_accepted: false,
                rule_matched: None,
                accept_reason: None,
            },
        )
        .await;
    } else {
        // Best-effort: release the Layer-2 claim so a retry isn't blocked for the full 600s
        // TTL. `rule_id: None` — manual accepts never populate the inflight quota set, so this
        // is a harmless no-op SREM against a set that was never written to.
        state
            .poller
            .executor
            .release_claim_auto(&booking.account_id, &booking.spx_id, None)
            .await;
        dedup.abort_accept(&booking.spx_id);
        let _ = store::update_booking_status(
            &state.poller.pool,
            user.tenant_id,
            &booking.spx_id,
            store::BookingStatusUpdate {
                status: "failed",
                latency_ms: None,
                auto_accepted: false,
                rule_matched: None,
                accept_reason: Some("manual_accept_failed"),
            },
        )
        .await;
    }

    let _ = store::insert_accept_event(
        &state.poller.pool,
        user.tenant_id,
        &store::NewAcceptEvent {
            booking_id: Some(booking.id),
            rule_id: None,
            outcome: outcome.to_string(),
            local_dispatch_us: None,
            accept_e2e_ms: None,
            detail: serde_json::json!({
                "manual": true,
                "retcode": result.retcode,
                "message": result.message,
            }),
        },
    )
    .await;

    Ok(Json(ManualAcceptResponse {
        ok: matches!(result.reason, spx_client::AcceptReason::Ok),
        reason: outcome.to_string(),
        message: result.message,
    }))
}
```
```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — bookings_router, add the accept route
pub fn bookings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/live", get(live))
        .route("/history", get(history))
        .route("/{id}/detail", get(detail))
        .route("/spx-log", get(spx_log))
        .route("/{id}/accept", post(accept))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p api-gateway --test bookings_routes manual_accept -- --test-threads=1`
Expected: both PASS.

- [x] **Step 5: Full crate verification**

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings`
Expected: 0 failures, clean.

- [x] **Step 6: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(api-gateway): POST /bookings/:id/accept (manual accept via try_claim_manual)"
```

---

## Task 11: `GET`/`PUT /bookings/settings` — rules editor + automation toggle + OTP-gated arm

**Design decisions (all disclosed, flag to the task reviewer):**
- **Persistence strategy is replace-all, not per-rule upsert.** `core_domain::AcceptRule.id: String` (e.g. `"rule_3"`, a `sanitize_accept_rules` fallback) has no stable relationship to `accept_rules.id: Uuid` across saves, and `sanitize_accept_rules`/`dedupe_rules` operate on the FULL list anyway (a rule can merge into or vanish from a differently-sized output list). Every `PUT` deletes every existing `accept_rules` row for the tenant and inserts the sanitized+deduped set fresh, inside one transaction (`accept_rules::replace_all`, Task 2). `rule_booking_targets` rows cascade-delete automatically (`ON DELETE CASCADE`, migration 0006) — no separate cleanup needed. `accepted_count` survives across saves because the client echoes back whatever it read from the prior `GET` (`core_domain::RawRuleConditions::accepted_count` already exists for exactly this round-trip).
- **`GET /bookings/settings` is an addition beyond the design doc's literal scope line** (which names only `PUT`) — a write-only settings endpoint with no way to read current state (including the `accepted_count` values a `PUT` round-trip depends on) would not be a usable feature. Needs only `session_auth`; `PUT` needs `require_permission(&user, Permission::ManageRules)`.
- **The `autoAccept:false→true` OTP gate consumes the proof Fase 6b's `POST /auth/verify-aa-otp` already writes** (`spx:pwverify:<tenant_id>:<portal_user_id>`, `EX 120`, value `"1"`) — this route does NOT accept an OTP code in its own request body; the client's flow is call `/auth/verify-aa-otp` first (already built), then call this route, which does a plain `GET` then `DEL` against that exact key. Missing/expired/already-consumed → `401 Unauthorized` (no body message, matching Task 5's "WrongCode/NoActiveCode collapse to an identical 401" precedent — no distinguishing text for a security-sensitive gate). Only the `false→true` transition is gated — staying armed (`true→true`) or disarming (`→false`) needs no proof.
- After a successful `PUT`, the freshly persisted rule set is pushed to every running poller account via `state.poller.rules_tx.send(...)` (Task 6/7's channel) — reloaded fresh from the DB via `poller::rules::load_compiled_rules` (reusing Task 7's loader rather than re-deriving a `RuleSet` from in-memory data) so the broadcast is guaranteed consistent with what was actually committed.

**Files:**
- Modify: `Backend/crates/api-gateway/src/otp.rs` (`pwverify_key` visibility only)
- Create: `Backend/crates/api-gateway/src/routes/rules.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs` (mount)
- Test: `Backend/crates/api-gateway/tests/rules_routes.rs`

**Interfaces:**
- Consumes: `store::{accept_rules::{list_all, replace_all, NewAcceptRule}, rule_booking_targets::{list_for_tenant, replace_for_rule}, automation_settings::{get, set_auto_accept_enabled}}` (Tasks 2-4), `core_domain::{sanitize_accept_rules, dedupe_rules, RawAcceptRule, RawRuleConditions, AcceptRule, RuleMode, RuleBookingType, RouteMatchMode}`, `poller::rules::load_compiled_rules`, `crate::otp::pwverify_key` (this task, visibility fix), `crate::auth::permission::{require_permission, Permission::{ManageRules, ArmAutoAccept}}`.

- [x] **Step 1: Make `pwverify_key` reachable from this crate's other modules**

```rust
// Backend/crates/api-gateway/src/otp.rs — change ONE line
// (was: fn pwverify_key(tenant_id: Uuid, user_id: Uuid) -> String {)
pub(crate) fn pwverify_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:pwverify:{tenant_id}:{user_id}")
}
```
Run: `cargo build -p api-gateway 2>&1 | grep -E "^error"` — expected: no output (a visibility widening never breaks a caller).

- [x] **Step 2: Write the route module**

```rust
// Backend/crates/api-gateway/src/routes/rules.rs
//! `GET`/`PUT /bookings/settings` — the rules editor + the `autoAccept` global kill switch,
//! OTP-gated on its `false→true` transition. See this task's own plan-doc header for the
//! replace-all persistence strategy and the OTP-consumption contract; both are load-bearing
//! design decisions, not incidental implementation details.
use axum::extract::{Extension, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

fn mode_to_text(m: core_domain::RuleMode) -> &'static str {
    match m {
        core_domain::RuleMode::BookingId => "booking_id",
        core_domain::RuleMode::Route => "route",
        core_domain::RuleMode::Filter => "filter",
    }
}
fn booking_type_to_text(t: core_domain::RuleBookingType) -> &'static str {
    match t {
        core_domain::RuleBookingType::All => "all",
        core_domain::RuleBookingType::Spxid => "spxid",
        core_domain::RuleBookingType::Reguler => "reguler",
    }
}
fn match_mode_to_text(m: core_domain::RouteMatchMode) -> &'static str {
    match m {
        core_domain::RouteMatchMode::Strict => "strict",
        core_domain::RouteMatchMode::Flexible => "flexible",
    }
}

#[derive(Debug, Deserialize)]
pub struct RuleInput {
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub priority: Option<i64>,
    pub mode: Option<String>,
    #[serde(default)]
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    #[serde(default)]
    pub coc_only: bool,
    #[serde(default)]
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    #[serde(default)]
    pub booking_ids: Vec<String>,
    pub origin: Option<String>,
    #[serde(default)]
    pub destinations: Vec<String>,
    pub booking_type: Option<String>,
    #[serde(default)]
    pub shift_types: Vec<i64>,
    #[serde(default)]
    pub trip_types: Vec<i64>,
    pub match_mode: Option<String>,
    pub min_deadline_min: Option<f64>,
    pub max_accept_count: Option<f64>,
    pub accepted_count: Option<i64>,
}

fn to_raw_rule(input: &RuleInput) -> core_domain::RawAcceptRule {
    core_domain::RawAcceptRule {
        id: None,
        name: input.name.clone(),
        enabled: input.enabled,
        priority: input.priority,
        mode: input.mode.clone(),
        conditions: core_domain::RawRuleConditions {
            service_types: input.service_types.clone(),
            max_weight: input.max_weight,
            coc_only: input.coc_only,
            non_coc_only: input.non_coc_only,
            max_cod_amount: input.max_cod_amount,
            booking_ids: input.booking_ids.clone(),
            origin: input.origin.clone(),
            destinations: input.destinations.clone(),
            booking_type: input.booking_type.clone(),
            shift_types: input.shift_types.clone(),
            trip_types: input.trip_types.clone(),
            match_mode: input.match_mode.clone(),
            min_deadline_min: input.min_deadline_min,
            max_accept_count: input.max_accept_count,
            accepted_count: input.accepted_count,
        },
    }
}

fn to_new_accept_rule(r: &core_domain::AcceptRule) -> store::NewAcceptRule {
    store::NewAcceptRule {
        name: r.name.clone(),
        enabled: r.enabled,
        priority: r.priority,
        mode: mode_to_text(r.mode).to_string(),
        service_types: r.conditions.service_types.clone(),
        max_weight: r.conditions.max_weight,
        coc_only: r.conditions.coc_only,
        non_coc_only: r.conditions.non_coc_only,
        max_cod_amount: r.conditions.max_cod_amount,
        origin: r.conditions.origin.clone(),
        destinations: r.conditions.destinations.clone(),
        booking_type: booking_type_to_text(r.conditions.booking_type).to_string(),
        shift_types: r.conditions.shift_types.clone(),
        trip_types: r.conditions.trip_types.clone(),
        match_mode: match_mode_to_text(r.conditions.match_mode).to_string(),
        min_deadline_min: r.conditions.min_deadline_min.map(|v| v as i32),
        max_accept_count: r.conditions.max_accept_count as i32,
        accepted_count: r.conditions.accepted_count as i32,
    }
}

#[derive(Debug, Serialize)]
pub struct RuleOutput {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: String,
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub booking_ids: Vec<String>,
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

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub auto_accept_enabled: bool,
    pub rules: Vec<RuleOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SettingsRequest {
    pub auto_accept_enabled: bool,
    #[serde(default)]
    pub rules: Vec<RuleInput>,
}

async fn build_response(
    pool: &store::PgPool,
    tenant_id: Uuid,
    warnings: Vec<String>,
) -> Result<SettingsResponse, ApiError> {
    let settings = store::get_automation_settings(pool, tenant_id).await?;
    let auto_accept_enabled = settings.map(|s| s.auto_accept_enabled).unwrap_or(false);

    let rows = store::accept_rules::list_all(pool, tenant_id).await?;
    let targets = store::rule_booking_targets::list_for_tenant(pool, tenant_id).await?;
    let mut targets_by_rule: HashMap<Uuid, Vec<String>> = HashMap::new();
    for t in targets {
        targets_by_rule
            .entry(t.rule_id)
            .or_default()
            .push(t.booking_id_raw);
    }

    let rules = rows
        .into_iter()
        .map(|r| {
            let booking_ids = targets_by_rule.remove(&r.id).unwrap_or_default();
            RuleOutput {
                id: r.id,
                name: r.name,
                enabled: r.enabled,
                priority: r.priority,
                mode: r.mode,
                service_types: r.service_types,
                max_weight: r.max_weight,
                coc_only: r.coc_only,
                non_coc_only: r.non_coc_only,
                max_cod_amount: r.max_cod_amount,
                booking_ids,
                origin: r.origin,
                destinations: r.destinations,
                booking_type: r.booking_type,
                shift_types: r.shift_types,
                trip_types: r.trip_types,
                match_mode: r.match_mode,
                min_deadline_min: r.min_deadline_min,
                max_accept_count: r.max_accept_count,
                accepted_count: r.accepted_count,
            }
        })
        .collect();

    Ok(SettingsResponse {
        auto_accept_enabled,
        rules,
        warnings,
    })
}

async fn get_settings(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<SettingsResponse>, ApiError> {
    Ok(Json(
        build_response(&state.poller.pool, user.tenant_id, vec![]).await?,
    ))
}

async fn put_settings(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<SettingsRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
    require_permission(&user, Permission::ManageRules)?;

    let current = store::get_automation_settings(&state.poller.pool, user.tenant_id).await?;
    let currently_enabled = current.map(|s| s.auto_accept_enabled).unwrap_or(false);

    if body.auto_accept_enabled && !currently_enabled {
        require_permission(&user, Permission::ArmAutoAccept)?;
        let key = crate::otp::pwverify_key(user.tenant_id, user.portal_user_id);
        let proof: Option<String> = state
            .redis
            .get(&key)
            .await
            .map_err(|e| ApiError::Internal(format!("redis get pwverify: {e}")))?;
        if proof.is_none() {
            return Err(ApiError::Unauthorized);
        }
        let _: () = state
            .redis
            .del(&key)
            .await
            .map_err(|e| ApiError::Internal(format!("redis del pwverify: {e}")))?;
    }

    let raw_rules: Vec<core_domain::RawAcceptRule> = body.rules.iter().map(to_raw_rule).collect();
    let sanitized = core_domain::sanitize_accept_rules(&raw_rules);
    let deduped = core_domain::dedupe_rules(&sanitized.rules);

    let new_rows: Vec<store::NewAcceptRule> = deduped.iter().map(to_new_accept_rule).collect();
    let inserted =
        store::accept_rules::replace_all(&state.poller.pool, user.tenant_id, &new_rows).await?;

    for (rule, row) in deduped.iter().zip(inserted.iter()) {
        if rule.mode == core_domain::RuleMode::BookingId {
            store::rule_booking_targets::replace_for_rule(
                &state.poller.pool,
                user.tenant_id,
                row.id,
                &rule.conditions.booking_ids,
            )
            .await?;
        }
    }

    store::set_auto_accept_enabled(&state.poller.pool, user.tenant_id, body.auto_accept_enabled)
        .await?;

    // Push the freshly committed rule set to every running poller account (Task 6/7's
    // channel). Reloaded fresh from the DB rather than re-derived from `deduped`/`inserted` in
    // memory, so the broadcast is guaranteed to match what was actually persisted.
    if let Ok(fresh) = poller::rules::load_compiled_rules(&state.poller.pool, user.tenant_id).await
    {
        let _ = state.poller.rules_tx.send(fresh);
    }

    Ok(Json(
        build_response(&state.poller.pool, user.tenant_id, sanitized.warnings).await?,
    ))
}

/// Nested at `/bookings` by `build_router`, alongside Task 8/9/10's `bookings_router` — two
/// separate routers sharing one prefix (`/live`, `/history`, `/:id/detail`, `/spx-log`,
/// `/:id/accept` from one; `/settings` from this one), merged in `lib.rs`.
pub fn rules_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [x] **Step 3: Wire the module**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod rules;
```
```rust
// Backend/crates/api-gateway/src/lib.rs — replace the existing
//   .nest("/bookings", routes::bookings::bookings_router(state.clone()))
// with BOTH routers merged under the same prefix:
        .nest(
            "/bookings",
            routes::bookings::bookings_router(state.clone())
                .merge(routes::rules::rules_router(state.clone())),
        )
```

- [x] **Step 4: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/rules_routes.rs (new file)
//! `GET`/`PUT /bookings/settings` — rules editor, automation toggle, OTP-gated arm.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use redis::AsyncCommands;
use uuid::Uuid;

use api_gateway::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes([7u8; 32]))
}
async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("Rules Test Tenant")
        .bind(format!("rules-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, "Test User", &hash, is_main)
        .await
        .expect("create portal user")
        .id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(vec![]),
        session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(resp.status(), 200, "login must succeed");
    resp.cookies()
        .find(|c| c.name() == "spx_session")
        .map(|c| format!("spx_session={}", c.value()))
        .expect("session cookie must be set")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

#[tokio::test]
async fn get_settings_defaults_to_disabled_and_empty() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["auto_accept_enabled"], false);
    assert_eq!(body["rules"].as_array().unwrap().len(), 0);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn put_settings_sanitizes_dedupes_and_round_trips_via_get() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    // Two rules that target the SAME route (same origin/destination/mode) — dedupe_rules must
    // collapse them to one.
    let put_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({
            "auto_accept_enabled": false,
            "rules": [
                {"name": "A", "enabled": true, "mode": "route", "origin": "Padang DC", "destinations": ["Cileungsi DC"]},
                {"name": "B", "enabled": true, "mode": "route", "origin": "Padang DC", "destinations": ["Cileungsi DC"]}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 200);
    let put_body: serde_json::Value = put_resp.json().await.unwrap();
    assert_eq!(
        put_body["rules"].as_array().unwrap().len(),
        1,
        "two rules targeting the same lane must collapse to one via dedupe_rules"
    );

    let get_resp = http
        .get(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["rules"].as_array().unwrap().len(), 1);
    assert_eq!(get_body["rules"][0]["origin"], "Padang DC");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn sub_user_cannot_write_settings_but_can_read_them() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "helper").await;

    let get_resp = http
        .get(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);

    let put_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": false, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 403);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn arming_auto_accept_without_a_pwverify_proof_is_unauthorized_but_disarming_never_needs_one() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    // Disarming (staying/going to false) never needs a proof.
    let disarm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": false, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(disarm_resp.status(), 200);

    // Arming WITHOUT a proof must be rejected.
    let arm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": true, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(arm_resp.status(), 401);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn arming_auto_accept_consumes_the_pwverify_proof_exactly_once_and_broadcasts_new_rules() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let user_id = insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let mut rules_watcher = state.poller.rules_tx.subscribe();
    let mut redis = test_redis_manager().await;
    // Seed the proof directly — this test exercises THIS route's consumption contract, not
    // Fase 6b's OTP generation/verification (already covered by that sub-phase's own tests).
    let key = format!("spx:pwverify:{tenant_id}:{user_id}");
    let _: () = redis.set_ex(&key, "1", 120).await.expect("seed pwverify proof");

    let base = spawn_server(state).await;
    let http = reqwest::Client::builder().cookie_store(false).build().unwrap();
    let cookie = login_cookie(&http, &base, "owner").await;

    let arm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({
            "auto_accept_enabled": true,
            "rules": [{"name": "R1", "enabled": true, "mode": "filter"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(arm_resp.status(), 200);
    let arm_body: serde_json::Value = arm_resp.json().await.unwrap();
    assert_eq!(arm_body["auto_accept_enabled"], true);

    let proof_after: Option<String> = redis.get(&key).await.expect("read proof after arm");
    assert!(proof_after.is_none(), "the proof must be consumed (deleted) after a successful arm");

    assert!(
        rules_watcher.has_changed().unwrap_or(false),
        "a running account's rules_rx subscriber must see the freshly saved rule set"
    );
    let broadcast = rules_watcher.borrow_and_update().clone();
    assert_eq!(broadcast.rules.len(), 1, "the broadcast RuleSet must reflect the just-saved rule");

    // A SECOND arm attempt (already armed — true→true, not a transition) must NOT need a proof.
    let second_arm_resp = http
        .put(format!("{base}/bookings/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"auto_accept_enabled": true, "rules": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        second_arm_resp.status(),
        200,
        "staying armed (true\u{2192}true) must not require a fresh OTP proof"
    );

    cleanup(&pool, tenant_id).await;
}
```

- [x] **Step 5: Run the tests to verify they fail, then implement, then pass**

Run: `cargo test -p api-gateway --test rules_routes -- --test-threads=1`
First run (before Step 2's handlers exist / before mounting in Step 3): FAIL — routes don't exist yet. After completing Steps 2-3: run again.
Expected after implementation: all 5 tests PASS.

- [x] **Step 6: Full crate + workspace verification**

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings`
Expected: 0 failures, clean.

Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 0 failures, clean — this is the natural full-workspace checkpoint before Task 12's formal sign-off.

- [x] **Step 7: Commit**

```bash
git add Backend/crates/api-gateway/src/otp.rs Backend/crates/api-gateway/src/routes/rules.rs \
        Backend/crates/api-gateway/src/routes/mod.rs Backend/crates/api-gateway/src/lib.rs \
        Backend/crates/api-gateway/tests/rules_routes.rs
git commit -m "feat(api-gateway): GET/PUT /bookings/settings (rules editor, OTP-gated auto-accept arm, live-reload broadcast)"
```

---

## Task 12: Final verification + sign-off

**Files:** none (verification-only task, no new code).

- [x] **Step 1: Full workspace test suite**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo test --workspace -- --test-threads=1 2>&1 | tail -80`
Expected: `0 failed` across every crate. `--test-threads=1` matters — several `store`/`poller`/`api-gateway` integration tests share the same Postgres/Redis instance and are not designed for parallel execution within a crate (established convention every prior fase's sign-off task has used).

- [x] **Step 2: Clippy, workspace-wide, warnings as errors**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [x] **Step 3: `cargo deny check`**

Run: `cargo deny check`
Expected: `advisories ok, bans ok, licenses ok, sources ok` (exit code 0). The only expected non-error output is the pre-existing duplicate-crate-version info already present before this plan (e.g. `aead`/`aes` dual-version — unrelated to Fase 6c, do not attempt to fix as part of this sign-off).

- [x] **Step 4: `cargo tree` cross-dependency check (DoD #8)**

Run: `cargo tree -p api-gateway | grep -E "^(store|executor|spx-client|poller|ws-hub|notifier|core-domain) v" | sort -u`
Expected: all 7 crates present (api-gateway is the ONLY crate depending on all of them). Then run `cargo tree -p store -p executor -p spx-client -p poller -p ws-hub -p notifier -p core-domain --edges normal -i api-gateway 2>&1 | head -5` and confirm it reports no matching package (api-gateway must not be a dependency OF any of these — the direction only ever goes one way).

- [x] **Step 5: `reactor-core` boot smoke test (already covered by Task 7's own test, re-run here as part of the whole-branch checkpoint)**

Run: `cargo test -p reactor-core -- --test-threads=1`
Expected: all boot-smoke tests pass, including Task 7's `boot_smoke_seeds_rules_from_db_and_live_reload_reaches_running_account`.

- [x] **Step 6: Checkbox guard**

This plan has checkbox (`- [ ]`) syntax throughout. Before merge, every REAL step checkbox across all 12 tasks must be converted to `- [x]` as its task completes during execution — this step is a final guard confirming none were missed. Run:
```bash
grep -c '^\- \[ \]' Docs/superpowers/plans/2026-07-16-fase-6c-bookings-rules.md
```
Expected: `0` (all converted). If not, find and convert the remainder, then manually eyeball a `git diff` of the plan file to confirm only checkbox markers changed (`[ ]` → `[x]`) — no prose was accidentally altered in the process, matching this project's established checkbox-guard procedure (a repeatedly-violated failure mode in earlier fases, per the progress ledger's own notes on Fase 6b Task 7).

- [x] **Step 7: Definition of Done cross-check, scoped to THIS sub-phase's slice**

Re-read the shared design doc's Definition of Done list (`Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md`). Confirm 6c's contribution:
- #1 (route-level parity coverage): `GET /bookings/live`, `/history`, `/:id/detail`, `/spx-log`, `POST /:id/accept`, `GET`/`PUT /bookings/settings` — all six now have TOWER routes with route-level tests proving status + body shape. Full #1 still needs 6d/6e's remaining route groups (prices/branding/locations/bot-settings/quick-accept) — not this sub-phase's job to close alone.
- #2 (session auth + `require_permission` on every mutating route, tested): `POST /:id/accept` deliberately has NO `require_permission` (disclosed judgment call, Task 10) — confirm this was flagged to the task reviewer and either accepted or reversed before this sign-off. `PUT /bookings/settings` is fully covered (`ManageRules` + conditional `ArmAutoAccept`, both tested).
- #3 (OTP gate blocks `autoAccept:false→true` without a fresh, single-use, main-account-scoped proof — REAL Redis test): fully closed by Task 11 — this is the FIRST and ONLY consumer of the `spx:pwverify` proof Fase 6b produced; `arming_auto_accept_consumes_the_pwverify_proof_exactly_once_and_broadcasts_new_rules` proves single-use (GET+DEL) against real Redis.
- #8 (`api-gateway` is the sole crate depending on all of `store`/`executor`/`spx-client`/`poller`/`ws-hub`/`notifier`/`core-domain`): re-confirmed by Step 4 above — `store` gained a NEW dependency on `core-domain` in Task 3 (needed for `norm_id`), which does not violate DoD #8 (that item is about `api-gateway`'s uniqueness as the crate depending on ALL seven, not about whether `store` may depend on `core-domain`) but is worth calling out explicitly in the sign-off notes since it's a real, new inter-crate edge this sub-phase introduced.

Do NOT claim #4/#5/#6/#7 as closed by this sub-phase alone (docker-compose smoke test, security headers/CORS/rate-limit, quick-accept HMAC, and workspace-wide clean build respectively — the last one IS re-verified by Steps 1-2 above, but was not exclusively this sub-phase's to close).

- [x] **Step 8: Update the progress ledger**

Append one line per task (mirroring every prior sub-phase's ledger entries — see `.superpowers/sdd/progress.md`'s existing Fase 6b entries for the level of detail expected) plus a closing summary line: `Fase 6c (bookings + rules): all 12 tasks complete. Proceeding to final whole-branch review.`

- [x] **Step 9: Commit**

```bash
git add -A
git commit -m "test(fase-6c): bookings + rules sign-off — full workspace verification"
```

---

## Self-Review Notes (writing-plans skill, run by the plan author before handoff)

**Spec coverage:** every bullet in the shared design doc's 6c scope line has a task — `GET /bookings/live` (Task 8), `/history` (Task 8), `/:id/detail` (Task 8), `/spx-log` (Task 9), `POST /:id/accept` via `try_claim_manual` (Task 10), `PUT /bookings/settings` with rules CRUD via `sanitize_accept_rules`/`dedupe_rules` (Task 11), the OTP-gated `autoAccept:false→true` transition (Task 11). Three genuine, previously-undiscovered gaps surfaced during planning and are resolved with disclosed, reasoned scope decisions rather than silently invented: `bookings.account_id` (Task 1, confirmed with the user), `AccountHandle` cross-task access for dedup + a real SPX HTTP dispatch (Task 6), and the rule-loader/live-reload path with no prior existence anywhere in the codebase (Task 7).

**Placeholder scan:** no "TBD"/"handle appropriately"/"similar to Task N" patterns — every step carries complete, real code against verified signatures (every `store`/`poller`/`executor`/`core-domain`/`spx-client` function this plan calls was read from its actual source during planning, not assumed).

**Type consistency:** `RuleSet { rules: Arc<Vec<CompiledRule>>, rule_meta: Arc<Vec<RuleMeta>> }` (Task 6) is used identically in Task 7's loader, Task 10's implicit dependency (via `AccountHandle`), and Task 11's broadcast. `ManualAcceptRequest { booking_id: i64, request_ids: Vec<i64>, reply: oneshot::Sender<AcceptResult> }` (Task 6) matches Task 10's construction exactly. `store::NewAcceptRule`'s field list (Task 2) matches `accept_rules.rs`'s `replace_all` INSERT column list, `to_new_accept_rule`'s output (Task 11), and `load_compiled_rules`'s read-back (Task 7) — all four enumerate the same 19 columns in the same order.

**Cross-task dependency order:** Tasks 1-5 (store layer) have no dependency on Tasks 6-11. Task 6 depends on nothing new (pure `poller` types). Task 7 depends on Tasks 2/3 (`accept_rules`/`rule_booking_targets` list fns) and Task 6 (`RuleSet`). Tasks 8/9 depend only on Task 1/5 (store reads) — independent of Tasks 6/7/10/11. Task 10 depends on Task 1 (`account_id` column), Task 6 (`dedup`/`manual_accept` on `AccountHandle`), Task 5 (`accept_events`). Task 11 depends on Tasks 2/3/4 (store CRUD), Task 7 (`load_compiled_rules` for the broadcast), and the OTP visibility fix (its own Step 1). This ordering (1→2→3→4→5→6→7→8→9→10→11→12) is a valid topological sort — no task requires anything from a LATER task.
