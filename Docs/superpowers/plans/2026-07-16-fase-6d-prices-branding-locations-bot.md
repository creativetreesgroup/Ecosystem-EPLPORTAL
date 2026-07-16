# Fase 6d (prices, branding, locations, bot settings) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the fourth api-gateway sub-phase: public price-list read + CRUD, a branding editor with a 15MB body-limit carve-out, location CRUD, and WAHA/n8n bot-settings + a Redis-backed bot-activity log.

**Architecture:** Three new `store` CRUD modules (`route_prices`, `route_locations`, plus completing `site_settings`'s CRUD verbs). `spx_client::waha_settings::WahaSettings` gains the 4 fields it's been missing since 6b so it can serve as the FULL `GET/PUT /bot/settings` shape. A new `notifier::bot_log` module gives this workspace its first Redis-backed audit-log ring buffer, called explicitly from the two existing WAHA-touching call sites (no changes to `notify_accepted`/`notify_agency_loss`'s own signatures). `build_router` gets restructured so branding's 15MB body-limit doesn't leak onto every other route — the single highest-risk piece of this sub-phase, verified against `tower-http`'s actual source before this plan was written (see Task 8's own risk note).

**Tech Stack:** Same as every prior sub-phase — `axum` 0.8, `sqlx` 0.9/Postgres, `redis` 1.3 (`aio::ConnectionManager`), `tower_governor` 0.8 (already a direct dependency, no new Cargo.toml entries needed anywhere in this plan).

## Global Constraints

- Every tenant-scoped query MUST run inside `store::begin_tenant_tx(pool, tenant_id)`.
- `Permission::{ManagePrices, ManageBranding, ManageLocations, ManageBotSettings}` all already exist (`Backend/crates/api-gateway/src/auth/permission.rs`) — no enum change needed anywhere in this plan.
- `GET /prices` is genuinely public — no `session_auth` at all, the first route in this crate with that property. It gets a NEW 120/min/IP `tower_governor` limiter instead (Task 4). `GET/POST/DELETE /locations` stay `session_auth`-gated (confirmed against the reference: locations are NOT in the public exemption list).
- `GET/PUT /bot/settings` is gated on `Permission::ManageBotSettings` on **BOTH** verbs (unlike every prior sub-phase's "GET = any session, mutation = gated" convention) — the reference itself main-account-gates reading WAHA config, since it includes sensitive connection info even with the API key masked.
- `PUT /branding` is gated on `Permission::ManageBranding` — **stricter than the reference** (which allows any session), a deliberate, disclosed tightening consistent with every other settings-mutation route in this crate.
- `ApiError` variants unchanged: `Unauthorized | Forbidden | NotFound | Conflict(String) | BadRequest(String) | Internal(String) | TooManyRequests(String)`.
- No new Cargo.toml dependencies anywhere in this plan — `tower_governor`, `governor`, `redis`, `serde`/`serde_json` are all already direct dependencies of the crates that need them.
- Every new file needs a top doc comment; `cargo fmt`/`cargo clippy --workspace --all-targets -- -D warnings`/`cargo test --workspace -- --test-threads=1` must stay clean after every task.

---

## Task 1: `store::route_prices` CRUD

**Files:**
- Create: `Backend/crates/store/src/route_prices.rs`
- Modify: `Backend/crates/store/src/lib.rs`

**Interfaces:**
- Consumes: `crate::begin_tenant_tx`, `crate::models::RoutePrice` (`id, tenant_id, route_code, region, origin, destinations: Value, price: i64, vehicle_type, created_at, updated_at`).
- Produces (for Task 4): `route_prices::{list_all, create, update, delete}`.

- [ ] **Step 1: Write the module**

```rust
// Backend/crates/store/src/route_prices.rs
//! `route_prices` CRUD. `destinations` is a JSONB array of 1-5 strings (schema
//! CHECK constraint `route_prices_destinations_1to5`, migration 0013) — this
//! module passes it through as `serde_json::Value` untouched; validating the
//! 1-5 count and shape is the ROUTE layer's job (Task 4), same "store trusts
//! its caller, the DB is the final backstop" convention every other
//! CHECK-constrained table in this crate already follows.
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::RoutePrice;

#[derive(Debug, Clone)]
pub struct NewRoutePrice {
    pub route_code: String,
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
}

pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<RoutePrice>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, RoutePrice>(
        "SELECT id, tenant_id, route_code, region, origin, destinations, price, vehicle_type, \
         created_at, updated_at FROM route_prices WHERE tenant_id = $1 ORDER BY route_code ASC",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// A duplicate `(tenant_id, route_code)` surfaces as a real `23505`, propagated via `?` for
/// `ApiError::From<sqlx::Error>` to map to `409` — same non-special-casing convention as
/// `agency_credentials::create`.
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    new: &NewRoutePrice,
) -> Result<RoutePrice, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, RoutePrice>(
        "INSERT INTO route_prices (tenant_id, route_code, region, origin, destinations, price, vehicle_type) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, tenant_id, route_code, region, origin, destinations, price, vehicle_type, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(&new.route_code)
    .bind(&new.region)
    .bind(&new.origin)
    .bind(&new.destinations)
    .bind(new.price)
    .bind(&new.vehicle_type)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// `None` when no row matches `(tenant_id, id)` — caller maps that to `404`.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    pool: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
    new: &NewRoutePrice,
) -> Result<Option<RoutePrice>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, RoutePrice>(
        "UPDATE route_prices SET route_code = $3, region = $4, origin = $5, destinations = $6, \
         price = $7, vehicle_type = $8, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 \
         RETURNING id, tenant_id, route_code, region, origin, destinations, price, vehicle_type, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(id)
    .bind(&new.route_code)
    .bind(&new.region)
    .bind(&new.origin)
    .bind(&new.destinations)
    .bind(new.price)
    .bind(&new.vehicle_type)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM route_prices WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
```

- [ ] **Step 2: Wire the module**

```rust
// Backend/crates/store/src/lib.rs
pub mod route_prices;
```
```rust
pub use route_prices::{
    create as create_route_price, delete as delete_route_price, list_all as list_route_prices,
    update as update_route_price, NewRoutePrice,
};
```

- [ ] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn route_prices_create_update_delete_round_trip() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let created = route_prices::create(
        &pool,
        tenant_id,
        &route_prices::NewRoutePrice {
            route_code: "PDG-CGS".to_string(),
            region: "Sumatra".to_string(),
            origin: "Padang DC".to_string(),
            destinations: serde_json::json!(["Cileungsi DC"]),
            price: 1_500_000,
            vehicle_type: "TRONTON".to_string(),
        },
    )
    .await
    .expect("create");
    assert_eq!(created.price, 1_500_000);

    let updated = route_prices::update(
        &pool,
        tenant_id,
        created.id,
        &route_prices::NewRoutePrice {
            route_code: "PDG-CGS".to_string(),
            region: "Sumatra".to_string(),
            origin: "Padang DC".to_string(),
            destinations: serde_json::json!(["Cileungsi DC", "Jakarta DC"]),
            price: 1_750_000,
            vehicle_type: "TRONTON".to_string(),
        },
    )
    .await
    .expect("update query")
    .expect("row must exist");
    assert_eq!(updated.price, 1_750_000);
    assert_eq!(updated.id, created.id);

    let listed = route_prices::list_all(&pool, tenant_id).await.expect("list_all");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].price, 1_750_000);

    let deleted = route_prices::delete(&pool, tenant_id, created.id).await.expect("delete");
    assert!(deleted);
    let after = route_prices::list_all(&pool, tenant_id).await.expect("list_all after delete");
    assert_eq!(after.len(), 0);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [ ] **Step 4: Run it, then full verification + commit**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p store route_prices_create_update_delete_round_trip -- --test-threads=1`
Expected: PASS. Then `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings` — 0 failures, clean.

```bash
git add Backend/crates/store/src/route_prices.rs Backend/crates/store/src/lib.rs
git commit -m "feat(store): route_prices CRUD"
```

---

## Task 2: `store::route_locations` CRUD

**Files:**
- Create: `Backend/crates/store/src/route_locations.rs`
- Modify: `Backend/crates/store/src/lib.rs`

**Interfaces:**
- Consumes: `crate::models::RouteLocation` (`id, tenant_id, name, created_at` — no `updated_at`, locations are add/delete-only per the schema).
- Produces (for Task 5): `route_locations::{list_all, create, delete}`.

- [ ] **Step 1: Write the module**

```rust
// Backend/crates/store/src/route_locations.rs
//! `route_locations` CRUD. Add/delete only — the table has no `updated_at`
//! column (migration 0014), matching the reference's own behavior: a
//! location's `name` is either right or gets deleted and re-added, never
//! edited in place.
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::RouteLocation;

pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<RouteLocation>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, RouteLocation>(
        "SELECT id, tenant_id, name, created_at FROM route_locations \
         WHERE tenant_id = $1 ORDER BY name ASC",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// A duplicate `(tenant_id, name)` surfaces as `23505` via `?`, mapped to `409`.
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    name: &str,
) -> Result<RouteLocation, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, RouteLocation>(
        "INSERT INTO route_locations (tenant_id, name) VALUES ($1, $2) \
         RETURNING id, tenant_id, name, created_at",
    )
    .bind(tenant_id)
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM route_locations WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
```

- [ ] **Step 2: Wire the module**

```rust
// Backend/crates/store/src/lib.rs
pub mod route_locations;
```
```rust
pub use route_locations::{
    create as create_route_location, delete as delete_route_location,
    list_all as list_route_locations,
};
```

- [ ] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn route_locations_create_list_delete_round_trip() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let created = route_locations::create(&pool, tenant_id, "Padang DC")
        .await
        .expect("create");
    assert_eq!(created.name, "Padang DC");

    let dup = route_locations::create(&pool, tenant_id, "Padang DC").await;
    assert!(dup.is_err(), "duplicate (tenant_id, name) must fail");
    let db_err = dup.unwrap_err();
    assert_eq!(
        db_err.as_database_error().and_then(|e| e.code().map(|c| c.to_string())),
        Some("23505".to_string())
    );

    let listed = route_locations::list_all(&pool, tenant_id).await.expect("list_all");
    assert_eq!(listed.len(), 1);

    let deleted = route_locations::delete(&pool, tenant_id, created.id).await.expect("delete");
    assert!(deleted);
    let after = route_locations::list_all(&pool, tenant_id).await.expect("list_all after delete");
    assert_eq!(after.len(), 0);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [ ] **Step 4: Run it, then full verification + commit**

Run: `cargo test -p store route_locations_create_list_delete_round_trip -- --test-threads=1`, then `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings`.

```bash
git add Backend/crates/store/src/route_locations.rs Backend/crates/store/src/lib.rs
git commit -m "feat(store): route_locations CRUD"
```

---

## Task 3: `store::site_settings` — complete the CRUD verbs

**Context:** Only `get` exists today (Fase 6b), with its own doc comment explicitly deferring `put`/`delete`/`list` to 6d. This task adds them — needed by Task 6 (bot settings) and Task 8 (branding), both of which persist a JSONB blob under a fixed `key`.

**Files:**
- Modify: `Backend/crates/store/src/site_settings.rs`
- Modify: `Backend/crates/store/src/lib.rs`

**Interfaces:**
- Produces (for Tasks 6 and 8): `site_settings::{put, delete, list}` alongside the existing `get`.

- [ ] **Step 1: Add the missing verbs**

```rust
// Backend/crates/store/src/site_settings.rs — append (keep the existing `get` fn unchanged)

/// Upserts a `site_settings` row — `INSERT ... ON CONFLICT (tenant_id, key) DO UPDATE`, since the
/// table's PK IS `(tenant_id, key)` (migration 0012). Every writer of this table (WAHA settings,
/// bot settings, branding) wants "set this key to this value, whether or not it existed before" —
/// no caller needs to distinguish create-vs-update, matching this crate's established
/// `agency_credentials`-adjacent "PUT is idempotent, always 200" convention.
pub async fn put(
    pool: &PgPool,
    tenant_id: Uuid,
    key: &str,
    value: &Value,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query(
        "INSERT INTO site_settings (tenant_id, key, value, updated_at) VALUES ($1, $2, $3, now()) \
         ON CONFLICT (tenant_id, key) DO UPDATE SET value = $3, updated_at = now()",
    )
    .bind(tenant_id)
    .bind(key)
    .bind(value)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `true` if a row existed and was deleted, `false` if no such `(tenant_id, key)` row existed.
pub async fn delete(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM site_settings WHERE tenant_id = $1 AND key = $2")
        .bind(tenant_id)
        .bind(key)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// Every `(key, value)` pair for `tenant_id` — no consumer needs this yet in THIS plan, but
/// `GET /bot/settings`-adjacent admin tooling (or a future settings-export feature) is the
/// obvious future caller; added now since it's a two-line query and completes the CRUD verb set
/// this module's own doc comment already promised.
pub async fn list(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<(String, Value)>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows: Vec<(String, Value)> =
        sqlx::query_as("SELECT key, value FROM site_settings WHERE tenant_id = $1 ORDER BY key ASC")
            .bind(tenant_id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(rows)
}
```

- [ ] **Step 2: Update the module's own top doc comment**

```rust
// Backend/crates/store/src/site_settings.rs — replace the doc comment's "Deliberately just this
// one `get` fn..." paragraph with:
//! Full CRUD (`get`/`put`/`delete`/`list`) for the generic tenant-scoped `site_settings`
//! key/value store (migration 0012, PK `(tenant_id, key)`). `get` shipped in Fase 6b (needed
//! before any writer existed); `put`/`delete`/`list` complete the set in Fase 6d, this table's
//! first two real writers (`waha_settings` extended by Task 6, `price_page`/branding by Task 8).
```

- [ ] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside `#[cfg(test)] mod tests`
#[tokio::test]
async fn site_settings_put_get_delete_round_trip() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let before = site_settings::get(&pool, tenant_id, "test_key").await.expect("get before put");
    assert!(before.is_none());

    site_settings::put(&pool, tenant_id, "test_key", &serde_json::json!({"a": 1}))
        .await
        .expect("first put (creates)");
    let after_create = site_settings::get(&pool, tenant_id, "test_key")
        .await
        .expect("get after create")
        .expect("row must exist");
    assert_eq!(after_create, serde_json::json!({"a": 1}));

    site_settings::put(&pool, tenant_id, "test_key", &serde_json::json!({"a": 2}))
        .await
        .expect("second put (updates)");
    let after_update = site_settings::get(&pool, tenant_id, "test_key")
        .await
        .expect("get after update")
        .expect("row must still exist");
    assert_eq!(after_update, serde_json::json!({"a": 2}), "put must overwrite, not merge");

    let listed = site_settings::list(&pool, tenant_id).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0, "test_key");

    let deleted = site_settings::delete(&pool, tenant_id, "test_key").await.expect("delete");
    assert!(deleted);
    let after_delete = site_settings::get(&pool, tenant_id, "test_key").await.expect("get after delete");
    assert!(after_delete.is_none());

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

- [ ] **Step 4: Run it, then full verification + commit**

Run: `cargo test -p store site_settings_put_get_delete_round_trip -- --test-threads=1`, then `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings`.

```bash
git add Backend/crates/store/src/site_settings.rs
git commit -m "feat(store): complete site_settings CRUD (put, delete, list)"
```

---

## Task 4: `public_rate_limit_layer` + `GET /prices` (public) + `POST/PUT/DELETE /prices` (`ManagePrices`)

**Context:** `GET /prices` is the first route in this crate with NO `session_auth` at all — the reference exposes it unauthenticated (a public rate-card page). Mirrors `routes/auth.rs::auth_router`'s existing `login.merge(protected)` structure: a public sub-router with its own rate-limit `route_layer`, merged alongside a `session_auth`-gated sub-router for the mutating verbs.

**Files:**
- Modify: `Backend/crates/api-gateway/src/middleware/rate_limit.rs` (new `public_rate_limit_layer` fn)
- Modify: `Backend/crates/api-gateway/src/middleware/mod.rs` (re-export)
- Create: `Backend/crates/api-gateway/src/routes/prices.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs` (mount)
- Test: `Backend/crates/api-gateway/tests/prices_routes.rs`

**Interfaces:**
- Consumes: `store::{list_route_prices, create_route_price, update_route_price, delete_route_price, NewRoutePrice}` (Task 1).
- Produces: `middleware::public_rate_limit_layer() -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body>`.

- [ ] **Step 1: Add the public rate limiter**

```rust
// Backend/crates/api-gateway/src/middleware/rate_limit.rs — append, after the existing
// login_rate_limit_layer fn and its constants

/// ~120 requests/minute/IP for public reads (`GET /prices`, and Task 8's `GET /branding` +
/// friends) — the reference's own "undifferentiated public-GET limiter" figure, matching the
/// design doc's binding constant. A burst of 120 immediately, replenishing one element every
/// 500ms thereafter (120 elements/60s steady-state) — the SAME `SmartIpKeyExtractor` as
/// `login_rate_limit_layer` (see that fn's own doc comment for the X-Forwarded-For trust
/// invariant this depends on; identical here, not re-derived).
const PUBLIC_BURST_SIZE: u32 = 120;
const PUBLIC_REPLENISH_PERIOD_MS: u64 = 500;

/// Builds the route-scoped rate-limit layer for public GET routes. Applied via `.route_layer(...)`
/// on the specific public sub-router (e.g. `routes/prices.rs::prices_router`'s public half) —
/// never mounted globally, never applied to session-authenticated traffic.
pub fn public_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(PUBLIC_REPLENISH_PERIOD_MS)
        .burst_size(PUBLIC_BURST_SIZE)
        .finish()
        .expect("PUBLIC_BURST_SIZE and PUBLIC_REPLENISH_PERIOD_MS are both non-zero");
    GovernorLayer::new(config)
}
```
Check `GovernorConfigBuilder`'s actual method name for a millisecond-granularity replenish period (`per_millisecond` vs. only `per_second`/`per_nanosecond` existing in the resolved `governor`/`tower_governor` version) by reading the vendored source (`~/.cargo/registry/src/.../tower_governor-0.8.0/` and its `governor` dependency) BEFORE writing this — `login_rate_limit_layer` only ever used `per_second`, so this is the first call site in this crate needing sub-second granularity; if `per_millisecond` doesn't exist, use `per_second(1)` with `burst_size(120)` instead (a coarser but still-compliant 120/min figure: 1 token/sec replenish, burst up to 120) and note the substitution in your report.

- [ ] **Step 2: Re-export**

```rust
// Backend/crates/api-gateway/src/middleware/mod.rs
pub use rate_limit::{login_rate_limit_layer, public_rate_limit_layer};
```

- [ ] **Step 3: Write the route module**

```rust
// Backend/crates/api-gateway/src/routes/prices.rs
//! `GET /prices` — public, no `session_auth`, rate-limited 120/min/IP instead. The first route in
//! this crate with no session concept at all (unlike `POST /auth/portal-login`, which merely
//! doesn't yet HAVE a session — this route never authenticates anyone, for anyone, ever).
//! `POST/PUT/DELETE /prices` are `session_auth` + `Permission::ManagePrices`-gated, following
//! this crate's established mutation-gating convention.
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct RoutePriceItem {
    pub id: Uuid,
    pub route_code: String,
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
}

impl From<store::models::RoutePrice> for RoutePriceItem {
    fn from(r: store::models::RoutePrice) -> Self {
        Self {
            id: r.id,
            route_code: r.route_code,
            region: r.region,
            origin: r.origin,
            destinations: r.destinations,
            price: r.price,
            vehicle_type: r.vehicle_type,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PriceInput {
    pub route_code: String,
    #[serde(default)]
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
}

/// Validates `destinations` is a JSON array of 1-5 non-empty strings — mirrors the DB's OWN
/// `route_prices_destinations_1to5` CHECK constraint (migration 0013) at the HTTP layer, so a
/// malformed request gets a clear `400` instead of an opaque `500` from a raw constraint
/// violation (`ApiError::From<sqlx::Error>` maps any non-`23505` DB error, including a CHECK
/// violation, to `Internal`/`500` — this validation exists specifically to avoid that for the
/// common, easily-anticipated case).
fn validate_destinations(v: &Value) -> Result<(), ApiError> {
    let arr = v
        .as_array()
        .ok_or_else(|| ApiError::BadRequest("destinations must be a JSON array".to_string()))?;
    if arr.is_empty() || arr.len() > 5 {
        return Err(ApiError::BadRequest(
            "destinations must have between 1 and 5 entries".to_string(),
        ));
    }
    if !arr.iter().all(|d| d.as_str().is_some_and(|s| !s.trim().is_empty())) {
        return Err(ApiError::BadRequest(
            "every destination must be a non-empty string".to_string(),
        ));
    }
    Ok(())
}

fn to_new_route_price(input: &PriceInput) -> store::NewRoutePrice {
    store::NewRoutePrice {
        route_code: input.route_code.trim().to_string(),
        region: input.region.trim().to_string(),
        origin: input.origin.trim().to_string(),
        destinations: input.destinations.clone(),
        price: input.price,
        vehicle_type: input.vehicle_type.trim().to_string(),
    }
}

async fn list_prices(State(state): State<AppState>) -> Result<Json<Vec<RoutePriceItem>>, ApiError> {
    let rows = store::list_route_prices(&state.poller.pool, state.tenant_id).await?;
    Ok(Json(rows.into_iter().map(RoutePriceItem::from).collect()))
}

async fn create_price(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<PriceInput>,
) -> Result<Json<RoutePriceItem>, ApiError> {
    require_permission(&user, Permission::ManagePrices)?;
    validate_destinations(&body.destinations)?;
    let row = store::create_route_price(&state.poller.pool, user.tenant_id, &to_new_route_price(&body))
        .await?;
    Ok(Json(RoutePriceItem::from(row)))
}

async fn update_price(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    Json(body): Json<PriceInput>,
) -> Result<Json<RoutePriceItem>, ApiError> {
    require_permission(&user, Permission::ManagePrices)?;
    validate_destinations(&body.destinations)?;
    let row = store::update_route_price(&state.poller.pool, user.tenant_id, id, &to_new_route_price(&body))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(RoutePriceItem::from(row)))
}

async fn delete_price(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManagePrices)?;
    let deleted = store::delete_route_price(&state.poller.pool, user.tenant_id, id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /` (public, rate-limited) merged with `POST /`, `PUT/DELETE /{id}` (session_auth +
/// `ManagePrices`) — same `public.merge(protected)` shape `routes/auth.rs::auth_router` already
/// established for `/portal-login` vs. `/me`+`/logout`. Different HTTP methods at the SAME path
/// (`GET "/"` in `public`, `POST "/"` in `protected`) compose cleanly under `Router::merge` —
/// axum only rejects merging the SAME method at the same path twice, not different methods.
pub fn prices_router(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/", get(list_prices))
        .route_layer(crate::middleware::public_rate_limit_layer());

    let protected = Router::new()
        .route("/", post(create_price))
        .route("/{id}", put(update_price).delete(delete_price))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth));

    public.merge(protected)
}
```

- [ ] **Step 4: Wire the module**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod prices;
```
```rust
// Backend/crates/api-gateway/src/lib.rs — add to build_router, alongside the other .nest(...) calls
        .nest("/prices", routes::prices::prices_router(state.clone()))
```

- [ ] **Step 5: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/prices_routes.rs (new file)
//! `GET /prices` (public) + `POST/PUT/DELETE /prices` (`ManagePrices`-gated).
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
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
        .bind("Prices Test Tenant")
        .bind(format!("prices-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await
        .expect("create portal user")
        .id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
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
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
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
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next())
        .map(|s| s.to_string())
        .expect("session cookie must be set")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

#[tokio::test]
async fn get_prices_is_public_and_lists_seeded_rows() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    store::create_route_price(
        &pool,
        tenant_id,
        &store::NewRoutePrice {
            route_code: "AAA".to_string(),
            region: "".to_string(),
            origin: "Padang DC".to_string(),
            destinations: serde_json::json!(["Cileungsi DC"]),
            price: 100,
            vehicle_type: "TRONTON".to_string(),
        },
    )
    .await
    .expect("seed price");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // NO cookie at all — must still succeed (public route).
    let resp = http.get(format!("{base}/prices")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["route_code"], "AAA");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn create_price_requires_main_account_and_validates_destinations() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let helper_cookie = login_cookie(&http, &base, "helper").await;
    let sub_user_resp = http
        .post(format!("{base}/prices"))
        .header("Cookie", &helper_cookie)
        .json(&serde_json::json!({
            "route_code": "BBB", "origin": "X", "destinations": ["Y"], "price": 1, "vehicle_type": "V"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(sub_user_resp.status(), 403);

    let owner_cookie = login_cookie(&http, &base, "owner").await;
    let bad_dest_resp = http
        .post(format!("{base}/prices"))
        .header("Cookie", &owner_cookie)
        .json(&serde_json::json!({
            "route_code": "CCC", "origin": "X", "destinations": [], "price": 1, "vehicle_type": "V"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_dest_resp.status(), 400, "empty destinations array must be rejected");

    let good_resp = http
        .post(format!("{base}/prices"))
        .header("Cookie", &owner_cookie)
        .json(&serde_json::json!({
            "route_code": "DDD", "origin": "X", "destinations": ["Y"], "price": 500, "vehicle_type": "V"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(good_resp.status(), 200);

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 6: Run the tests, then full crate verification**

Run: `cargo test -p api-gateway --test prices_routes -- --test-threads=1` — both PASS. Check `insert_portal_user`'s exact `store::portal_users::create` argument order against `Backend/crates/api-gateway/tests/bookings_routes.rs`'s ALREADY-WORKING helper before assuming the signature above is right — Tasks 8/9/10/11 of Fase 6c each independently had to correct a brief's guess at this exact signature; copy the verified-working shape, don't re-guess it.

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings` — 0 failures, clean.

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/api-gateway/src/middleware/rate_limit.rs Backend/crates/api-gateway/src/middleware/mod.rs \
        Backend/crates/api-gateway/src/routes/prices.rs Backend/crates/api-gateway/src/routes/mod.rs \
        Backend/crates/api-gateway/src/lib.rs Backend/crates/api-gateway/tests/prices_routes.rs
git commit -m "feat(api-gateway): GET /prices (public, rate-limited) + POST/PUT/DELETE /prices (ManagePrices)"
```

---

## Task 5: `GET/POST/DELETE /locations`

**Context:** Unlike `/prices`, `/locations` is NOT public — confirmed against the reference (explicitly excluded from the public-GET exemption list: "Semua endpoint di bawah guard session global — tidak publik"). `GET` needs only `session_auth`; `POST`/`DELETE` need `Permission::ManageLocations`. No `PUT` — locations are add/delete-only (Task 2's store layer has no `update` fn, matching the schema's lack of `updated_at`).

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/locations.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs` (mount)
- Test: `Backend/crates/api-gateway/tests/locations_routes.rs`

**Interfaces:**
- Consumes: `store::{list_route_locations, create_route_location, delete_route_location}` (Task 2).

- [ ] **Step 1: Write the route module**

```rust
// Backend/crates/api-gateway/src/routes/locations.rs
//! `GET/POST/DELETE /locations` — session-auth-only for `GET` (any tenant member sees the
//! location list, matching this project's established data-visibility model), `Permission::
//! ManageLocations`-gated for `POST`/`DELETE`. No `PUT` — locations are add/delete-only (no
//! `updated_at` column, Task 2's store layer has no `update` fn either).
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct LocationItem {
    pub id: Uuid,
    pub name: String,
}

impl From<store::models::RouteLocation> for LocationItem {
    fn from(l: store::models::RouteLocation) -> Self {
        Self { id: l.id, name: l.name }
    }
}

#[derive(Debug, Deserialize)]
pub struct LocationInput {
    pub name: String,
}

async fn list_locations(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<LocationItem>>, ApiError> {
    let rows = store::list_route_locations(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(rows.into_iter().map(LocationItem::from).collect()))
}

async fn create_location(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<LocationInput>,
) -> Result<Json<LocationItem>, ApiError> {
    require_permission(&user, Permission::ManageLocations)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("name is required".to_string()));
    }
    let row = store::create_route_location(&state.poller.pool, user.tenant_id, name).await?;
    Ok(Json(LocationItem::from(row)))
}

async fn delete_location(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManageLocations)?;
    let deleted = store::delete_route_location(&state.poller.pool, user.tenant_id, id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

pub fn locations_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(list_locations).post(create_location))
        .route("/{id}", axum::routing::delete(delete_location))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [ ] **Step 2: Wire the module**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod locations;
```
```rust
// Backend/crates/api-gateway/src/lib.rs — add to build_router
        .nest("/locations", routes::locations::locations_router(state.clone()))
```

- [ ] **Step 3: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/locations_routes.rs (new file)
//! `GET/POST/DELETE /locations` — session-auth-gated read, `ManageLocations`-gated writes.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
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
    redis::Client::open(redis_url()).expect("open redis client").get_connection_manager().await.expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id).bind("Locations Test Tenant").bind(format!("locations-test-{id}"))
        .execute(pool).await.expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await.expect("create portal user").id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor), client: Arc::new(client), pool: pool.clone(),
        config: poller::PollerConfig::default(), accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar), notifier: None, redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared, ws_hub: ws_hub::Hub::new(), tenant_id,
        cors_origins: Arc::new(vec![]), session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false, master_key: test_master_key(), redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http.post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send().await.expect("login request");
    assert_eq!(resp.status(), 200);
    resp.headers().get_all("set-cookie").iter().find_map(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next()).map(|s| s.to_string())
        .expect("session cookie must be set")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

#[tokio::test]
async fn locations_require_session_and_gate_writes_on_main_account() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let unauth = http.get(format!("{base}/locations")).send().await.unwrap();
    assert_eq!(unauth.status(), 401);

    let helper_cookie = login_cookie(&http, &base, "helper").await;
    let read_resp = http.get(format!("{base}/locations")).header("Cookie", &helper_cookie).send().await.unwrap();
    assert_eq!(read_resp.status(), 200);

    let write_resp = http.post(format!("{base}/locations")).header("Cookie", &helper_cookie)
        .json(&serde_json::json!({"name": "Padang DC"})).send().await.unwrap();
    assert_eq!(write_resp.status(), 403, "sub-user must not create locations");

    let owner_cookie = login_cookie(&http, &base, "owner").await;
    let create_resp = http.post(format!("{base}/locations")).header("Cookie", &owner_cookie)
        .json(&serde_json::json!({"name": "Padang DC"})).send().await.unwrap();
    assert_eq!(create_resp.status(), 200);
    let created: serde_json::Value = create_resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let delete_resp = http.delete(format!("{base}/locations/{id}")).header("Cookie", &owner_cookie)
        .send().await.unwrap();
    assert_eq!(delete_resp.status(), 204);

    let listed = http.get(format!("{base}/locations")).header("Cookie", &owner_cookie).send().await.unwrap();
    let listed_body: Vec<serde_json::Value> = listed.json().await.unwrap();
    assert_eq!(listed_body.len(), 0);

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 4: Run the tests, then full crate verification + commit**

Run: `cargo test -p api-gateway --test locations_routes -- --test-threads=1` — PASS. Then `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings` — clean.

```bash
git add Backend/crates/api-gateway/src/routes/locations.rs Backend/crates/api-gateway/src/routes/mod.rs \
        Backend/crates/api-gateway/src/lib.rs Backend/crates/api-gateway/tests/locations_routes.rs
git commit -m "feat(api-gateway): GET/POST/DELETE /locations"
```

---

## Task 6: extend `WahaSettings` + `GET/PUT /bot/settings`

**Context:** `spx_client::waha_settings::WahaSettings` (Fase 3, extended in 6b with `wa_number`) only carries WAHA connection info — no `enabled`/`webhook_url`/`wa_group`/`portal_label`. `api_gateway::routes::otp::load_bot_settings` (already merged) hardcodes these 4 fields to zero-value with a comment pointing at 6d. This task extends the struct (keeping ONE `site_settings` row at `key="waha_settings"` as the single source of truth — NOT the reference's plaintext-Redis storage, NOT a second `site_settings` key), fixes `load_bot_settings` to read the real values, and builds `GET/PUT /bot/settings`.

**Security-critical points:**
- `Permission::ManageBotSettings` gates **BOTH** `GET` and `PUT` — unlike this crate's usual "GET = any session" convention (the reference itself main-account-gates reading WAHA config).
- The API key is **never echoed back** in `GET`'s response — only `waha_api_key_set: bool`.
- A blank `waha_api_key` in the `PUT` body means "keep the previously configured key" — never wipes a configured value with an empty field.
- An SSRF guard (`is_safe_outbound_url`, ported from the reference's `isSafeOutboundUrl`) rejects internal/loopback/link-local hosts for BOTH `waha_url` and `webhook_url` before storing — defends against a malicious main-account URL rewrite redirecting OTP delivery to an attacker endpoint. Implemented with plain string parsing (no new `url` crate dependency — this crate has none today and this plan adds none).

**Files:**
- Modify: `Backend/crates/spx-client/src/waha_settings.rs`
- Modify: `Backend/crates/api-gateway/src/routes/otp.rs` (fix `load_bot_settings` to read real values)
- Create: `Backend/crates/api-gateway/src/routes/bot.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs` (mount)
- Test: `Backend/crates/api-gateway/tests/bot_routes.rs`

**Interfaces:**
- Produces: `spx_client::waha_settings::WahaSettings` gains `enabled: bool, webhook_url: String, wa_group: String, portal_label: String` (all `#[serde(default)]`).

- [ ] **Step 1: Extend `WahaSettings`**

```rust
// Backend/crates/spx-client/src/waha_settings.rs — replace the struct definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WahaSettings {
    #[serde(default)]
    pub waha_url: String,
    #[serde(default)]
    pub waha_session: String,
    #[serde(default)]
    pub wa_number: String,
    /// Fase 6d Task 6 additions — `#[serde(default)]` for the same forward-compat reason as
    /// `wa_number` (Fase 6b): a `site_settings` row encoded before this field existed still
    /// decodes cleanly (as its zero value). `otp.rs::load_bot_settings` (6b) is no longer the
    /// only reader — `GET/PUT /bot/settings` (6d) is this field's first real read+write path.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default)]
    pub wa_group: String,
    #[serde(default)]
    pub portal_label: String,
    /// base64(STANDARD) of the AES-256-GCM ciphertext of the API key.
    pub api_key_ciphertext_b64: String,
    /// base64(STANDARD) of the 12-byte nonce.
    pub api_key_nonce_b64: String,
    /// Master-key version used to encrypt the API key.
    pub key_version: i32,
}
```
```rust
// Backend/crates/spx-client/src/waha_settings.rs — encrypt_new's struct literal gains the 4 new
// fields, all zero-value (same "constructor's job is connection info + encrypted key; callers set
// the rest afterward" precedent Fase 6b's `wa_number` already established)
    pub fn encrypt_new(
        master: &MasterKey,
        tenant_id: Uuid,
        waha_url: &str,
        waha_session: &str,
        api_key: &str,
    ) -> Result<Self, CryptoError> {
        let ct = encrypt_waha_key(master, tenant_id, api_key)?;
        Ok(WahaSettings {
            waha_url: waha_url.to_string(),
            waha_session: waha_session.to_string(),
            wa_number: String::new(),
            enabled: false,
            webhook_url: String::new(),
            wa_group: String::new(),
            portal_label: String::new(),
            api_key_ciphertext_b64: STANDARD.encode(&ct.bytes),
            api_key_nonce_b64: STANDARD.encode(ct.nonce),
            key_version: KEY_VERSION,
        })
    }
```

- [ ] **Step 2: Run `spx-client`'s existing tests to confirm the additive change is safe**

Run: `cargo test -p spx-client -- --test-threads=1`
Expected: all pass, including `roundtrip_in_memory_and_plaintext_never_in_json` (`waha_settings.rs`'s own test) — confirms the new fields don't break JSON round-tripping or the plaintext-never-in-JSON guarantee.

- [ ] **Step 3: Fix `otp.rs::load_bot_settings` to read the real values instead of hardcoding zero**

```rust
// Backend/crates/api-gateway/src/routes/otp.rs — replace load_bot_settings' final
// `Ok(notifier::BotSettings { ... })` block
    Ok(notifier::BotSettings {
        enabled: waha.enabled,
        webhook_url: waha.webhook_url,
        wa_group: waha.wa_group,
        wa_number: waha.wa_number,
        waha_url: waha.waha_url,
        waha_api_key: api_key.expose_secret().to_string(),
        waha_session: waha.waha_session,
        portal_label: waha.portal_label,
    })
```
Also update this function's doc comment (currently explains the 4-field gap this step closes) to reflect that the gap is now closed — a short note that Fase 6d Task 6 extended `WahaSettings` with the missing fields, replacing the "defaults all four to zero" sentence.

Run: `cargo test -p api-gateway --test otp_routes -- --test-threads=1` — confirm all pre-existing OTP tests still pass (this change only widens what `load_bot_settings` reads; it does not change `request_otp`/`verify_otp`'s own logic).

- [ ] **Step 4: Write the route module**

```rust
// Backend/crates/api-gateway/src/routes/bot.rs
//! `GET/PUT /bot/settings` — WAHA/n8n bot configuration. `Permission::ManageBotSettings`-gated
//! on BOTH verbs (this crate's usual convention is "GET = any session, mutation = gated" — this
//! route is a deliberate exception, matching the reference's own behavior: WAHA connection info
//! is sensitive enough that even reading it requires main-account, even with the API key masked).
use axum::extract::{Extension, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};

#[derive(Debug, Serialize)]
pub struct BotSettingsResponse {
    pub enabled: bool,
    pub webhook_url: String,
    pub wa_number: String,
    pub wa_group: String,
    pub waha_url: String,
    pub waha_session: String,
    /// The API key is NEVER echoed back — only whether one is currently configured, matching the
    /// reference's own `{...s, wahaApiKey: '', wahaApiKeySet: !!s.wahaApiKey}` masking.
    pub waha_api_key_set: bool,
}

#[derive(Debug, Deserialize)]
pub struct BotSettingsRequest {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook_url: String,
    #[serde(default)]
    pub wa_number: String,
    #[serde(default)]
    pub wa_group: String,
    #[serde(default)]
    pub waha_url: String,
    #[serde(default)]
    pub waha_session: String,
    /// Blank = "keep the previously configured key" — never wipes a configured value with an
    /// empty PUT body field, matching the reference's own `bot.ts` semantic exactly.
    #[serde(default)]
    pub waha_api_key: String,
}

/// Ports the reference's `isSafeOutboundUrl` SSRF guard, applied to BOTH `waha_url` and
/// `webhook_url` before storing. No `url`-crate dependency — plain string parsing (scheme prefix
/// + host substring up to the first `/`/`:`/`?`/`#`) is sufficient for this narrow check and adds
/// no new Cargo.toml entry. Empty string is considered safe ("disabled"/"keep previous").
fn is_safe_outbound_url(raw: &str) -> bool {
    let s = raw.trim();
    if s.is_empty() {
        return true;
    }
    let after_scheme = if let Some(rest) = s.strip_prefix("https://") {
        rest
    } else if let Some(rest) = s.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    let host_end = after_scheme.find(['/', ':', '?', '#']).unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host == "::1" || host.ends_with(".local") || host == "0.0.0.0" {
        return false;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let o = ip.octets();
        if o[0] == 127
            || o[0] == 10
            || (o[0] == 192 && o[1] == 168)
            || (o[0] == 169 && o[1] == 254)
            || (o[0] == 172 && (16..=31).contains(&o[1]))
        {
            return false;
        }
    }
    true
}

fn empty_waha_settings() -> WahaSettings {
    WahaSettings {
        waha_url: String::new(),
        waha_session: String::new(),
        wa_number: String::new(),
        enabled: false,
        webhook_url: String::new(),
        wa_group: String::new(),
        portal_label: String::new(),
        api_key_ciphertext_b64: String::new(),
        api_key_nonce_b64: String::new(),
        key_version: 0,
    }
}

fn to_response(waha: &WahaSettings) -> BotSettingsResponse {
    BotSettingsResponse {
        enabled: waha.enabled,
        webhook_url: waha.webhook_url.clone(),
        wa_number: waha.wa_number.clone(),
        wa_group: waha.wa_group.clone(),
        waha_url: waha.waha_url.clone(),
        waha_session: waha.waha_session.clone(),
        waha_api_key_set: !waha.api_key_ciphertext_b64.is_empty(),
    }
}

async fn get_settings(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<BotSettingsResponse>, ApiError> {
    require_permission(&user, Permission::ManageBotSettings)?;
    let value = store::site_settings::get(&state.poller.pool, user.tenant_id, SITE_SETTINGS_KEY).await?;
    let waha = match value {
        Some(v) => WahaSettings::from_json_value(&v)
            .map_err(|e| ApiError::Internal(format!("corrupt waha_settings row: {e}")))?,
        None => empty_waha_settings(),
    };
    Ok(Json(to_response(&waha)))
}

async fn put_settings(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<BotSettingsRequest>,
) -> Result<Json<BotSettingsResponse>, ApiError> {
    require_permission(&user, Permission::ManageBotSettings)?;

    if !is_safe_outbound_url(&body.waha_url) {
        return Err(ApiError::BadRequest("waha_url points to a disallowed host".to_string()));
    }
    if !is_safe_outbound_url(&body.webhook_url) {
        return Err(ApiError::BadRequest("webhook_url points to a disallowed host".to_string()));
    }

    let existing = store::site_settings::get(&state.poller.pool, user.tenant_id, SITE_SETTINGS_KEY)
        .await?
        .map(|v| WahaSettings::from_json_value(&v))
        .transpose()
        .map_err(|e| ApiError::Internal(format!("corrupt waha_settings row: {e}")))?;

    let mut waha = if body.waha_api_key.trim().is_empty() {
        existing.ok_or_else(|| {
            ApiError::BadRequest("waha_api_key is required on first setup".to_string())
        })?
    } else {
        WahaSettings::encrypt_new(
            &state.master_key,
            user.tenant_id,
            &body.waha_url,
            &body.waha_session,
            &body.waha_api_key,
        )
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?
    };

    waha.waha_url = body.waha_url.trim().to_string();
    waha.waha_session = body.waha_session.trim().to_string();
    waha.wa_number = body.wa_number.trim().to_string();
    waha.enabled = body.enabled;
    waha.webhook_url = body.webhook_url.trim().to_string();
    waha.wa_group = body.wa_group.trim().to_string();

    store::site_settings::put(
        &state.poller.pool,
        user.tenant_id,
        SITE_SETTINGS_KEY,
        &waha.to_json_value(),
    )
    .await?;

    Ok(Json(to_response(&waha)))
}

pub fn bot_settings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [ ] **Step 5: Wire the module (mounted at `/bot`, Task 7 adds `/bot/logs` to the SAME router)**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod bot;
```
```rust
// Backend/crates/api-gateway/src/lib.rs — add to build_router
        .nest("/bot", routes::bot::bot_settings_router(state.clone()))
```

- [ ] **Step 6: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/bot_routes.rs (new file)
//! `GET/PUT /bot/settings` — `ManageBotSettings`-gated on BOTH verbs.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
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
    redis::Client::open(redis_url()).expect("open redis client").get_connection_manager().await.expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id).bind("Bot Test Tenant").bind(format!("bot-test-{id}"))
        .execute(pool).await.expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await.expect("create portal user").id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor), client: Arc::new(client), pool: pool.clone(),
        config: poller::PollerConfig::default(), accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar), notifier: None, redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared, ws_hub: ws_hub::Hub::new(), tenant_id,
        cors_origins: Arc::new(vec![]), session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false, master_key: test_master_key(), redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http.post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send().await.expect("login request");
    assert_eq!(resp.status(), 200);
    resp.headers().get_all("set-cookie").iter().find_map(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next()).map(|s| s.to_string())
        .expect("session cookie must be set")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

#[tokio::test]
async fn sub_user_is_forbidden_on_both_get_and_put() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let helper_cookie = login_cookie(&http, &base, "helper").await;

    let get_resp = http.get(format!("{base}/bot/settings")).header("Cookie", &helper_cookie).send().await.unwrap();
    assert_eq!(get_resp.status(), 403, "GET must also be main-account-gated");

    let put_resp = http.put(format!("{base}/bot/settings")).header("Cookie", &helper_cookie)
        .json(&serde_json::json!({"waha_api_key": "k"})).send().await.unwrap();
    assert_eq!(put_resp.status(), 403);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn put_then_get_never_echoes_the_api_key_and_blank_key_preserves_previous() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let first_put = http.put(format!("{base}/bot/settings")).header("Cookie", &cookie)
        .json(&serde_json::json!({
            "enabled": true, "wa_number": "6281234567890", "waha_url": "http://waha.example.com:3000",
            "waha_session": "default", "waha_api_key": "secret-key-1"
        }))
        .send().await.unwrap();
    assert_eq!(first_put.status(), 200);
    let first_body: serde_json::Value = first_put.json().await.unwrap();
    assert_eq!(first_body["waha_api_key_set"], true);
    assert!(
        first_body.get("waha_api_key").is_none() || first_body["waha_api_key"] == serde_json::Value::Null,
        "the API key must never be echoed back in any form"
    );
    assert!(!first_body.to_string().contains("secret-key-1"), "the plaintext key must not appear anywhere in the response");

    // Second PUT with a BLANK api key — must keep the previously configured key, not wipe it.
    let second_put = http.put(format!("{base}/bot/settings")).header("Cookie", &cookie)
        .json(&serde_json::json!({
            "enabled": false, "wa_number": "6289999999999", "waha_url": "http://waha.example.com:3000",
            "waha_session": "default", "waha_api_key": ""
        }))
        .send().await.unwrap();
    assert_eq!(second_put.status(), 200);
    let second_body: serde_json::Value = second_put.json().await.unwrap();
    assert_eq!(second_body["waha_api_key_set"], true, "a blank api_key on PUT must preserve the previously configured key");
    assert_eq!(second_body["wa_number"], "6289999999999", "non-key fields must still update");
    assert_eq!(second_body["enabled"], false);

    let get_resp = http.get(format!("{base}/bot/settings")).header("Cookie", &cookie).send().await.unwrap();
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["waha_api_key_set"], true);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn ssrf_guard_rejects_internal_hosts() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    for bad_url in ["http://localhost:3000", "http://127.0.0.1:3000", "http://192.168.1.5:3000", "http://10.0.0.1"] {
        let resp = http.put(format!("{base}/bot/settings")).header("Cookie", &cookie)
            .json(&serde_json::json!({"waha_url": bad_url, "waha_api_key": "k"}))
            .send().await.unwrap();
        assert_eq!(resp.status(), 400, "waha_url={bad_url} must be rejected");
    }

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 7: Run the tests, then full crate verification + commit**

Run: `cargo test -p api-gateway --test bot_routes -- --test-threads=1` — all PASS. Then `cargo test -p spx-client -p api-gateway -- --test-threads=1 && cargo clippy -p spx-client -p api-gateway --all-targets -- -D warnings` — clean.

```bash
git add Backend/crates/spx-client/src/waha_settings.rs Backend/crates/api-gateway/src/routes/otp.rs \
        Backend/crates/api-gateway/src/routes/bot.rs Backend/crates/api-gateway/src/routes/mod.rs \
        Backend/crates/api-gateway/src/lib.rs Backend/crates/api-gateway/tests/bot_routes.rs
git commit -m "feat(spx-client,api-gateway): extend WahaSettings + GET/PUT /bot/settings (SSRF-guarded, key-masked)"
```

---

## Task 7: `notifier::bot_log` (Redis ring buffer) + `GET/DELETE /bot/logs` + wiring

**Context:** No TOWER data source exists for bot activity logs — the Fase-2 Postgres `notifications` table is unused by any production code and shaped like a delivery queue, not an audit log. The reference's actual mechanism is a Redis ring buffer (`spx:bot:logs`, `LPUSH`+`LTRIM`, capped at 200 entries), fed from inside the notification-sending code path. This task builds that ring buffer and wires it into the two existing WAHA-touching call sites — **without changing `notify_accepted`/`notify_agency_loss`'s own signatures** (already-shipped Fase 5 code): each caller calls the new `record` fn explicitly, immediately after its own existing notify call.

**Design decision:** rather than adding a new field to `PollerShared` (which would require another mechanical sweep of every `PollerShared { ... }` test-literal site, as Task 6/7 of Fase 6c already had to do twice), this task extends the EXISTING `poller::publish::RedisPublisher` (already wraps a `ConnectionManager`, already an `Option` field on `PollerShared` for ws-hub pub/sub) with a new method. Zero new `PollerShared` fields, zero mechanical sweep.

**Files:**
- Create: `Backend/crates/notifier/src/bot_log.rs`
- Modify: `Backend/crates/notifier/src/lib.rs` (`pub mod bot_log;`)
- Modify: `Backend/crates/poller/src/publish.rs` (`RedisPublisher::record_bot_log`)
- Modify: `Backend/crates/poller/src/dispatch.rs` (wire into `finalize_win` + the `LostToAgency` branch)
- Modify: `Backend/crates/api-gateway/src/routes/otp.rs` (wire into `request_otp`)
- Modify: `Backend/crates/api-gateway/src/routes/bot.rs` (append `GET/DELETE /logs`)
- Test: `Backend/crates/notifier/tests/bot_log_pg.rs` (misnamed — no Postgres, real Redis; matches this crate's existing `tests/waha_mock.rs`-adjacent naming loosely, use `bot_log_redis.rs` instead), `Backend/crates/api-gateway/tests/bot_routes.rs` (append)

- [ ] **Step 1: Write `notifier::bot_log`**

```rust
// Backend/crates/notifier/src/bot_log.rs
//! Redis-backed bot-activity audit log (`spx:bot:logs`, `LPUSH`+`LTRIM`, capped at 200 entries) —
//! mirrors the reference's own `recordBotLog`/`BOT_LOGS_KEY` mechanism (Fase 6d Task 7).
//! Deliberately NOT wired automatically into `notify_accepted`/`notify_agency_loss`/
//! `waha::send_to_waha_many` — those already-shipped fns' signatures stay untouched; `record` is
//! called EXPLICITLY by each caller immediately after its own existing notify call, keeping this
//! crate's core send path free of a new Redis dependency while still sharing one logging fn.
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

const BOT_LOGS_KEY: &str = "spx:bot:logs";
const MAX_LOGS: isize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotLogEntry {
    pub ts: i64,
    /// `"success"` | `"error"`.
    pub log_type: String,
    /// `"accept"` | `"agency_loss"` | `"otp"` — `None` for a kind this task doesn't produce yet.
    pub kind: Option<String>,
    pub booking_id: Option<String>,
    pub latency_ms: Option<i64>,
    pub rule: Option<String>,
    pub error: Option<String>,
}

/// Best-effort — a serialization or Redis failure here must never propagate to the caller's own
/// (more important) notify call; every error is silently dropped, matching this crate's
/// established fire-and-forget tolerance for anything Redis/WAHA-adjacent.
pub async fn record(redis: &mut ConnectionManager, entry: &BotLogEntry) {
    let Ok(serialized) = serde_json::to_string(entry) else {
        return;
    };
    let _: Result<i64, redis::RedisError> = redis.lpush(BOT_LOGS_KEY, &serialized).await;
    let _: Result<(), redis::RedisError> = redis.ltrim(BOT_LOGS_KEY, 0, MAX_LOGS - 1).await;
}

/// Newest-first (LPUSH prepends, so index 0 is already the newest — no `ORDER BY` needed, unlike
/// every Postgres-backed list fn in this workspace). `limit` is clamped to `[1, MAX_LOGS]`.
pub async fn list(redis: &mut ConnectionManager, limit: isize) -> Vec<BotLogEntry> {
    let clamped = limit.clamp(1, MAX_LOGS);
    let raw: Result<Vec<String>, redis::RedisError> =
        redis.lrange(BOT_LOGS_KEY, 0, clamped - 1).await;
    raw.unwrap_or_default()
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect()
}

pub async fn clear(redis: &mut ConnectionManager) {
    let _: Result<i64, redis::RedisError> = redis.del(BOT_LOGS_KEY).await;
}
```

- [ ] **Step 2: Wire the module**

```rust
// Backend/crates/notifier/src/lib.rs
pub mod bot_log;
```

- [ ] **Step 3: Extend `RedisPublisher`**

```rust
// Backend/crates/poller/src/publish.rs — append inside `impl RedisPublisher`, after
// `publish_ticket_accepted`
    /// Records one bot-activity log entry (Fase 6d Task 7) — reuses this struct's own
    /// `ConnectionManager`, same `.clone()`-then-use pattern every other method here already
    /// follows. `poller` already depends on `notifier` (`PollerShared.notifier`), so this adds
    /// no new Cargo.toml entry.
    pub async fn record_bot_log(&self, entry: &notifier::bot_log::BotLogEntry) {
        let mut con = self.con.clone();
        notifier::bot_log::record(&mut con, entry).await;
    }
```

- [ ] **Step 4: Wire into `dispatch.rs`'s `finalize_win`**

```rust
// Backend/crates/poller/src/dispatch.rs — inside `finalize_win`, extend the existing
// `if let Some(pub_) = &shared.redis { ... }` block (do not add a second, separate `if let`)
    if let Some(pub_) = &shared.redis {
        pub_.publish_ticket_accepted(
            &st.account_id,
            serde_json::json!({
                "bookingId": booking.booking_id,
                "latencyMs": latency_ms,
                "autoAccept": true,
                "rule": meta.name,
            }),
        )
        .await;
        pub_.record_bot_log(&notifier::bot_log::BotLogEntry {
            ts: chrono::Utc::now().timestamp_millis(),
            log_type: "success".to_string(),
            kind: Some("accept".to_string()),
            booking_id: Some(booking.id.clone()),
            latency_ms: Some(latency_ms as i64),
            rule: Some(meta.name.clone()),
            error: None,
        })
        .await;
    }
```

- [ ] **Step 5: Wire into `dispatch.rs`'s `LostToAgency` branch**

```rust
// Backend/crates/poller/src/dispatch.rs — inside dispatch_booking's AgencyDupOutcome::LostToAgency
// arm, AFTER the existing `if let Some(settings) = shared.notifier.clone() { ...tokio::spawn... }`
// block, add a new block (this branch does not touch `shared.redis` today):
                    if let Some(pub_) = &shared.redis {
                        pub_.record_bot_log(&notifier::bot_log::BotLogEntry {
                            ts: chrono::Utc::now().timestamp_millis(),
                            log_type: "error".to_string(),
                            kind: Some("agency_loss".to_string()),
                            booking_id: Some(booking.id.clone()),
                            latency_ms: Some(latency_ms as i64),
                            rule: Some(meta.name.clone()),
                            error: Some(format!("lost to {rival_email}")),
                        })
                        .await;
                    }
```
Read the exact current `LostToAgency` arm in `dispatch.rs` before editing — confirm `rival_email`/`meta`/`latency_ms` are all genuinely in scope at that point (they are, per the existing `notify_agency_loss` call in the same arm using all three already) and match the indentation level of the surrounding match arm exactly.

- [ ] **Step 6: Write a real end-to-end test proving the wiring (extends the existing notifier-wiring test, does not duplicate its setup)**

Read `Backend/crates/poller/tests/notifier_wiring.rs` (already exists, exercises `finalize_win`'s `notify_accepted` dispatch against a real wiremock WAHA server) and ADD one assertion to its existing win-path test (do not write a whole new test file) proving the bot log now also gets a `spx:bot:logs` entry — after the existing test's win assertion, add:

```rust
    // Task 7: the SAME finalize_win call must also have written a bot_log entry.
    let mut redis = redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis");
    let logs = notifier::bot_log::list(&mut redis, 10).await;
    assert_eq!(logs.len(), 1, "finalize_win must record exactly one bot_log entry");
    assert_eq!(logs[0].log_type, "success");
    assert_eq!(logs[0].kind.as_deref(), Some("accept"));
    let _: () = redis::AsyncCommands::del(&mut redis, "spx:bot:logs").await.unwrap_or(());
```
(adjust the exact `redis_url()` helper name/import to match whatever this test file already uses — read it first, don't guess the exact local helper name.)

- [ ] **Step 7: Write `notifier::bot_log`'s own direct test**

```rust
// Backend/crates/notifier/tests/bot_log_redis.rs (new file)
//! `notifier::bot_log` — record/list/clear round trip + the 200-entry cap, against real Redis.
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

async fn connection() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis")
}

#[tokio::test]
async fn record_list_clear_round_trip_newest_first() {
    let mut redis = connection().await;
    notifier::bot_log::clear(&mut redis).await; // start from a known-empty state

    for kind in ["accept", "otp", "agency_loss"] {
        notifier::bot_log::record(
            &mut redis,
            &notifier::bot_log::BotLogEntry {
                ts: 1000,
                log_type: "success".to_string(),
                kind: Some(kind.to_string()),
                booking_id: None,
                latency_ms: None,
                rule: None,
                error: None,
            },
        )
        .await;
    }

    let listed = notifier::bot_log::list(&mut redis, 10).await;
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].kind.as_deref(), Some("agency_loss"), "LPUSH means newest (last-recorded) is first");
    assert_eq!(listed[2].kind.as_deref(), Some("accept"));

    notifier::bot_log::clear(&mut redis).await;
    let after_clear = notifier::bot_log::list(&mut redis, 10).await;
    assert_eq!(after_clear.len(), 0);
}

#[tokio::test]
async fn caps_at_200_entries() {
    let mut redis = connection().await;
    notifier::bot_log::clear(&mut redis).await;

    for i in 0..210 {
        notifier::bot_log::record(
            &mut redis,
            &notifier::bot_log::BotLogEntry {
                ts: i,
                log_type: "success".to_string(),
                kind: None,
                booking_id: None,
                latency_ms: None,
                rule: None,
                error: None,
            },
        )
        .await;
    }

    let listed = notifier::bot_log::list(&mut redis, 250).await;
    assert_eq!(listed.len(), 200, "LTRIM must cap the list at 200 regardless of how many were pushed");
    assert_eq!(listed[0].ts, 209, "the newest 200 must survive, not the oldest");

    notifier::bot_log::clear(&mut redis).await;
}
```

- [ ] **Step 8: Wire into `api-gateway`'s `request_otp`**

```rust
// Backend/crates/api-gateway/src/routes/otp.rs — request_otp, immediately after the existing
// `let (sent, _failed) = send_to_waha_many(&bot, &bot.wa_number, &text).await;` /
// `if sent == 0 { tracing::warn!(...); }` block, BEFORE the final `Ok(Json(OtpOk { ok: true }))`
    notifier::bot_log::record(
        &mut state.redis,
        &notifier::bot_log::BotLogEntry {
            ts: chrono::Utc::now().timestamp_millis(),
            log_type: if sent > 0 { "success".to_string() } else { "error".to_string() },
            kind: Some("otp".to_string()),
            booking_id: None,
            latency_ms: None,
            rule: None,
            error: if sent == 0 {
                Some("zero WAHA sends delivered".to_string())
            } else {
                None
            },
        },
    )
    .await;
```

- [ ] **Step 9: Append `GET/DELETE /bot/logs` to the existing `bot.rs` router**

```rust
// Backend/crates/api-gateway/src/routes/bot.rs — append imports
use axum::routing::delete;
```
```rust
// Backend/crates/api-gateway/src/routes/bot.rs — append handlers

async fn get_logs(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<notifier::bot_log::BotLogEntry>>, ApiError> {
    require_permission(&user, Permission::ManageBotSettings)?;
    Ok(Json(notifier::bot_log::list(&mut state.redis, 200).await))
}

async fn delete_logs(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<axum::http::StatusCode, ApiError> {
    require_permission(&user, Permission::ManageBotSettings)?;
    notifier::bot_log::clear(&mut state.redis).await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
```
```rust
// Backend/crates/api-gateway/src/routes/bot.rs — bot_settings_router, add the /logs route
// (rename the fn to bot_router since it now covers both /settings and /logs)
pub fn bot_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route("/logs", get(get_logs).delete(delete_logs))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```
Update `Backend/crates/api-gateway/src/lib.rs`'s mount line to call the renamed `bot_router` instead of `bot_settings_router`.

- [ ] **Step 10: Write the failing test**

```rust
// Backend/crates/api-gateway/tests/bot_routes.rs — append
#[tokio::test]
async fn bot_logs_records_from_otp_and_can_be_cleared() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let mut redis_check = test_redis_manager().await;
    notifier::bot_log::clear(&mut redis_check).await;

    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let get_resp = http.get(format!("{base}/bot/logs")).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(get_resp.status(), 200);
    let body: Vec<serde_json::Value> = get_resp.json().await.unwrap();
    assert_eq!(body.len(), 0, "no entries recorded yet in this test's own clean Redis state");

    let delete_resp = http.delete(format!("{base}/bot/logs")).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(delete_resp.status(), 204);

    cleanup(&pool, tenant_id).await;
}
```
(This test proves the route wiring, not the OTP-triggered recording — Step 7's `notifier` crate test already proves `record`/`list`/`clear` correctness directly, and Step 6's `poller` test proves `finalize_win`'s wiring; re-testing `request_otp`'s own wiring end-to-end would need a real WAHA wiremock server, already covered by `otp_routes.rs`'s existing `request_otp_sends_to_wa_number_via_waha` test — extending THAT test with a bot_log assertion is optional polish, not required by this task.)

- [ ] **Step 11: Run everything, then full workspace verification**

Run: `cargo test -p notifier --test bot_log_redis -- --test-threads=1` — both PASS.
Run: `cargo test -p poller --test notifier_wiring -- --test-threads=1` — PASS (including the new bot_log assertion).
Run: `cargo test -p api-gateway --test bot_routes -- --test-threads=1` — all PASS.
Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — 0 failures, clean (this task touches 4 crates — `notifier`, `poller`, `api-gateway`, and indirectly re-verifies `spx-client` — a full workspace check is the right scope here, not just the touched crates individually).

- [ ] **Step 12: Commit**

```bash
git add Backend/crates/notifier/src/bot_log.rs Backend/crates/notifier/src/lib.rs \
        Backend/crates/notifier/tests/bot_log_redis.rs \
        Backend/crates/poller/src/publish.rs Backend/crates/poller/src/dispatch.rs \
        Backend/crates/poller/tests/notifier_wiring.rs \
        Backend/crates/api-gateway/src/routes/otp.rs Backend/crates/api-gateway/src/routes/bot.rs \
        Backend/crates/api-gateway/src/lib.rs Backend/crates/api-gateway/tests/bot_routes.rs
git commit -m "feat(notifier,poller,api-gateway): Redis-backed bot_log ring buffer + GET/DELETE /bot/logs + wiring"
```

---

## Task 8: `GET/PUT /branding` + the 15MB body-limit carve-out

**This is the single highest-risk task in this sub-phase.** Verified against `tower-http 0.7.0`'s actual `RequestBodyLimit::call` source BEFORE this plan was written (see the research doc): a more-permissive per-route layer CANNOT override a more-restrictive outer/global one — the outer layer short-circuits on `Content-Length` before routing runs at all, and nested `Limited` body wrapping always enforces the SMALLEST cap encountered regardless of layering order. The fix is NOT an additive inner layer; it requires restructuring `build_router` so branding's sub-router carries its OWN `RequestBodyLimitLayer` and is `.merge()`d in AFTER the rest of the app already has its OWN, separate 1.5MB layer applied — two independently-layered route trees combined, not one router with two competing layers.

**Scope decisions (disclosed):**
- **`/branding/meta`, `/branding/logo`, `/branding/favicon` are deferred** — the reference adds these (versioned-URL indirection so SSR pages don't inline multi-MB data URIs), but the design doc's 6d bullet literally names only `GET/PUT /branding`, and no Fase-7 UI exists yet to consume the optimization. Addable later when Fase 7 needs it (YAGNI — no current consumer).
- **`PUT /branding` is gated on `Permission::ManageBranding`** — stricter than the reference (any session), consistent with every other settings-mutation route in this crate already being main-account-gated.
- **`GET /branding` is public** (no `session_auth`), same `public_rate_limit_layer()` (Task 4) as `GET /prices` — the reference exempts both from its session guard identically.
- Image validation (PNG/JPEG/WEBP only, SVG/ICO rejected, 5MB decoded cap each) implemented via plain string parsing — no `regex` crate dependency (this crate has none today, this plan adds none).

**Files:**
- Create: `Backend/crates/api-gateway/src/branding.rs` (data shape + validation, api-gateway-only — nothing else in the workspace needs `Branding`)
- Create: `Backend/crates/api-gateway/src/routes/branding.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs` (`branding` module + the `build_router` restructuring)
- Test: `Backend/crates/api-gateway/tests/branding_routes.rs`

**Interfaces:**
- Consumes: `store::site_settings::{get, put}` (Task 3), `spx_client::waha_settings::SITE_SETTINGS_KEY`-style constant (this task defines its OWN `"price_page"` constant, a different key — do not confuse with `waha_settings`).

- [ ] **Step 1: Write the `Branding` data + validation module**

```rust
// Backend/crates/api-gateway/src/branding.rs
//! `Branding` — the `site_settings` row at `key = "price_page"` (yes, historically named after
//! the public price page it originally only served; the reference's own naming, kept verbatim
//! for continuity with the stored data shape, not renamed). Validation ports the reference's
//! `validateBranding`/`isSafeOutboundUrl`-adjacent rules exactly: PNG/JPEG/WEBP-only data URIs
//! (SVG/ICO rejected — SVG can embed executable script if opened as a top-level document), 5MB
//! decoded cap each.
use serde::{Deserialize, Serialize};

pub const SITE_SETTINGS_KEY: &str = "price_page";

const TITLE_MAX: usize = 60;
const SUBTITLE_MAX: usize = 160;
const SITE_NAME_MAX: usize = 60;
const BRAND_TAG_MAX: usize = 20;
const LOGO_MAX_BYTES: usize = 5 * 1024 * 1024;
const FAVICON_MAX_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Branding {
    pub title: String,
    pub subtitle: String,
    pub site_name: String,
    pub brand_tag: String,
    pub logo_data_uri: Option<String>,
    pub favicon_data_uri: Option<String>,
}

impl Default for Branding {
    fn default() -> Self {
        Self {
            title: "Harga Harga".to_string(),
            subtitle: "Daftar harga rute per jenis kendaraan — SPX Portal".to_string(),
            site_name: "SPX Agency Portal".to_string(),
            brand_tag: String::new(),
            logo_data_uri: None,
            favicon_data_uri: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BrandingInput {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub site_name: String,
    #[serde(default)]
    pub brand_tag: String,
    #[serde(default)]
    pub logo_data_uri: Option<String>,
    #[serde(default)]
    pub favicon_data_uri: Option<String>,
}

/// Validates a `data:image/(png|jpeg|webp);base64,...` URI, rejecting every other image type
/// (SVG/ICO especially) and anything exceeding `max_bytes` DECODED size. Computes decoded length
/// from base64 length + padding (matching the reference's own `decodedSize()` helper) rather than
/// actually decoding — avoids allocating the full image just to check its size.
fn validate_data_uri(value: &str, max_bytes: usize) -> Result<(), String> {
    let prefixes = [
        "data:image/png;base64,",
        "data:image/jpeg;base64,",
        "data:image/webp;base64,",
    ];
    let Some(b64) = prefixes.iter().find_map(|p| value.strip_prefix(p)) else {
        return Err("must be a data:image/(png|jpeg|webp);base64,... URI (svg/ico are not allowed)".to_string());
    };
    if b64.is_empty()
        || !b64
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    {
        return Err("invalid base64 payload".to_string());
    }
    let padding = b64.chars().rev().take_while(|&c| c == '=').count().min(2);
    let decoded_len = (b64.len() / 4) * 3 - padding;
    if decoded_len > max_bytes {
        return Err(format!("image exceeds {max_bytes} bytes decoded"));
    }
    Ok(())
}

pub fn validate_and_normalize(input: BrandingInput) -> Result<Branding, String> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err("title is required".to_string());
    }
    if title.chars().count() > TITLE_MAX {
        return Err(format!("title exceeds {TITLE_MAX} characters"));
    }
    let subtitle = input.subtitle.trim();
    if subtitle.chars().count() > SUBTITLE_MAX {
        return Err(format!("subtitle exceeds {SUBTITLE_MAX} characters"));
    }
    let site_name_trimmed = input.site_name.trim();
    let site_name = if site_name_trimmed.is_empty() {
        Branding::default().site_name
    } else {
        site_name_trimmed.to_string()
    };
    if site_name.chars().count() > SITE_NAME_MAX {
        return Err(format!("site_name exceeds {SITE_NAME_MAX} characters"));
    }
    let brand_tag = input.brand_tag.trim();
    if brand_tag.chars().count() > BRAND_TAG_MAX {
        return Err(format!("brand_tag exceeds {BRAND_TAG_MAX} characters"));
    }
    let logo_data_uri = input.logo_data_uri.filter(|s| !s.is_empty());
    if let Some(logo) = &logo_data_uri {
        validate_data_uri(logo, LOGO_MAX_BYTES)?;
    }
    let favicon_data_uri = input.favicon_data_uri.filter(|s| !s.is_empty());
    if let Some(favicon) = &favicon_data_uri {
        validate_data_uri(favicon, FAVICON_MAX_BYTES)?;
    }
    Ok(Branding {
        title: title.to_string(),
        subtitle: subtitle.to_string(),
        site_name,
        brand_tag: brand_tag.to_string(),
        logo_data_uri,
        favicon_data_uri,
    })
}
```

- [ ] **Step 2: Write the failing unit tests for validation (pure, no DB/HTTP needed)**

```rust
// Backend/crates/api-gateway/src/branding.rs — append
#[cfg(test)]
mod tests {
    use super::*;

    fn base_input() -> BrandingInput {
        BrandingInput {
            title: "My Title".to_string(),
            subtitle: String::new(),
            site_name: String::new(),
            brand_tag: String::new(),
            logo_data_uri: None,
            favicon_data_uri: None,
        }
    }

    #[test]
    fn blank_title_is_rejected() {
        let mut input = base_input();
        input.title = "   ".to_string();
        assert!(validate_and_normalize(input).is_err());
    }

    #[test]
    fn blank_site_name_falls_back_to_default() {
        let branding = validate_and_normalize(base_input()).expect("valid");
        assert_eq!(branding.site_name, Branding::default().site_name);
    }

    #[test]
    fn svg_data_uri_is_rejected() {
        let mut input = base_input();
        input.logo_data_uri = Some("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=".to_string());
        let err = validate_and_normalize(input).unwrap_err();
        assert!(err.contains("svg/ico are not allowed") || err.contains("must be a data:image"));
    }

    #[test]
    fn oversized_logo_is_rejected() {
        let mut input = base_input();
        // ~6.7MB of base64 'A' characters decodes to ~5.02MB, just over the 5MB cap.
        let huge_b64 = "A".repeat(6_900_000);
        input.logo_data_uri = Some(format!("data:image/png;base64,{huge_b64}"));
        assert!(validate_and_normalize(input).is_err());
    }

    #[test]
    fn valid_png_logo_is_accepted() {
        let mut input = base_input();
        input.logo_data_uri = Some("data:image/png;base64,iVBORw0KGgo=".to_string());
        let branding = validate_and_normalize(input).expect("valid small PNG must pass");
        assert!(branding.logo_data_uri.is_some());
    }
}
```

- [ ] **Step 3: Run the unit tests**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p api-gateway branding:: -- --test-threads=1`
Expected: FAIL (module doesn't exist yet in `lib.rs`) — then wire it in and re-run.

```rust
// Backend/crates/api-gateway/src/lib.rs — add to the top pub mod list
pub mod branding;
```
Re-run: PASS, all 5 tests.

- [ ] **Step 4: Write the route module**

```rust
// Backend/crates/api-gateway/src/routes/branding.rs
//! `GET /branding` (public, rate-limited) + `PUT /branding` (session_auth + `ManageBranding`).
//! Mounted from a SEPARATELY-layered sub-router in `lib.rs::build_router` — see that fn's own
//! doc comment for why the 15MB body-limit carve-out requires this structural split.
use axum::extract::{Extension, State};
use axum::routing::{get, put};
use axum::{Json, Router};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::branding::{validate_and_normalize, Branding, BrandingInput, SITE_SETTINGS_KEY};
use crate::error::ApiError;
use crate::state::AppState;

async fn get_branding(State(state): State<AppState>) -> Result<Json<Branding>, ApiError> {
    let value = store::site_settings::get(&state.poller.pool, state.tenant_id, SITE_SETTINGS_KEY).await?;
    let branding = match value {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => Branding::default(),
    };
    Ok(Json(branding))
}

async fn put_branding(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<BrandingInput>,
) -> Result<Json<Branding>, ApiError> {
    require_permission(&user, Permission::ManageBranding)?;
    let branding = validate_and_normalize(body).map_err(ApiError::BadRequest)?;
    let value = serde_json::to_value(&branding).map_err(|e| ApiError::Internal(e.to_string()))?;
    store::site_settings::put(&state.poller.pool, user.tenant_id, SITE_SETTINGS_KEY, &value).await?;
    Ok(Json(branding))
}

/// `GET /` (public, `public_rate_limit_layer` — Task 4) merged with `PUT /` (session_auth +
/// `ManageBranding`) — same `public.merge(protected)` shape already established by
/// `routes/prices.rs::prices_router` and `routes/auth.rs::auth_router`.
pub fn branding_router(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/", get(get_branding))
        .route_layer(crate::middleware::public_rate_limit_layer());
    let protected = Router::new()
        .route("/", put(put_branding))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth));
    public.merge(protected)
}
```

- [ ] **Step 5: Wire the module**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod branding;
```

- [ ] **Step 6: Restructure `build_router` — the risky part**

```rust
// Backend/crates/api-gateway/src/lib.rs — replace the ENTIRE build_router fn
/// Global request body-limit: 1.5MB, matching the reference's default. `/branding`'s 15MB
/// carve-out (Task 8) is NOT an additive inner layer on top of this — verified against
/// `tower-http 0.7.0`'s actual `RequestBodyLimit::call` source: an outer/global layer
/// short-circuits on `Content-Length` before routing runs, and nested `Limited` body wrapping
/// always enforces the SMALLEST cap regardless of layering order, so a naive "bigger inner
/// layer" would still be capped at 1.5MB. The actual fix: `branding` is built as its OWN
/// `Router`, with its OWN `RequestBodyLimitLayer`, `.merge()`d into `rest` AFTER `rest` already
/// has its OWN separate 1.5MB layer applied — two independently-layered route trees, not one
/// router with two competing layers. `cors_layer`/`security_headers` don't need to differ
/// per-route, so they stay wrapping the FINAL merged whole, same as before this task.
const GLOBAL_BODY_LIMIT_BYTES: usize = 1_500_000;
const BRANDING_BODY_LIMIT_BYTES: usize = 15_000_000;

pub fn build_router(state: AppState) -> Router {
    let rest = Router::new()
        .route("/healthz", get(healthz))
        .nest("/auth", routes::auth::auth_router(state.clone()))
        .nest("/auth", routes::otp::otp_router(state.clone()))
        .nest(
            "/auth/spx-credentials",
            routes::spx_credentials::spx_credentials_router(state.clone()),
        )
        .nest(
            "/auth/spx-login",
            routes::spx_login::spx_login_router(state.clone()),
        )
        .nest(
            "/auth/portal-users",
            routes::portal_users::portal_users_router(state.clone()),
        )
        .nest(
            "/bookings",
            routes::bookings::bookings_router(state.clone())
                .merge(routes::rules::rules_router(state.clone())),
        )
        .nest("/prices", routes::prices::prices_router(state.clone()))
        .nest("/locations", routes::locations::locations_router(state.clone()))
        .nest("/bot", routes::bot::bot_router(state.clone()))
        .with_state(state.clone())
        .layer(RequestBodyLimitLayer::new(GLOBAL_BODY_LIMIT_BYTES));

    let branding = Router::new()
        .nest("/branding", routes::branding::branding_router(state.clone()))
        .with_state(state.clone())
        .layer(RequestBodyLimitLayer::new(BRANDING_BODY_LIMIT_BYTES));

    rest.merge(branding)
        .layer(middleware::cors_layer(&state.cors_origins))
        .layer(axum::middleware::from_fn(middleware::security_headers))
}
```

- [ ] **Step 7: Write the failing tests proving BOTH halves of the carve-out**

```rust
// Backend/crates/api-gateway/tests/branding_routes.rs (new file)
//! `GET /branding` (public) + `PUT /branding` (`ManageBranding`) + the 15MB body-limit carve-out
//! itself — proves BOTH (a) branding accepts a body between 1.5MB and 15MB and (b) every OTHER
//! route still correctly 413s above 1.5MB, per Task 8's own risk note.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
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
    redis::Client::open(redis_url()).expect("open redis client").get_connection_manager().await.expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id).bind("Branding Test Tenant").bind(format!("branding-test-{id}"))
        .execute(pool).await.expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await.expect("create portal user").id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor), client: Arc::new(client), pool: pool.clone(),
        config: poller::PollerConfig::default(), accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar), notifier: None, redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared, ws_hub: ws_hub::Hub::new(), tenant_id,
        cors_origins: Arc::new(vec![]), session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false, master_key: test_master_key(), redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http.post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send().await.expect("login request");
    assert_eq!(resp.status(), 200);
    resp.headers().get_all("set-cookie").iter().find_map(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next()).map(|s| s.to_string())
        .expect("session cookie must be set")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

/// A real, base64-encoded ~4MB PNG-shaped payload (starts with a real PNG magic-byte-adjacent
/// prefix so it passes `validate_data_uri`'s prefix check; content past that is irrelevant filler
/// for THIS test's purpose — proving the body-limit carve-out, not image correctness).
fn big_valid_logo_data_uri(approx_decoded_bytes: usize) -> String {
    let b64_len = (approx_decoded_bytes / 3) * 4;
    format!("data:image/png;base64,{}", "A".repeat(b64_len))
}

#[tokio::test]
async fn get_branding_is_public_and_returns_defaults_when_unconfigured() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/branding")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "GET /branding must be reachable with no session cookie at all");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["site_name"], "SPX Agency Portal");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn put_branding_accepts_a_4mb_body_but_prices_still_rejects_it() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    // ~4MB decoded logo — well over the GLOBAL 1.5MB limit, well under branding's 15MB one.
    let logo = big_valid_logo_data_uri(4_000_000);
    let put_resp = http
        .put(format!("{base}/branding"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"title": "Test", "logo_data_uri": logo}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        put_resp.status(),
        200,
        "a ~4MB branding PUT must succeed — this is the carve-out's whole point"
    );

    let get_resp = http.get(format!("{base}/branding")).send().await.unwrap();
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["title"], "Test");
    assert!(get_body["logo_data_uri"].as_str().unwrap().len() > 1_500_000, "the stored logo must be the full oversized payload, not truncated");

    // A DIFFERENT route (not branding) must STILL reject a body over 1.5MB — proves the global
    // layer wasn't accidentally widened for the whole app instead of scoped to just branding.
    let oversized_json = serde_json::json!({"name": "A".repeat(2_000_000)});
    let other_route_resp = http
        .post(format!("{base}/locations"))
        .header("Cookie", &cookie)
        .json(&oversized_json)
        .send()
        .await
        .unwrap();
    assert_eq!(
        other_route_resp.status(),
        413,
        "a >1.5MB body on ANY non-branding route must still be rejected"
    );

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn sub_user_cannot_write_branding_but_can_read_it() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let get_resp = http.get(format!("{base}/branding")).send().await.unwrap();
    assert_eq!(get_resp.status(), 200);

    let helper_cookie = login_cookie(&http, &base, "helper").await;
    let put_resp = http
        .put(format!("{base}/branding"))
        .header("Cookie", &helper_cookie)
        .json(&serde_json::json!({"title": "Hacked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 403);

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 8: Run the tests, then full crate + workspace verification**

Run: `cargo test -p api-gateway --test branding_routes -- --test-threads=1` — all 3 PASS. If `put_branding_accepts_a_4mb_body_but_prices_still_rejects_it` fails on the 200 assertion, the carve-out did NOT work — re-check Step 6's exact layering order against this task's own risk note before touching anything else.

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings` — 0 failures, clean.

Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — this task restructures a load-bearing shared fn (`build_router`) every other route in this crate depends on; a full workspace check is the right scope here, not just `api-gateway`.

- [ ] **Step 9: Commit**

```bash
git add Backend/crates/api-gateway/src/branding.rs Backend/crates/api-gateway/src/routes/branding.rs \
        Backend/crates/api-gateway/src/routes/mod.rs Backend/crates/api-gateway/src/lib.rs \
        Backend/crates/api-gateway/tests/branding_routes.rs
git commit -m "feat(api-gateway): GET/PUT /branding + 15MB body-limit carve-out (build_router restructuring)"
```

---

## Task 9: Final verification + sign-off

**Files:** none (verification-only task, no new code).

- [ ] **Step 1: Full workspace test suite**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo test --workspace -- --test-threads=1 2>&1 | tail -100`
Expected: `0 failed` across every crate.

- [ ] **Step 2: Clippy, workspace-wide, warnings as errors**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: `cargo deny check`**

Run: `cargo deny check`
Expected: `advisories ok, bans ok, licenses ok, sources ok`, exit code 0. This plan adds NO new Cargo.toml dependencies anywhere — confirm `Backend/Cargo.lock`'s diff against the pre-6d state has no new crate entries (only version-bump noise, if any, from the workspace's existing deps).

- [ ] **Step 4: `cargo tree` cross-dependency check (DoD #8)**

Run: `cargo tree -p api-gateway --depth 1 | grep -E "^(store|executor|spx-client|poller|ws-hub|notifier|core-domain) v"` — confirm all 7 present.
Run: `for c in store executor spx-client poller ws-hub notifier core-domain; do cargo tree -p "$c" -i api-gateway 2>&1; done` — every invocation must fail with "did not match any packages" (confirms none of the 7 depends back on `api-gateway` — same verification method Fase 6c's own sign-off used).

- [ ] **Step 5: Body-limit carve-out regression guard**

Run: `cargo test -p api-gateway --test branding_routes put_branding_accepts_a_4mb_body_but_prices_still_rejects_it -- --test-threads=1`
Expected: PASS. This is the single test in this whole sub-phase that most directly proves Task 8's structural risk was correctly resolved — call it out explicitly in the sign-off notes, don't let it blend into the general test-count tally.

- [ ] **Step 6: Checkbox guard**

```bash
grep -c '^\- \[ \]' Docs/superpowers/plans/2026-07-16-fase-6d-prices-branding-locations-bot.md
```
Expected: `0` after converting every real step checkbox to `- [x]` as it completes during execution. Verify via `diff` that only checkbox markers changed (no prose corruption), matching the established procedure from every prior sub-phase's sign-off.

- [ ] **Step 7: Definition of Done cross-check, scoped to THIS sub-phase's slice**

Re-read `Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md`'s DoD list. Confirm 6d's contribution:
- #1 (route-level parity): `GET /prices` + CRUD, `GET/PUT /branding`, `GET/POST/DELETE /locations`, `GET/PUT /bot/settings`, `GET/DELETE /bot/logs` — all present with route-level tests. `/branding/meta`, `/branding/logo`, `/branding/favicon` deliberately deferred (disclosed, Task 8) — full #1 still needs 6e's quick-accept routes, not this sub-phase's job to close alone.
- #2 (`require_permission` on every mutating route, tested): `POST/PUT/DELETE /prices` (`ManagePrices`), `POST/DELETE /locations` (`ManageLocations`), `PUT /bot/settings` (`ManageBotSettings`, uniquely also gating its own `GET`), `PUT /branding` (`ManageBranding`) — all tested, all main-account-gated, no exceptions in this sub-phase (unlike 6c's disclosed manual-accept asymmetry).
- #5 (body-limit carve-out): closed by Task 8, with a dedicated regression test (Step 5 above) proving both directions.
- #5 (public-GET rate limit, 120/min/IP): closed by Task 4's `public_rate_limit_layer`, applied to `GET /prices` and `GET /branding`.
- Do NOT claim #3/#4/#6/#7/#8 as closed by 6d alone — #7/#8 ARE re-verified by Steps 1-4 above (workspace-wide), but were not exclusively this sub-phase's to close.

- [ ] **Step 8: Update the progress ledger**

Append one line per task plus a closing summary line: `Fase 6d (prices, branding, locations, bot settings): all 9 tasks complete. Proceeding to final whole-branch review.`

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "test(fase-6d): prices/branding/locations/bot-settings sign-off — full workspace verification"
```

---

## Self-Review Notes (writing-plans skill, run by the plan author before handoff)

**Spec coverage:** every bullet in the shared design doc's 6d scope line has a task — `GET /prices` + CRUD (Task 4), `GET/PUT /branding` (Task 8), `GET/POST/DELETE /locations` (Task 5), `GET/PUT /bot/settings` incl. `wa_number` (Task 6), `GET/DELETE /bot/logs` (Task 7). Three genuine, previously-undiscovered gaps surfaced during planning and are resolved with disclosed, reasoned scope decisions rather than silently invented: the body-limit carve-out's actual layering mechanism (Task 8, verified against `tower-http`'s real source, not assumed), the bot-logs data source (Task 7, a new Redis ring buffer rather than repurposing an ill-shaped unused Postgres table), and `WahaSettings`'s missing 4 fields (Task 6, extended in place rather than a second `site_settings` key or the reference's rejected plaintext-Redis storage).

**Placeholder scan:** no "TBD"/"handle appropriately"/"similar to Task N" patterns — every step carries complete, real code against verified signatures (every `store`/`api-gateway`/`spx-client`/`notifier`/`poller` function or struct this plan touches was read from its actual current source during planning research, and cross-checked against the reference's real source where relevant — SSRF guard, branding validation rules, bot-log shape — not guessed from the design doc's one-line hints alone).

**Type consistency:** `WahaSettings`'s 4 new fields (Task 6) are used identically in `otp.rs`'s fixed `load_bot_settings` and `bot.rs`'s `GET`/`PUT` handlers. `notifier::bot_log::BotLogEntry` (Task 7) is constructed identically by `dispatch.rs`'s two call sites, `otp.rs`'s one call site, and read back identically by `bot.rs`'s `GET /bot/logs` and the direct `notifier` crate test. `Branding`/`BrandingInput` (Task 8) round-trip through `site_settings.value` JSONB via plain `serde_json::to_value`/`from_value`, no adapter layer needed.

**Cross-task dependency order:** Tasks 1-3 (store layer) have no dependency on Tasks 4-9. Task 4 depends only on Task 1. Task 5 depends only on Task 2. Task 6 depends on Task 3 (`site_settings::put`) and is otherwise self-contained (extends `WahaSettings`, fixes `otp.rs`). Task 7 depends on Task 6 (mounts onto the SAME `bot.rs` router Task 6 created) and touches `poller::dispatch.rs`/`publish.rs` independently of any other 6d task. Task 8 depends on Task 3 (`site_settings::put`) and is the only task touching `build_router`'s own structure — ordered LAST among the route tasks specifically so it restructures a `build_router` that already has every other 6d route nested into it via the ordinary `.nest()` path, minimizing the risk of the structural change interacting badly with a route added afterward. This ordering (1→2→3→4→5→6→7→8→9) is a valid topological sort.

