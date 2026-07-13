# TOWER Fase 2 — store + Skema DB Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the 15-table multi-tenant Postgres schema (via forward-only `sqlx migrate`) plus the `store` Rust crate (connection pool, tenant-scoped transactions, 1:1 row structs) that Fase 3+ builds on.

**Architecture:** One `sqlx migrate` file per logical group of tables, applied in dependency order (tenants/users/sessions → credentials/rules → bookings → events → operational tables → RLS). `store` crate holds only types + connection/pool plumbing — no business-logic repository functions yet (those land in the fases that actually consume them, per YAGNI).

**Tech Stack:** `sqlx` (postgres, runtime-tokio-rustls, migrate, macros), `uuid`, `chrono`, PostgreSQL 16 (already running via `Docker/docker-compose.yml`'s `tower-postgres` since Fase 0).

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and [`Docs/superpowers/specs/2026-07-13-fase-2-store-db-design.md`](../specs/2026-07-13-fase-2-store-db-design.md).

- **No PRD document exists** — this schema is a new design per the master spec's own explicit bullets, not a port. Where the master spec is silent on a detail, the design doc's decisions (informed by the reference repo's actual field usage, researched separately) govern — do not invent additional fields beyond what's in the design doc without checking with the controller first.
- Multi-tenant from the root: every business table (all except `tenants` and `archive_runs`) has `tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE` + Row-Level Security.
- **RLS must use `FORCE ROW LEVEL SECURITY`, not just `ENABLE`** — without `FORCE`, RLS is silently bypassed for the table owner, which is exactly the role a simple local Postgres setup connects as. This is the single easiest way to ship RLS that looks correct in the migration but does nothing.
- **`is_coc`'s generated-column predicate must be `^\s*SPXID` (whitespace-tolerant), not `^SPXID`** — the reference repo's own `IS_COC_SQL` constant lacks the `\s*` and is a known, minor inconsistency with its own JS `isCocName` (which IS whitespace-tolerant via `.trim_start()`-equivalent). This plan's generated column corrects that inconsistency to stay semantically identical to Fase 1's Rust `is_coc_name`/`is_coc` — this is a deliberate improvement, not an oversight; do not "fix" it back to match the reference's SQL literally.
- **Partial index predicates must be IMMUTABLE** — never use `now()` (volatile) in a partial index's `WHERE` clause; use it in query predicates instead, backed by a plain (non-partial) index on the relevant column.
- `accept_events` is append-only: an `app_role` Postgres role gets `SELECT, INSERT` but explicitly not `UPDATE, DELETE` — and the verification test must `SET ROLE app_role` before attempting a forbidden write, since table owners bypass GRANT/REVOKE entirely.
- `automation_settings.auto_accept_enabled` defaults to `false` at the schema level — this is Aturan Keras #2 (global kill switch) enforced by the DB, not just application convention.
- Forward-only migrations only — never edit a migration file once another task's migration depends on it. `sqlx migrate add` for new files.
- Cargo workspace: run `cargo` commands from `Backend/` (the workspace root).

---

### Task 1: `store` crate scaffold + `tenants` / `portal_users` / `portal_sessions`

**Files:**
- Create: `Backend/crates/store/Cargo.toml`
- Create: `Backend/crates/store/migrations/0001_tenants.sql`
- Create: `Backend/crates/store/migrations/0002_portal_users.sql`
- Create: `Backend/crates/store/migrations/0003_portal_sessions.sql`
- Create: `Backend/crates/store/src/lib.rs`
- Create: `Backend/crates/store/src/pool.rs`
- Create: `Backend/crates/store/src/models/mod.rs`
- Create: `Backend/crates/store/src/models/tenant.rs`
- Create: `Backend/crates/store/src/models/portal_user.rs`
- Create: `Backend/crates/store/src/models/portal_session.rs`
- Modify: `Backend/Cargo.toml` (add `crates/store` to workspace `members`)

**Interfaces:**
- Consumes: nothing new (first task in this crate).
- Produces: `pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error>`, `pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError>`, `pub async fn begin_tenant_tx(pool: &PgPool, tenant_id: Uuid) -> Result<Transaction<'_, Postgres>, sqlx::Error>` — every later task's tests use `begin_tenant_tx` to scope queries under RLS. `Tenant`, `PortalUser`, `PortalSession` row structs (all `#[derive(Debug, Clone, sqlx::FromRow)]`).

- [ ] **Step 1: Register the crate in the workspace**

Edit `Backend/Cargo.toml`, add `"crates/store"` to the `members` array (alongside the existing 8 crates + 2 bins).

- [ ] **Step 2: Scaffold the crate**

```bash
mkdir -p Backend/crates/store/src/models Backend/crates/store/migrations
cat > Backend/crates/store/Cargo.toml <<'EOF'
[package]
name = "store"
version.workspace = true
edition.workspace = true
publish.workspace = true
EOF
```

```bash
cd Backend
cargo add --package store sqlx --features postgres,runtime-tokio-rustls,macros,migrate,uuid,chrono
cargo add --package store uuid --features v4,serde
cargo add --package store chrono --features serde
cargo add --package store tokio --features rt-multi-thread,macros
cargo add --package store --dev tokio --features rt-multi-thread,macros,test-util
cd ..
```

- [ ] **Step 3: Write the `tenants` migration**

```sql
-- Backend/crates/store/migrations/0001_tenants.sql
CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT tenants_slug_unique UNIQUE (slug)
);
```

- [ ] **Step 4: Write the `portal_users` migration**

```sql
-- Backend/crates/store/migrations/0002_portal_users.sql
CREATE TABLE portal_users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    username TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    display_name TEXT NOT NULL,
    is_main_account BOOLEAN NOT NULL DEFAULT false,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT portal_users_tenant_username_unique UNIQUE (tenant_id, username)
);

CREATE INDEX idx_portal_users_tenant ON portal_users (tenant_id);
```

- [ ] **Step 5: Write the `portal_sessions` migration**

```sql
-- Backend/crates/store/migrations/0003_portal_sessions.sql
CREATE TABLE portal_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL,
    ip TEXT,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT portal_sessions_token_hash_unique UNIQUE (token_hash)
);

CREATE INDEX idx_portal_sessions_user ON portal_sessions (portal_user_id);
-- Plain (non-partial) index: `now()` is volatile and cannot appear in a partial
-- index predicate. Queries filter `WHERE tenant_id = ? AND expires_at > now()`
-- against this composite index instead.
CREATE INDEX idx_portal_sessions_tenant_expires ON portal_sessions (tenant_id, expires_at);
```

- [ ] **Step 6: Write `pool.rs`**

```rust
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new().max_connections(10).connect(database_url).await
}

pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

/// Begin a transaction with `app.tenant_id` set for its duration via
/// `set_config(..., true)` (the `true` = local-to-transaction, matching `SET
/// LOCAL` semantics but parameter-bindable, unlike `SET LOCAL` itself). Every
/// tenant-scoped query MUST go through this — Row-Level Security policies key
/// off `current_setting('app.tenant_id', true)`, so a bare pool connection
/// sees no rows in any tenant-scoped table (RLS defaults to "no match" when
/// the setting is unset).
pub async fn begin_tenant_tx(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
```

Note: `Transaction<'static, Postgres>` requires `sqlx`'s pool-owned transaction (via `PgPool::begin`, which returns an owned transaction, not one borrowed from a `&PgPool` reference) — this is already what `PgPool::begin()` returns, so no lifetime issue in practice; if the compiler disagrees, adjust the signature to `Transaction<'_, Postgres>` bound to the pool's lifetime and propagate that generic parameter instead.

- [ ] **Step 7: Write the row structs**

```rust
// Backend/crates/store/src/models/mod.rs
pub mod portal_session;
pub mod portal_user;
pub mod tenant;

pub use portal_session::PortalSession;
pub use portal_user::PortalUser;
pub use tenant::Tenant;
```

```rust
// Backend/crates/store/src/models/tenant.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/portal_user.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PortalUser {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub display_name: String,
    pub is_main_account: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/portal_session.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PortalSession {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portal_user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}
```

- [ ] **Step 8: Write `lib.rs`**

```rust
pub mod models;
pub mod pool;

pub use pool::{begin_tenant_tx, connect, run_migrations};
```

- [ ] **Step 9: Verify migrations apply and the crate compiles**

Bring up just Postgres (the rest of the stack isn't needed for this task): `cd Docker && docker compose up -d tower-postgres && cd ..`. Wait for healthy (`docker compose ps`), then:

```bash
cd Backend
cargo build -p store
```

Expected: clean build. (No automated test yet in this task — Step 10 is the first one that actually connects and runs migrations; do that as this task's verification.)

- [ ] **Step 10: Write and run a migration-smoke integration test**

Add to `Backend/crates/store/src/lib.rs` (or a new `tests/` file — either is fine, keep it simple as an inline `#[cfg(test)]` module in `lib.rs` for this first task):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:5432/tower".to_string())
    }

    #[tokio::test]
    async fn migrations_apply_and_tenant_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Test Tenant")
            .bind(format!("test-{tenant_id}"))
            .execute(&pool)
            .await
            .expect("insert tenant");

        let fetched: models::Tenant = sqlx::query_as("SELECT * FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("fetch tenant");
        assert_eq!(fetched.name, "Test Tenant");

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
    }
}
```

This test connects to the REAL `tower-postgres` container from Fase 0 — it is not mockable (migrations are the thing under test). `DATABASE_URL` must point at `127.0.0.1:5432` (the container's port is not published by default per Fase 0's "no published ports except edge" rule — for this task, temporarily verify with `docker compose port tower-postgres 5432` or run the test FROM INSIDE a container on `tower-net`; if neither is convenient, add a temporary local-only port publish to `Docker/docker-compose.yml` for `tower-postgres` scoped to `127.0.0.1` for the duration of Fase 2's development, and note this decision in your report — Fase 8's VPS overlay already treats Postgres as internal-only, so a dev-time convenience publish here doesn't violate the "no published ports" rule's intent, which is about production/edge exposure).

Run: `cargo test -p store -- --test-threads=1` (single-threaded: migrations + shared tenant table means concurrent test runs could race on first-run migration application).
Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] **Step 11: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 12: Commit**

```bash
git add Backend/Cargo.toml Backend/crates/store
git commit -m "feat(store): scaffold crate + tenants/portal_users/portal_sessions schema"
```

---

### Task 2: `agency_credentials` / `accept_rules` / `rule_booking_targets`

**Files:**
- Create: `Backend/crates/store/migrations/0004_agency_credentials.sql`
- Create: `Backend/crates/store/migrations/0005_accept_rules.sql`
- Create: `Backend/crates/store/migrations/0006_rule_booking_targets.sql`
- Create: `Backend/crates/store/src/models/agency_credential.rs`
- Create: `Backend/crates/store/src/models/accept_rule.rs`
- Create: `Backend/crates/store/src/models/rule_booking_target.rs`
- Modify: `Backend/crates/store/src/models/mod.rs`

**Interfaces:**
- Consumes: `tenants` (Task 1, FK target).
- Produces: `AgencyCredential`, `AcceptRule` (store's row struct — NOT the same type as `core_domain::AcceptRule`; this one has DB-native types like `Option<f64>`/`Vec<String>` mapped from Postgres arrays, and an extra `route_signature: Option<String>` field the generated column produces), `RuleBookingTarget`. Task 3 (`bookings`) references `accept_rules(id)` via FK.

**Column-naming note:** `accept_rules`'s columns are named to match `core-domain::RuleConditions`'s field names 1:1 (Fase 1, `Backend/crates/core-domain/src/rule.rs`) so a future mapping layer (Fase 3+) is a straight field-for-field copy, not a renaming exercise.

- [ ] **Step 1: Write the `agency_credentials` migration**

```sql
-- Backend/crates/store/migrations/0004_agency_credentials.sql
CREATE TABLE agency_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    username TEXT NOT NULL,
    ciphertext BYTEA NOT NULL,
    nonce BYTEA NOT NULL,
    key_version INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT agency_credentials_tenant_label_unique UNIQUE (tenant_id, label)
);

CREATE INDEX idx_agency_credentials_tenant ON agency_credentials (tenant_id);
```

- [ ] **Step 2: Write the `accept_rules` migration**

**Correction (post-Task-2-review, applied before Task 2 was marked complete):** the original draft below used `REAL`/`array_to_string(destinations, '>')` directly. Real Postgres 16 rejected the generated column outright (`array_to_string` is catalogued `STABLE`, not `IMMUTABLE` — `GENERATED ALWAYS AS (...) STORED` requires every function used to be `IMMUTABLE`), and a design review found two further issues before any production data existed: (1) `REAL` (`f32`) loses precision on money-critical values above ~16.7M — `core_domain::RuleConditions::max_weight`/`max_cod_amount` are `f64` specifically to survive amounts like `4_500_000_000.0` intact (see `core-domain/src/rule.rs`'s test at that magnitude) — a `REAL` column would silently perturb such values by up to ±256 on a write-then-read round trip; (2) the generated `route_signature` must mirror `core_domain::dedupe_rules`'s actual 5-part signature (`norm_loc(origin)|dests_sig|mode|booking_type|service_types_sig`) — the original 4-part version (missing `service_types_sig`, and not normalizing `destinations` the same way as `origin`) would make the DB's dedup unique index **reject legitimate, Rust-validated distinct rules** that share a lane but differ only by `service_types` (a false-positive collision, not merely a weaker backstop). The corrected SQL below fixes all three; use this version, not a literal reading of `REAL`/plain `array_to_string` from any older description of this task.

```sql
-- Backend/crates/store/migrations/0005_accept_rules.sql

-- IMMUTABLE wrapper: array_to_string() is STABLE in Postgres's catalog (a
-- conservative classification for polymorphic array functions in general),
-- even though for a fixed-separator TEXT[] join the result is fully
-- deterministic. Generated-column expressions require every function used
-- to be IMMUTABLE, so this narrow (TEXT[], TEXT) -> TEXT wrapper — not left
-- polymorphic — re-labels exactly the deterministic case.
CREATE OR REPLACE FUNCTION accept_rules_destinations_join_immutable(arr TEXT[], sep TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT array_to_string(arr, sep);
$$;

-- Mirrors core_domain::norm_loc exactly: lowercase, collapse any run of
-- non-alphanumeric characters to a single space, trim leading/trailing space.
CREATE OR REPLACE FUNCTION accept_rules_norm_loc_immutable(s TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT btrim(regexp_replace(lower(s), '[^a-z0-9]+', ' ', 'g'));
$$;

-- Mirrors core_domain::dedupe_rules's dests_sig: each destination run through
-- norm_loc, empties dropped, joined with '>' (order preserved, NOT sorted —
-- matches the Rust implementation).
CREATE OR REPLACE FUNCTION accept_rules_destinations_sig_immutable(arr TEXT[])
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT accept_rules_destinations_join_immutable(
        ARRAY(
            SELECT accept_rules_norm_loc_immutable(elem)
            FROM unnest(arr) AS elem
            WHERE accept_rules_norm_loc_immutable(elem) <> ''
        ),
        '>'
    );
$$;

-- Mirrors core_domain::dedupe_rules's service_types_sig: each entry
-- lowercased+trimmed, empties dropped, SORTED (unlike destinations), joined
-- with ','.
CREATE OR REPLACE FUNCTION accept_rules_service_types_sig_immutable(arr TEXT[])
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT array_to_string(
        ARRAY(
            SELECT lower(btrim(elem))
            FROM unnest(arr) AS elem
            WHERE btrim(elem) <> ''
            ORDER BY 1
        ),
        ','
    );
$$;

CREATE TABLE accept_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT false,
    priority INT NOT NULL DEFAULT 0,
    mode TEXT NOT NULL CHECK (mode IN ('booking_id', 'route', 'filter')),
    service_types TEXT[] NOT NULL DEFAULT '{}',
    max_weight DOUBLE PRECISION,
    coc_only BOOLEAN NOT NULL DEFAULT false,
    non_coc_only BOOLEAN NOT NULL DEFAULT false,
    max_cod_amount DOUBLE PRECISION,
    origin TEXT NOT NULL DEFAULT '',
    destinations TEXT[] NOT NULL DEFAULT '{}',
    booking_type TEXT NOT NULL DEFAULT 'all' CHECK (booking_type IN ('spxid', 'reguler', 'all')),
    shift_types INT[] NOT NULL DEFAULT '{}',
    trip_types INT[] NOT NULL DEFAULT '{}',
    match_mode TEXT NOT NULL DEFAULT 'strict' CHECK (match_mode IN ('strict', 'flexible')),
    min_deadline_min INT,
    max_accept_count INT NOT NULL DEFAULT 0,
    accepted_count INT NOT NULL DEFAULT 0,
    route_signature TEXT GENERATED ALWAYS AS (
        accept_rules_norm_loc_immutable(origin) || '|' ||
        accept_rules_destinations_sig_immutable(destinations) || '|' ||
        match_mode || '|' || booking_type || '|' ||
        accept_rules_service_types_sig_immutable(service_types)
    ) STORED,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT accept_rules_destinations_max5 CHECK (
        array_length(destinations, 1) IS NULL OR array_length(destinations, 1) <= 5
    )
);

CREATE INDEX idx_accept_rules_tenant ON accept_rules (tenant_id);
-- Dedup lane: only one ROUTE-mode rule per tenant may occupy a given
-- normalized lane signature (origin + destinations + match_mode +
-- booking_type + service_types, all normalized identically to
-- core_domain::dedupe_rules). booking_id/filter modes are unrestricted here
-- (their own dedup semantics live in core-domain's dedupe_rules, applied
-- before insert — this index only enforces the route-lane invariant at the
-- DB level as a backstop, and must use the SAME key as the Rust dedup or it
-- will either miss real duplicates or reject legitimate distinct rules).
CREATE UNIQUE INDEX idx_accept_rules_route_dedup ON accept_rules (tenant_id, route_signature)
    WHERE mode = 'route';
```

- [ ] **Step 3: Write the `rule_booking_targets` migration**

```sql
-- Backend/crates/store/migrations/0006_rule_booking_targets.sql
CREATE TABLE rule_booking_targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    rule_id UUID NOT NULL REFERENCES accept_rules(id) ON DELETE CASCADE,
    booking_id_raw TEXT NOT NULL,
    booking_id_norm TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT rule_booking_targets_tenant_norm_unique UNIQUE (tenant_id, booking_id_norm)
);

CREATE INDEX idx_rule_booking_targets_rule ON rule_booking_targets (rule_id);
```

- [ ] **Step 4: Write the row structs**

```rust
// Backend/crates/store/src/models/agency_credential.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgencyCredential {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub label: String,
    pub username: String,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/accept_rule.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AcceptRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
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
    pub route_signature: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/rule_booking_target.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuleBookingTarget {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub rule_id: Uuid,
    pub booking_id_raw: String,
    pub booking_id_norm: String,
    pub created_at: DateTime<Utc>,
}
```

- [ ] **Step 5: Wire into `models/mod.rs`**

Add `pub mod accept_rule; pub mod agency_credential; pub mod rule_booking_target;` and the corresponding `pub use` lines.

- [ ] **Step 6: Run migrations and verify with a round-trip test**

Add a test (same pattern as Task 1's Step 10) that: creates a tenant, inserts an `accept_rules` row with `mode='route', origin='Padang DC', destinations=['Cileungsi DC']` (default `match_mode='strict'`, `booking_type='all'`, `service_types='{}'`), fetches it back, and asserts `route_signature` was computed by Postgres as `"padang dc|cileungsi dc|strict|all|"` (trace this by hand against the corrected generated-column SQL above before writing the assertion — normalized origin, normalized destinations joined by `>`, `match_mode`, `booking_type`, then a trailing `|` before the empty `service_types_sig`). Also insert a second `accept_rules` row with the SAME origin/destinations/mode/service_types and assert the insert **fails** (unique violation) — this proves the dedup index actually fires.

Add a second test proving the `service_types_sig` fix: insert two `accept_rules` rows with the same `origin`/`destinations`/`mode`, but `service_types=['TRONTON']` on one and `service_types=['FUSO']` on the other — assert **both inserts succeed** (no unique violation), proving the DB-level dedup index no longer produces false-positive collisions between legitimately distinct rules that share a lane but differ by service type.

Run: `cargo test -p store -- --test-threads=1`
Expected: all tests pass, including the dedup-collision test (same key → `Err`) and the service-types-distinctness test (different key → both `Ok`).

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/store
git commit -m "feat(store): agency_credentials/accept_rules/rule_booking_targets schema"
```

---

### Task 3: `bookings` — generated columns + hot-path indexes

**Files:**
- Create: `Backend/crates/store/migrations/0007_bookings.sql`
- Create: `Backend/crates/store/src/models/booking.rs`
- Modify: `Backend/crates/store/src/models/mod.rs`

**Interfaces:**
- Consumes: `tenants` (Task 1), `accept_rules` (Task 2, FK target for `rule_matched`).
- Produces: `Booking` row struct with `is_coc`/`needs_enrichment` as read-only generated fields.

**Money-critical detail:** `is_coc`'s predicate must be `^\s*SPXID` (case-insensitive via `~*`), NOT the reference repo's own `^SPXID` (missing `\s*`) — see this plan's Global Constraints. This task's verification step explicitly cross-checks this against Fase 1's Rust `is_coc_name` on the same inputs.

- [ ] **Step 1: Write the `bookings` migration**

```sql
-- Backend/crates/store/migrations/0007_bookings.sql
CREATE TABLE bookings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    spx_id TEXT NOT NULL,
    raw_data JSONB NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    is_coc BOOLEAN GENERATED ALWAYS AS (
        spx_id ~* '^\s*SPXID' OR COALESCE(raw_data->>'booking_name', '') ~* '^\s*SPXID'
    ) STORED,
    needs_enrichment BOOLEAN GENERATED ALWAYS AS (
        (raw_data->>'route_detail_list' IS NULL) AND (raw_data->>'route_stops' IS NULL)
    ) STORED,
    service_type TEXT,
    weight REAL NOT NULL DEFAULT 0,
    cod_amount REAL NOT NULL DEFAULT 0,
    auto_accepted BOOLEAN NOT NULL DEFAULT false,
    accept_latency_ms INT,
    rule_matched UUID REFERENCES accept_rules(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT bookings_tenant_spx_id_unique UNIQUE (tenant_id, spx_id)
);

-- Hot-path: newest-first pending list.
CREATE INDEX idx_bookings_pending ON bookings (tenant_id, created_at DESC) WHERE status = 'pending';
-- Covering index for the live-list UI query (avoids a heap fetch for the common columns).
CREATE INDEX idx_bookings_live_covering ON bookings (tenant_id, status, created_at DESC)
    INCLUDE (spx_id, service_type, weight, cod_amount, auto_accepted);
-- BRIN: bookings is large and append-mostly by created_at — BRIN is far cheaper than
-- B-tree for time-range scans on a table shaped like this.
CREATE INDEX idx_bookings_created_brin ON bookings USING BRIN (created_at);
```

- [ ] **Step 2: Write the row struct**

```rust
// Backend/crates/store/src/models/booking.rs
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub spx_id: String,
    pub raw_data: Value,
    pub status: String,
    /// Read-only — computed by Postgres, never set on INSERT/UPDATE.
    pub is_coc: bool,
    /// Read-only — computed by Postgres, never set on INSERT/UPDATE.
    pub needs_enrichment: bool,
    pub service_type: Option<String>,
    pub weight: f32,
    pub cod_amount: f32,
    pub auto_accepted: bool,
    pub accept_latency_ms: Option<i32>,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

`store` needs `serde_json` for the `Value` type — add it: `cd Backend && cargo add --package store serde_json && cd ..`.

- [ ] **Step 3: Wire into `models/mod.rs`**

Add `pub mod booking;` and `pub use booking::Booking;`.

- [ ] **Step 4: Cross-check `is_coc` against Fase 1's Rust logic**

Add a test that inserts several `bookings` rows exercising the SAME cases Fase 1's `core-domain::coc` tests already cover (`Backend/crates/core-domain/src/coc.rs`'s test module — read it again for the exact input/expected pairs), and asserts the DB-computed `is_coc` matches:

```rust
#[tokio::test]
async fn is_coc_generated_column_matches_core_domain_is_coc_name() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    // (spx_id, booking_name in raw_data, expected is_coc) — mirrors core-domain's
    // coc.rs test cases: SPXID-prefixed (upper/lower/leading-space) -> true,
    // non-SPXID -> false, SPXID-in-middle -> false (must be at start).
    let cases: &[(&str, &str, bool)] = &[
        ("SPXID12345", "", true),
        ("spxid-lower", "", true),
        ("  SPXID-leading-space", "", true),
        ("BK-778899", "", false),
        ("REGULER-1", "", false),
        ("MY-SPXID-suffix", "", false),
        ("884412771", "SPXID99887766", true), // COC via booking_name, not spx_id
        ("884412771", "BK-1", false),
    ];

    for (i, (spx_id, booking_name, expected)) in cases.iter().enumerate() {
        let unique_spx_id = format!("{spx_id}-case{i}");
        let raw_data = serde_json::json!({ "booking_name": booking_name });
        let row: (bool,) = sqlx::query_as(
            "INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, $3) RETURNING is_coc",
        )
        .bind(tenant_id)
        .bind(&unique_spx_id)
        .bind(&raw_data)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("case {i} ({spx_id:?}, {booking_name:?}) insert failed: {e}"));
        assert_eq!(row.0, *expected, "case {i}: spx_id={spx_id:?} booking_name={booking_name:?}");
    }
}
```

(Add a small `insert_test_tenant(pool) -> Uuid` test helper if one doesn't already exist from Task 1/2's tests — reuse it if it does, don't duplicate.)

Run: `cargo test -p store -- --test-threads=1`
Expected: all pass, including every case in the table above matching Fase 1's `is_coc_name`/`is_coc` truth table exactly.

- [ ] **Step 5: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/store Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(store): bookings schema with is_coc/needs_enrichment generated columns"
```

---

### Task 4: `accept_events` — append-only audit trail

**Files:**
- Create: `Backend/crates/store/migrations/0008_accept_events.sql`
- Create: `Backend/crates/store/src/models/accept_event.rs`
- Modify: `Backend/crates/store/src/models/mod.rs`

**Interfaces:**
- Consumes: `tenants`, `bookings` (Task 3), `accept_rules` (Task 2).
- Produces: `AcceptEvent` row struct. The `app_role` Postgres role this task creates is reused by Task 7's RLS setup (every tenant-scoped table's app-facing grants should eventually run as `app_role`, but wiring that up for all 13 tables is Task 7's job — this task only proves the append-only mechanism works for `accept_events` itself).

- [ ] **Step 1: Write the migration**

```sql
-- Backend/crates/store/migrations/0008_accept_events.sql
CREATE TABLE accept_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    booking_id UUID REFERENCES bookings(id) ON DELETE SET NULL,
    rule_id UUID REFERENCES accept_rules(id) ON DELETE SET NULL,
    outcome TEXT NOT NULL CHECK (outcome IN (
        'accepted', 'rejected', 'skipped', 'taken_by_agency', 'failed', 'agency_dup_unverified'
    )),
    local_dispatch_us BIGINT,
    accept_e2e_ms BIGINT,
    detail JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_accept_events_tenant_created ON accept_events (tenant_id, created_at DESC);
CREATE INDEX idx_accept_events_created_brin ON accept_events USING BRIN (created_at);

-- Append-only enforcement: `app_role` may SELECT/INSERT but never UPDATE/DELETE.
-- `CREATE ROLE IF NOT EXISTS` doesn't exist in Postgres — guard with a DO block
-- so this migration is safe to run against a cluster where the role already
-- exists (e.g. a test DB recreated without recreating the whole cluster).
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'app_role') THEN
        CREATE ROLE app_role NOLOGIN;
    END IF;
END
$$;

-- Grant app_role to whichever role runs this migration, so that role (the
-- same one the application connects as, in a simple single-role setup) can
-- `SET ROLE app_role` to prove/exercise the restricted grants.
GRANT app_role TO CURRENT_USER;
GRANT SELECT, INSERT ON accept_events TO app_role;
REVOKE UPDATE, DELETE ON accept_events FROM app_role;
```

- [ ] **Step 2: Write the row struct**

```rust
// Backend/crates/store/src/models/accept_event.rs
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AcceptEvent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub booking_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub outcome: String,
    pub local_dispatch_us: Option<i64>,
    pub accept_e2e_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}
```

- [ ] **Step 3: Wire into `models/mod.rs`**

Add `pub mod accept_event;` and `pub use accept_event::AcceptEvent;`.

- [ ] **Step 4: Write the immutability test**

```rust
#[tokio::test]
async fn accept_events_is_append_only_for_app_role() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let event_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO accept_events (tenant_id, outcome) VALUES ($1, 'accepted') RETURNING id",
    )
    .bind(tenant_id)
    .fetch_one(&pool)
    .await
    .expect("insert event");

    let mut conn = pool.acquire().await.expect("acquire");
    sqlx::query("SET ROLE app_role").execute(&mut *conn).await.expect("set role");

    let update_result = sqlx::query("UPDATE accept_events SET outcome = 'rejected' WHERE id = $1")
        .bind(event_id.0)
        .execute(&mut *conn)
        .await;
    assert!(update_result.is_err(), "app_role must not be able to UPDATE accept_events");

    let delete_result = sqlx::query("DELETE FROM accept_events WHERE id = $1")
        .bind(event_id.0)
        .execute(&mut *conn)
        .await;
    assert!(delete_result.is_err(), "app_role must not be able to DELETE accept_events");

    sqlx::query("RESET ROLE").execute(&mut *conn).await.ok();
}
```

Run: `cargo test -p store -- --test-threads=1`
Expected: all pass, including both `is_err()` assertions genuinely observing a Postgres permission-denied error (paste the actual error text in your report — don't just check `is_err()` blindly; confirm it's a permission error, not some unrelated failure like a bad connection).

- [ ] **Step 5: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/store
git commit -m "feat(store): accept_events append-only audit trail (app_role REVOKE UPDATE/DELETE)"
```

---

### Task 5: `notifications` / `push_subscriptions` / `automation_settings` / `site_settings`

**Files:**
- Create: `Backend/crates/store/migrations/0009_notifications.sql`
- Create: `Backend/crates/store/migrations/0010_push_subscriptions.sql`
- Create: `Backend/crates/store/migrations/0011_automation_settings.sql`
- Create: `Backend/crates/store/migrations/0012_site_settings.sql`
- Create: `Backend/crates/store/src/models/notification.rs`
- Create: `Backend/crates/store/src/models/push_subscription.rs`
- Create: `Backend/crates/store/src/models/automation_settings.rs`
- Create: `Backend/crates/store/src/models/site_settings.rs`
- Modify: `Backend/crates/store/src/models/mod.rs`

**Interfaces:**
- Consumes: `tenants` (Task 1), `portal_users` (Task 1, FK target for `push_subscriptions`).
- Produces: `Notification`, `PushSubscription`, `AutomationSettings`, `SiteSetting` row structs.

**Money-critical detail:** `automation_settings.auto_accept_enabled DEFAULT false` is Aturan Keras #2 (global kill switch) enforced at the schema level — this task's verification step explicitly proves a freshly-inserted row can never come up `true` without an explicit `UPDATE`/insert value.

- [ ] **Step 1: Write the `notifications` migration**

```sql
-- Backend/crates/store/migrations/0009_notifications.sql
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    channel TEXT NOT NULL CHECK (channel IN ('whatsapp', 'push')),
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ
);

-- `status = 'pending'` is an immutable literal comparison (unlike `now()`), so
-- this partial index is valid and is exactly what a `SELECT ... FOR UPDATE
-- SKIP LOCKED` worker-claim query (Fase 5) will scan.
CREATE INDEX idx_notifications_pending ON notifications (created_at) WHERE status = 'pending';
```

- [ ] **Step 2: Write the `push_subscriptions` migration**

```sql
-- Backend/crates/store/migrations/0010_push_subscriptions.sql
CREATE TABLE push_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE,
    endpoint TEXT NOT NULL,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT push_subscriptions_tenant_endpoint_unique UNIQUE (tenant_id, endpoint)
);

CREATE INDEX idx_push_subscriptions_user ON push_subscriptions (portal_user_id);
```

- [ ] **Step 3: Write the `automation_settings` migration**

```sql
-- Backend/crates/store/migrations/0011_automation_settings.sql
CREATE TABLE automation_settings (
    tenant_id UUID PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    auto_accept_enabled BOOLEAN NOT NULL DEFAULT false,
    poll_interval_ms INT NOT NULL DEFAULT 1000,
    smart_paused BOOLEAN NOT NULL DEFAULT false,
    smart_paused_until TIMESTAMPTZ,
    smart_dry_run BOOLEAN NOT NULL DEFAULT false,
    smart_schedule JSONB NOT NULL DEFAULT '{}',
    smart_blacklist TEXT[] NOT NULL DEFAULT '{}',
    counter_reset_hour INT,
    counter_reset_last_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

- [ ] **Step 4: Write the `site_settings` migration**

```sql
-- Backend/crates/store/migrations/0012_site_settings.sql
CREATE TABLE site_settings (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, key)
);
```

- [ ] **Step 5: Write the row structs**

```rust
// Backend/crates/store/src/models/notification.rs
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub channel: String,
    pub payload: Value,
    pub status: String,
    pub attempts: i32,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}
```

```rust
// Backend/crates/store/src/models/push_subscription.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PushSubscription {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portal_user_id: Uuid,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/automation_settings.rs
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AutomationSettings {
    pub tenant_id: Uuid,
    /// Aturan Keras #2 — GLOBAL kill switch. Schema default is `false`;
    /// nothing in this crate ever flips it implicitly.
    pub auto_accept_enabled: bool,
    pub poll_interval_ms: i32,
    pub smart_paused: bool,
    pub smart_paused_until: Option<DateTime<Utc>>,
    pub smart_dry_run: bool,
    pub smart_schedule: Value,
    pub smart_blacklist: Vec<String>,
    pub counter_reset_hour: Option<i32>,
    pub counter_reset_last_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/site_settings.rs
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SiteSetting {
    pub tenant_id: Uuid,
    pub key: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}
```

- [ ] **Step 6: Wire into `models/mod.rs`**

Add all four `pub mod`/`pub use` pairs.

- [ ] **Step 7: Write the kill-switch-default test + round-trip tests**

```rust
#[tokio::test]
async fn automation_settings_auto_accept_defaults_to_false() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    sqlx::query("INSERT INTO automation_settings (tenant_id) VALUES ($1)")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .expect("insert with only tenant_id, everything else default");

    let row: models::AutomationSettings =
        sqlx::query_as("SELECT * FROM automation_settings WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("fetch");
    assert!(!row.auto_accept_enabled, "kill switch must default to false with zero explicit input");
}
```

Add straightforward insert/select round-trip tests for `notifications`, `push_subscriptions`, and `site_settings` too (one each — construct a row, insert, fetch, assert equality on the meaningful fields), following the same pattern as Task 1/3's tests.

Run: `cargo test -p store -- --test-threads=1`
Expected: all pass.

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 9: Commit**

```bash
git add Backend/crates/store
git commit -m "feat(store): notifications/push_subscriptions/automation_settings/site_settings schema"
```

---

### Task 6: `route_prices` / `route_locations` / `archive_runs`

**Files:**
- Create: `Backend/crates/store/migrations/0013_route_prices.sql`
- Create: `Backend/crates/store/migrations/0014_route_locations.sql`
- Create: `Backend/crates/store/migrations/0015_archive_runs.sql`
- Create: `Backend/crates/store/src/models/route_price.rs`
- Create: `Backend/crates/store/src/models/route_location.rs`
- Create: `Backend/crates/store/src/models/archive_run.rs`
- Modify: `Backend/crates/store/src/models/mod.rs`

**Interfaces:**
- Consumes: `tenants` (Task 1).
- Produces: `RoutePrice`, `RouteLocation`, `ArchiveRun` row structs. `archive_runs` is the only table in this plan that is deliberately NOT tenant-scoped (retention is a system-wide maintenance operation, per the design doc) — Task 7 must not add RLS to it.

- [ ] **Step 1: Write the `route_prices` migration**

```sql
-- Backend/crates/store/migrations/0013_route_prices.sql
CREATE TABLE route_prices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    route_code TEXT NOT NULL,
    region TEXT NOT NULL DEFAULT '',
    origin TEXT NOT NULL,
    destinations JSONB NOT NULL,
    price BIGINT NOT NULL,
    vehicle_type TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT route_prices_tenant_code_unique UNIQUE (tenant_id, route_code),
    CONSTRAINT route_prices_destinations_1to5 CHECK (
        jsonb_typeof(destinations) = 'array'
        AND jsonb_array_length(destinations) BETWEEN 1 AND 5
    )
);

CREATE INDEX idx_route_prices_tenant ON route_prices (tenant_id);
```

- [ ] **Step 2: Write the `route_locations` migration**

```sql
-- Backend/crates/store/migrations/0014_route_locations.sql
CREATE TABLE route_locations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT route_locations_tenant_name_unique UNIQUE (tenant_id, name)
);
```

- [ ] **Step 3: Write the `archive_runs` migration**

```sql
-- Backend/crates/store/migrations/0015_archive_runs.sql
-- NOT tenant-scoped: retention is a system-wide maintenance operation
-- (Fase 8), not a per-tenant business record. No RLS on this table.
CREATE TABLE archive_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    table_name TEXT NOT NULL,
    run_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    captured_count BIGINT NOT NULL,
    archived_count BIGINT NOT NULL,
    deleted_count BIGINT NOT NULL,
    archive_path TEXT,
    sha256 TEXT,
    status TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'completed', 'failed')),
    dry_run BOOLEAN NOT NULL DEFAULT false
);
```

- [ ] **Step 4: Write the row structs**

```rust
// Backend/crates/store/src/models/route_price.rs
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoutePrice {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub route_code: String,
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/route_location.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RouteLocation {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}
```

```rust
// Backend/crates/store/src/models/archive_run.rs
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ArchiveRun {
    pub id: Uuid,
    pub table_name: String,
    pub run_at: DateTime<Utc>,
    pub captured_count: i64,
    pub archived_count: i64,
    pub deleted_count: i64,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub status: String,
    pub dry_run: bool,
}
```

- [ ] **Step 5: Wire into `models/mod.rs`**

Add all three `pub mod`/`pub use` pairs.

- [ ] **Step 6: Write tests**

Add: (a) a `route_prices` test proving the CHECK constraint — attempt an insert with `destinations = '[]'::jsonb` (0 items) and assert it fails, attempt one with 6 items and assert it fails, attempt one with 1-5 items and assert it succeeds; (b) straightforward round-trip tests for `route_locations` and `archive_runs`.

```rust
#[tokio::test]
async fn route_prices_destinations_check_enforces_1_to_5() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_test_tenant(&pool).await;

    let insert = |destinations: serde_json::Value, code: String| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO route_prices (tenant_id, route_code, origin, destinations, price, vehicle_type) \
                 VALUES ($1, $2, 'Padang DC', $3, 100000, 'TRONTON')",
            )
            .bind(tenant_id)
            .bind(code)
            .bind(destinations)
            .execute(&pool)
            .await
        }
    };

    assert!(insert(serde_json::json!([]), "zero".into()).await.is_err(), "0 destinations must fail");
    assert!(
        insert(serde_json::json!(["A", "B", "C", "D", "E", "F"]), "six".into()).await.is_err(),
        "6 destinations must fail"
    );
    assert!(
        insert(serde_json::json!(["A", "B", "C"]), "three".into()).await.is_ok(),
        "3 destinations must succeed"
    );
}
```

Run: `cargo test -p store -- --test-threads=1`
Expected: all pass.

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/store
git commit -m "feat(store): route_prices/route_locations/archive_runs schema"
```

---

### Task 7: Row-Level Security across all tenant-scoped tables

**Files:**
- Create: `Backend/crates/store/migrations/0016_rls_policies.sql`

**Interfaces:**
- Consumes: all 13 tenant-scoped tables from Tasks 1-6 (`portal_users, portal_sessions, agency_credentials, accept_rules, rule_booking_targets, bookings, accept_events, notifications, push_subscriptions, automation_settings, site_settings, route_prices, route_locations`).
- Produces: RLS enabled + forced + a `tenant_isolation` policy on each of the 13 tables. `tenants` itself and `archive_runs` are deliberately excluded (see design doc / Task 6).

**The single most important correctness detail in this task:** `ALTER TABLE ... ENABLE ROW LEVEL SECURITY` alone does **not** restrict the table owner — only `FORCE ROW LEVEL SECURITY` does. Since local/dev Postgres connections are very often the table owner (simple single-role setups), a migration that only does `ENABLE` will pass every test trivially (because the test connection bypasses RLS as owner) while providing **zero actual protection** in exactly the same simple-single-role production topology this project is likely to run. Do not skip `FORCE`.

- [ ] **Step 1: Read `pool.rs`'s `begin_tenant_tx` again**

Confirm you understand `set_config('app.tenant_id', $1, true)` sets a transaction-local setting only visible for the duration of that transaction — every test in this task must open a fresh transaction (or `SET`/`RESET` explicitly) per tenant context, not reuse one connection's leftover setting across assertions.

- [ ] **Step 2: Write the RLS migration**

```sql
-- Backend/crates/store/migrations/0016_rls_policies.sql
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'portal_users', 'portal_sessions', 'agency_credentials', 'accept_rules',
        'rule_booking_targets', 'bookings', 'accept_events', 'notifications',
        'push_subscriptions', 'automation_settings', 'site_settings',
        'route_prices', 'route_locations'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I USING (tenant_id = current_setting(''app.tenant_id'', true)::uuid)',
            t
        );
    END LOOP;
END
$$;
```

Note: `current_setting('app.tenant_id', true)` — the second argument `true` means "missing_ok": if no transaction has set `app.tenant_id`, this returns `NULL` rather than raising an error, so `tenant_id = NULL` is simply never true (the query returns zero rows) instead of the whole query erroring out. This is the safer default (an availability/DoS concern: a code path that forgets to set the tenant context should silently see nothing, not crash).

- [ ] **Step 3: Write the cross-tenant isolation test**

```rust
#[tokio::test]
async fn rls_blocks_cross_tenant_reads_on_bookings() {
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");

    let tenant_a = insert_test_tenant(&pool).await;
    let tenant_b = insert_test_tenant(&pool).await;

    // Insert a booking as tenant A.
    let mut tx_a = begin_tenant_tx(&pool, tenant_a).await.expect("tx a");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, '{}')")
        .bind(tenant_a)
        .bind("SPX-CROSS-TENANT-TEST")
        .execute(&mut *tx_a)
        .await
        .expect("insert as tenant a");
    tx_a.commit().await.expect("commit a");

    // Tenant A can see its own row.
    let mut tx_a2 = begin_tenant_tx(&pool, tenant_a).await.expect("tx a2");
    let seen_by_a: Vec<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM bookings WHERE spx_id = 'SPX-CROSS-TENANT-TEST'")
            .fetch_all(&mut *tx_a2)
            .await
            .expect("select as tenant a");
    assert_eq!(seen_by_a.len(), 1, "tenant A must see its own booking");
    tx_a2.commit().await.ok();

    // Tenant B must NOT see tenant A's row.
    let mut tx_b = begin_tenant_tx(&pool, tenant_b).await.expect("tx b");
    let seen_by_b: Vec<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM bookings WHERE spx_id = 'SPX-CROSS-TENANT-TEST'")
            .fetch_all(&mut *tx_b)
            .await
            .expect("select as tenant b");
    assert_eq!(seen_by_b.len(), 0, "tenant B must NOT see tenant A's booking — RLS leak");
    tx_b.commit().await.ok();

    // No tenant context at all (bare pool query, no `app.tenant_id` set): must also see nothing.
    let seen_bare: Vec<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM bookings WHERE spx_id = 'SPX-CROSS-TENANT-TEST'")
            .fetch_all(&pool)
            .await
            .expect("select with no tenant context");
    assert_eq!(seen_bare.len(), 0, "queries with no tenant context set must see nothing, not error or leak");
}

#[tokio::test]
async fn rls_actually_forces_for_table_owner_not_just_enabled() {
    // Regression guard for the FORCE ROW LEVEL SECURITY requirement itself:
    // query pg_tables/pg_class metadata and assert relforcerowsecurity is true
    // for a sample of the 13 tables, so a future migration that drops FORCE
    // (leaving only ENABLE) fails this test immediately instead of silently
    // reintroducing an owner-bypass hole.
    let pool = connect(&test_database_url()).await.expect("connect");
    run_migrations(&pool).await.expect("migrate");

    for table in ["bookings", "accept_rules", "portal_users", "agency_credentials"] {
        let (forced,): (bool,) = sqlx::query_as(
            "SELECT relforcerowsecurity FROM pg_class WHERE relname = $1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("checking relforcerowsecurity for {table}: {e}"));
        assert!(forced, "{table} must have FORCE ROW LEVEL SECURITY set, not just ENABLE");
    }
}
```

Run: `cargo test -p store -- --test-threads=1`
Expected: both new tests pass — the cross-tenant test proving actual data isolation, and the metadata test proving `FORCE` specifically (not just `ENABLE`) is set, so a regression can't silently slip back in.

- [ ] **Step 4: Clippy**

Run: `cargo clippy -p store -- -D warnings` — expected clean.

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/store
git commit -m "feat(store): RLS (ENABLE+FORCE) tenant isolation across 13 business tables"
```

---

### Task 8: Final verification + Fase 2 sign-off

**Files:** None created — this task runs verification commands and checks off the plan.

**Interfaces:**
- Consumes: everything from Tasks 1-7.
- Produces: recorded evidence the Fase 2 Definition of Done (design doc) is met.

- [ ] **Step 1: Full crate test suite from a clean database**

```bash
cd Docker && docker compose down -v tower-postgres 2>/dev/null; docker compose up -d tower-postgres && cd ..
# wait for healthy
cd Backend && cargo test -p store -- --test-threads=1 && cd ..
```

Expected: every test across all 7 prior tasks passes against a genuinely fresh database (proves migrations apply cleanly from zero, not just incrementally on a DB that already had earlier test runs' side effects).

- [ ] **Step 2: `cargo sqlx prepare` — turn the Fase 0 CI placeholder into a real gate**

```bash
cd Backend
export DATABASE_URL="postgres://tower:tower_dev_only@127.0.0.1:5432/tower"
cargo sqlx prepare --workspace
cd ..
```

Expected: succeeds, writes/updates a `.sqlx/` directory at the workspace root with query metadata for every `sqlx::query!`/`query_as!` macro invocation in the codebase (if this plan's tasks only used the runtime `sqlx::query`/`query_as` — non-macro, no compile-time verification — rather than the `!` macro variants, note that explicitly: it means there's nothing yet for `cargo sqlx prepare` to capture, and the Fase 0 CI step's `continue-on-error: true` should be removed only once at least one real `query!`/`query_as!` macro call exists somewhere in the workspace. Check which pattern Tasks 1-7 actually used and report the true state — don't assume).

- [ ] **Step 3: Remove Fase 0's `continue-on-error` on the sqlx-prepare CI step, IF applicable**

If Step 2 produced real `.sqlx/` metadata (i.e. the codebase does use `query!`/`query_as!` macros somewhere), edit `.github/workflows/ci.yml`: remove the `continue-on-error: true` line and its explanatory comment from the "sqlx prepare check" step (added in Fase 0, always intended to become a hard gate "once Fase 2 adds real query! macros" — see that step's existing comment). Commit `.sqlx/` alongside this change. If Tasks 1-7 only used non-macro `sqlx::query`/`query_as` (runtime-checked, not compile-time), leave the CI step as-is and note in your report that this remains deferred to whichever fase first uses the macro form.

- [ ] **Step 4: Full workspace build/test/clippy**

```bash
cd Backend
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cd ..
```

Expected: all clean — `core-domain`'s 127 tests, `store`'s full test suite, `reactor-core`/`auth-sidecar`'s 2 tests, remaining empty crates' 0-test runs.

- [ ] **Step 5: Confirm `store` has no unintended I/O dependencies**

Run: `cd Backend && cargo tree -p store && cd ..`
Expected: `sqlx` (postgres/runtime-tokio-rustls/etc.), `uuid`, `chrono`, `serde_json`, `tokio` — all expected for a DB-access crate. No `reqwest`/`rquest`/`redis` (those belong to `spx-client`/`executor` in later fases, not `store`).

- [ ] **Step 6: Cross-check every DoD item in the design doc**

Read `Docs/superpowers/specs/2026-07-13-fase-2-store-db-design.md`'s "Definition of Done — Fase 2" section and confirm each of its 7 items against the actual repo state (migration files, test results, `pg_class.relforcerowsecurity`, etc.) — don't just assert they're done, cite the actual evidence for each (which migration file, which test, which command output).

- [ ] **Step 7: Mark this plan complete**

Check every remaining `- [ ]` box in this file to `- [x]` by hand or with a targeted script — verify afterward (grep) that no non-checkbox prose containing the literal `- [ ]` substring got corrupted (this exact mistake happened during Fase 0's sign-off and was caught during Fase 1's; do not repeat it a third time).

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/store Docs/superpowers/plans/2026-07-13-fase-2-store-db.md .github/workflows/ci.yml
git commit -m "test(store): Fase 2 sign-off — full verification + DoD cross-check"
```

Fase 2 is done once this commits clean. Fase 3 (spx-client + security kripto) is the next master-spec phase — do not start it in this same task; it gets its own spec/plan cycle.

