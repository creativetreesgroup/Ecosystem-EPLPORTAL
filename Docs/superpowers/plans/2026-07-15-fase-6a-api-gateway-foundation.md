# Fase 6a — api-gateway Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task is executed by a FRESH implementer who sees ONLY that task's text — so every task is self-contained.

**Goal:** Stand up the `api-gateway` crate (session auth + centralized RBAC + security/CORS/rate-limit/body-limit middleware + login/me/logout) and turn `reactor-core` from a `/healthz`-only scaffold into the real binary that constructs a live `PollerShared`, spawns account poller tasks, and mounts `ws-hub` with real session validation. This is sub-phase 1 of 5 for Fase 6 — later sub-phases (6b spx-creds+OTP, 6c bookings+rules, 6d prices/branding/locations, 6e quick-accept+sign-off) build on top of what this plan ships.

**Architecture:** `api-gateway` is a new library crate at the top of the dependency graph (depends on `store`, `executor`, `spx-client`, `poller`, `ws-hub`, `notifier`, `core-domain`) exposing `pub fn build_router(state: AppState) -> Router`. `AppState` (Clone, cheap — `Arc`-backed fields) is `reactor-core`'s one shared context, analogous to `poller::PollerShared` but for the HTTP layer; it WRAPS a `PollerShared` rather than duplicating its fields, since account bootstrap needs the exact same `executor`/`client`/`pool`/etc. Session auth is `axum` middleware: extract the session cookie → `spx_client::crypto::session_token::hash_session_token` → `store::portal_sessions::find_valid_by_hash` (tenant + not-expired) → insert a `CurrentUser` request extension. `require_permission(Permission)` is a small extractor that reads that extension and 403s if the permission's (currently uniform) `is_main_account` requirement isn't met. `reactor-core`'s `main()` resolves the single deployment tenant from `TENANT_SLUG`, connects Postgres as `app_role` (non-superuser — closes the Fase-2-flagged RLS gap), builds a real `PollerShared`, loads each `agency_credentials` row's rules and bootstraps a poller task per row via `poller::ensure_restored_then_spawn`, and mounts three route groups on one `axum::Router`: `api-gateway`'s REST routes, `ws_hub::ws_router` (wrapped with a pre-upgrade session check this plan adds), and `/healthz`.

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and the design doc [`Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md`](../specs/2026-07-15-fase-6-api-gateway-design.md). **Read the design doc before starting; it is the source of truth.** Pay special attention to its "Corrections vs. the reference" (5 numbered deviations) and the tenant-resolution addendum (single configured deployment tenant via `TENANT_SLUG`, NOT multi-tenant-by-request routing).

**Scope (from the design doc).** This plan (6a) builds ONLY: crate scaffold, session auth, RBAC scaffolding, security/CORS/rate-limit/body-limit middleware, `POST /auth/portal-login` + `GET /auth/me` + `POST /auth/logout`, and `reactor-core`'s binary assembly (real `PollerShared`, account bootstrap, `ws-hub` mount with session validation). It does NOT build: spx-credentials CRUD, the OTP gate, sub-user CRUD (all 6b), bookings/rules/manual-accept routes (6c), prices/branding/locations/bot-settings (6d), or quick-accept HMAC (6e) — those get their own plans against this same design doc.

**Tenant resolution.** `reactor-core` resolves exactly ONE tenant at boot from a `TENANT_SLUG` env var, looks it up once via a new `store::tenants::find_by_slug`, and holds the resolved `tenant_id: Uuid` in `AppState`. No route resolves a tenant from request input. Tests that need a tenant use this project's established `insert_test_tenant`-style helper (see `store/src/lib.rs`'s test module) with a unique slug per test.

**RLS / non-superuser pool (Fase-2-flagged gap, closed here).** `reactor-core`'s production `PgPool` MUST authenticate as `app_role` (`NOLOGIN` today per Fase 2's migration — this task promotes it or creates an equivalent `LOGIN` role with its own password via a new migration), not `tower` (the dev bootstrap superuser, which `BYPASSRLS`es and makes every RLS policy in the schema a silent no-op). Every tenant-scoped query — in this plan and every later 6b-6e plan — MUST go through `store::begin_tenant_tx`. A raw `pool.begin()` against a tenant-scoped table under `app_role` returns ZERO rows, not an error — a silent, hard-to-notice bug class; do not "fix" a mysteriously-empty query result by switching back to a superuser connection.

**Real-service testing standard (unchanged since Fase 1).** Real Postgres (`postgres://tower:tower_dev_only@127.0.0.1:15432/tower` for migrations/superuser setup, but application-level tests exercise the `app_role` path per the RLS constraint above) and real Redis (`redis://127.0.0.1:16379`) — no mocks for internal state. `wiremock` for outbound SPX/sidecar HTTP. Route-level tests drive a REAL locally-bound `axum::serve` instance with a real HTTP client (this crate's convention per the design doc: middleware ordering must be genuinely exercised, not bypassed by calling handlers directly). Unique tenant/account ids per test (`format!("t{}", Uuid::new_v4().simple())`). Run DB/Redis-touching suites with `-- --test-threads=1`.

**Verification confidence.** `tower_governor` and `tower_http`'s `cors`/`limit` modules are NEW to this workspace (confirmed via research: zero existing usage anywhere). Their exact API shapes in the tasks below are best-effort from published docs, not read-from-installed-source the way Fase 5 verified `chromiumoxide`/`web-push-native` — **each task using them says so explicitly and instructs the implementer to verify against the actually-resolved version before proceeding**, matching this project's established practice for genuinely new dependencies.

**Reuse the REAL Fase 1-5 signatures (do NOT guess — read from source for this plan):**
- `spx_client::crypto::password::{hash_password(password: &str) -> Result<String, CryptoError>, verify_password(password: &str, phc_hash: &str) -> bool}` — Argon2id, OWASP params, constant-time verify.
- `spx_client::crypto::session_token::{generate_session_token() -> Result<(SecretString, [u8;32]), CryptoError>, hash_session_token(token: &str) -> [u8;32]}`.
- `store::pool::{connect(database_url: &str) -> Result<PgPool, sqlx::Error>, run_migrations(pool), begin_tenant_tx(pool, tenant_id: Uuid) -> Result<Transaction<'static, Postgres>, sqlx::Error>}`.
- `store::models::{PortalUser{id, tenant_id, username, password_hash, display_name, is_main_account, enabled, created_at, updated_at}, PortalSession{id, tenant_id, portal_user_id, token_hash: Vec<u8>, ip: Option<String>, user_agent: Option<String>, created_at, expires_at, last_seen_at}}`.
- `executor::ExecutorHandle::connect(redis_url: &str) -> Result<Self, ExecutorError>`; `spx_client::SpxClient::new(base_url: impl Into<String>) -> Result<Self, SpxError>`; `poller::login::SidecarClient::new(base_url: impl Into<String>) -> Self`; `poller::publish::RedisPublisher::connect(redis_url: &str) -> Result<Self, redis::RedisError>`.
- `poller::state::{PollerShared{executor: Arc<ExecutorHandle>, client: Arc<SpxClient>, pool: store::PgPool, config: PollerConfig, accounts: Arc<DashMap<String,AccountHandle>>, sidecar: Arc<SidecarClient>, notifier: Option<Arc<notifier::BotSettings>>, redis: Option<RedisPublisher>}, PollerState::new(account_id: String, tenant_id: Uuid, agency_id: i64, cookies: SpxCookies, username: SecretString, password: SecretString) -> Self}` — a freshly-`new()`'d `PollerState` has EMPTY `rules`/`rule_meta` (`Arc::new(Vec::new())`); the caller must populate both after construction.
- `poller::schedule::ensure_restored_then_spawn(shared: Arc<PollerShared>, st: PollerState) -> AccountHandle` — the ONLY production-safe spawn entrypoint (awaits `restore_accepted_ids` first).
- `core_domain::matching::CompiledRule::compile(&AcceptRule) -> CompiledRule` — the established pattern for turning a DB `accept_rules` row into a dispatchable rule (see `poller/tests/dispatch_pipeline.rs` for the exact `CompiledRule::compile(...)` + parallel `RuleMeta{uuid, cap, accepted_count, name}` construction this plan's Task 9 must mirror).
- `ws_hub::{Hub::new() -> Arc<Hub>, ws_router(hub: Arc<Hub>) -> Router, ws_handler, WsQuery{session: String, account: String}}` — `ws_handler` today performs NO session validation; Task 10 adds an outer check.

**Workflow.** Run all `cargo` commands from `Backend/`. `export PATH="$HOME/.cargo/bin:$PATH"` if `cargo` is not found. Bring up services with `cd Docker && docker compose up -d tower-postgres tower-redis`. Commit only when a task's steps say to.

---

### Task 1: `api-gateway` crate scaffold + `AppState` + `ApiError` + empty router mounted into `reactor-core`

**Files:**
- Modify: `Backend/crates/api-gateway/Cargo.toml`
- Create: `Backend/crates/api-gateway/src/state.rs`
- Create: `Backend/crates/api-gateway/src/error.rs`
- Overwrite: `Backend/crates/api-gateway/src/lib.rs`
- Modify: `Backend/bin/reactor-core/Cargo.toml`, `Backend/bin/reactor-core/src/main.rs`

**Interfaces produced:**
- `pub struct AppState { pub poller: Arc<poller::PollerShared>, pub ws_hub: Arc<ws_hub::Hub>, pub tenant_id: Uuid, pub cors_origins: Arc<Vec<String>> }` (Clone; `poller`/`ws_hub` are already `Arc`-friendly, so `#[derive(Clone)]` works directly).
- `pub enum ApiError { Unauthorized, Forbidden, NotFound, BadRequest(String), Internal(String) }` implementing `axum::response::IntoResponse` → a consistent `{"error": "<message>"}` JSON body with the matching HTTP status (401/403/404/400/500).
- `pub fn build_router(state: AppState) -> axum::Router` — for this task, just `Router::new().route("/healthz", get(healthz)).with_state(state)` (proves the crate wires into `reactor-core` before any real route exists).

- [ ] **Step 1: Add `api-gateway`'s dependencies**

```bash
cd Backend
cargo add --package api-gateway axum --features ws
cargo add --package api-gateway tokio --features rt-multi-thread,macros
cargo add --package api-gateway serde --features derive
cargo add --package api-gateway serde_json
cargo add --package api-gateway uuid --features v4,serde
cargo add --package api-gateway chrono --features serde
cargo add --package api-gateway tracing
cargo add --package api-gateway --path crates/store store
cargo add --package api-gateway --path crates/executor executor
cargo add --package api-gateway --path crates/spx-client spx-client
cargo add --package api-gateway --path crates/poller poller
cargo add --package api-gateway --path crates/ws-hub ws-hub
cargo add --package api-gateway --path crates/notifier notifier
cargo add --package api-gateway --path crates/core-domain core-domain
cd ..
```

Match `axum`/`tokio`/`serde`/`uuid`/`chrono` to the SAME versions already resolved elsewhere in the workspace (check `Backend/bin/reactor-core/Cargo.toml` and `Backend/crates/poller/Cargo.toml` for the pinned versions already in `Cargo.lock`; if `cargo add` resolves a different version, pin explicitly to match — a duplicate `axum`/`tokio` major version across workspace crates is a real problem, not a warning to ignore).

- [ ] **Step 2: Write `error.rs`**

```rust
// Backend/crates/api-gateway/src/error.rs
//! Unified API error → consistent `{"error": "..."}` JSON + status code.
//! Every handler in this crate returns `Result<T, ApiError>`.
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden,
    NotFound,
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Internal(m) => {
                tracing::error!(error = %m, "internal api-gateway error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}
```

> `Internal`'s message is logged via `tracing::error!` but the HTTP response body is always the generic `"internal error"` string — never leak a raw `sqlx::Error`/DB detail to the client. `From<sqlx::Error>` lets handlers use `?` directly on store calls.

- [ ] **Step 2b: Run to verify it compiles standalone**

```bash
cd Backend && cargo build -p api-gateway 2>&1 | tail -20 && cd ..
```

Expected: fails only on the not-yet-written `state.rs`/`lib.rs` (or passes if you write them in whichever order — this checkpoint just confirms `error.rs`'s own syntax before moving on).

- [ ] **Step 3: Write `state.rs`**

```rust
// Backend/crates/api-gateway/src/state.rs
//! Shared HTTP-layer context. Wraps `poller::PollerShared` (the SAME
//! executor/client/pool/etc. account tasks use) rather than duplicating its
//! fields — `AppState` adds only what the HTTP layer needs on top: the
//! ws-hub registry and the resolved single deployment tenant (see the design
//! doc's tenant-resolution addendum — no per-request tenant resolution).
use std::sync::Arc;

use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub poller: Arc<poller::PollerShared>,
    pub ws_hub: Arc<ws_hub::Hub>,
    pub tenant_id: Uuid,
    /// Exact-match CORS allowlist (Task 7) — `Arc` so cloning `AppState` per
    /// request stays cheap.
    pub cors_origins: Arc<Vec<String>>,
    /// Session cookie name, configurable so a later fase/deployment can
    /// rename it without touching handler code.
    pub session_cookie_name: Arc<str>,
}
```

- [ ] **Step 4: Write `lib.rs`**

```rust
// Backend/crates/api-gateway/src/lib.rs
//! Fase 6 — api-gateway: the REST + WebSocket HTTP layer over Fases 1-5.
//! Session auth + centralized RBAC + security/CORS/rate-limit/body-limit
//! middleware. This sub-phase (6a) ships only the foundation: crate
//! scaffold, session/RBAC plumbing, login/me/logout, and the middleware
//! stack. Later sub-phases (6b-6e) add route modules here.
pub mod error;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}
```

- [ ] **Step 5: Mount into `reactor-core` (still `/healthz`-only — proves the wiring, nothing else changes yet)**

Modify `Backend/bin/reactor-core/Cargo.toml`: add `api-gateway = { path = "../../crates/api-gateway" }`, `poller = { path = "../../crates/poller" }`, `ws-hub = { path = "../../crates/ws-hub" }` (match versions to the workspace).

Modify `Backend/bin/reactor-core/src/main.rs`'s `app()`/`main()` MINIMALLY for this task — replace the bare `Router::new().route("/healthz", ...)` with a call into `api_gateway::build_router(state)`, where `state` is a placeholder `AppState` constructed with whatever's cheapest to stand up correctly (do NOT build the real `PollerShared`/account-bootstrap logic yet — that is Task 9's job; for THIS task, it's fine for `main()` to construct a minimal `AppState` with a freshly-connected (but otherwise idle) `PollerShared` so the binary boots and `/healthz` still returns 200). Keep the existing `#[cfg(test)] mod tests { ... healthz_returns_ok_status ... }` test passing (adjust its `app()` call site to match if the function signature changed).

- [ ] **Step 6: Test, clippy, commit**

```bash
cd Backend
cargo build --workspace
cargo test -p api-gateway -p reactor-core
cargo clippy -p api-gateway -p reactor-core --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway Backend/bin/reactor-core Backend/Cargo.lock
git commit -m "feat(api-gateway): crate scaffold + AppState + ApiError, mounted into reactor-core"
```

---

### Task 2: `store` query functions — tenants, portal_users, portal_sessions

**Files:**
- Create: `Backend/crates/store/src/tenants.rs`
- Create: `Backend/crates/store/src/portal_users.rs`
- Create: `Backend/crates/store/src/portal_sessions.rs`
- Modify: `Backend/crates/store/src/lib.rs` (add the 3 new `pub mod`s + re-exports)

**Interfaces produced:**
- `pub async fn tenants::find_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Tenant>, sqlx::Error>` — NOTE: this is the ONE query in this crate that legitimately runs OUTSIDE `begin_tenant_tx` (tenant resolution happens BEFORE a tenant_id is known — chicken-and-egg). `tenants` itself has no RLS policy scoped to `app.tenant_id` (verify this against `migrations/0016_rls_policies.sql`'s table list before assuming — if `tenants` IS in that policy list, this function cannot work as a bare `pool` query under `app_role` and must be reconsidered, e.g. a dedicated non-RLS'd lookup path).
- `pub async fn portal_users::find_by_username(pool: &PgPool, tenant_id: Uuid, username: &str) -> Result<Option<PortalUser>, sqlx::Error>`.
- `pub async fn portal_sessions::create(pool: &PgPool, tenant_id: Uuid, portal_user_id: Uuid, token_hash: [u8;32], ip: Option<&str>, user_agent: Option<&str>, ttl: chrono::Duration) -> Result<PortalSession, sqlx::Error>`.
- `pub async fn portal_sessions::find_valid_by_hash(pool: &PgPool, token_hash: [u8;32]) -> Result<Option<PortalSession>, sqlx::Error>` — filters `expires_at > now()`; does NOT filter by tenant (the hash is globally unique per the schema's `UNIQUE(token_hash)` constraint, so tenant is read back FROM the found row, not used to find it).
- `pub async fn portal_sessions::delete(pool: &PgPool, tenant_id: Uuid, session_id: Uuid) -> Result<(), sqlx::Error>` (logout).
- `pub async fn portal_sessions::touch_last_seen(pool: &PgPool, tenant_id: Uuid, session_id: Uuid) -> Result<(), sqlx::Error>`.

- [ ] **Step 1: Check `tenants`' RLS status before writing `find_by_slug`**

```bash
grep -n "tenants" Backend/crates/store/migrations/0016_rls_policies.sql
```

If `tenants` appears in that migration's `ENABLE ROW LEVEL SECURITY`/`FORCE`/policy list, `find_by_slug` querying via a bare `app_role` connection (no `app.tenant_id` set — there IS no tenant_id yet at this point) will see zero rows, and this task must instead add a NARROW exception (e.g. a policy allowing `SELECT` on `tenants` for any authenticated `app_role` connection regardless of `app.tenant_id`, since discovering "which tenant am I" is inherently pre-tenant-scoping) — write it as a new migration `Backend/crates/store/migrations/00XX_tenants_lookup_policy.sql` if needed, with a comment explaining exactly why (mirroring this project's established migration-comment rigor). If `tenants` is NOT in the RLS list, no migration is needed — proceed directly to Step 2.

- [ ] **Step 2: Write `tenants.rs`**

```rust
// Backend/crates/store/src/tenants.rs
//! Deployment-tenant resolution. `find_by_slug` is the ONE query in this
//! crate that legitimately runs outside `begin_tenant_tx` — tenant
//! resolution happens BEFORE any tenant_id is known (see Task 1's RLS check:
//! `tenants` itself is intentionally excluded from / carved out of the
//! per-tenant RLS policy for exactly this reason).
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

pub async fn find_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Tenant>, sqlx::Error> {
    sqlx::query_as::<_, Tenant>("SELECT id, name, slug FROM tenants WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await
}
```

- [ ] **Step 3: Write `portal_users.rs`**

```rust
// Backend/crates/store/src/portal_users.rs
//! Portal-user lookups. Tenant-scoped — every query here runs inside
//! `begin_tenant_tx` (the tenant is already known by the time login/session
//! code calls into this module; `tenants::find_by_slug` resolves it first).
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::PortalUser;

pub async fn find_by_username(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
) -> Result<Option<PortalUser>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, PortalUser>(
        "SELECT id, tenant_id, username, password_hash, display_name, is_main_account, \
         enabled, created_at, updated_at FROM portal_users \
         WHERE tenant_id = $1 AND username = $2",
    )
    .bind(tenant_id)
    .bind(username)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}
```

> `PortalUser` must already derive `sqlx::FromRow` (it does — `Backend/crates/store/src/models/portal_user.rs`, unchanged by this task). Re-verify the column list/order above against that file before finalizing the query (this snippet was written from the model's known fields, but confirm no drift).

- [ ] **Step 4: Write `portal_sessions.rs`**

```rust
// Backend/crates/store/src/portal_sessions.rs
//! Opaque session issuance/lookup/revocation. `token_hash` is always the
//! SHA-256 of the plaintext cookie token (`spx_client::crypto::session_token`)
//! — this crate never sees or stores a plaintext token.
use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::PortalSession;

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    portal_user_id: Uuid,
    token_hash: [u8; 32],
    ip: Option<&str>,
    user_agent: Option<&str>,
    ttl: Duration,
) -> Result<PortalSession, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let expires_at = Utc::now() + ttl;
    let row = sqlx::query_as::<_, PortalSession>(
        "INSERT INTO portal_sessions \
         (tenant_id, portal_user_id, token_hash, ip, user_agent, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, tenant_id, portal_user_id, token_hash, ip, user_agent, \
                   created_at, expires_at, last_seen_at",
    )
    .bind(tenant_id)
    .bind(portal_user_id)
    .bind(token_hash.as_slice())
    .bind(ip)
    .bind(user_agent)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Looks up by hash alone (globally unique per the schema) — tenant is read
/// FROM the result, not used to find it (the caller doesn't know the tenant
/// yet at this point in the auth-middleware flow).
pub async fn find_valid_by_hash(
    pool: &PgPool,
    token_hash: [u8; 32],
) -> Result<Option<PortalSession>, sqlx::Error> {
    sqlx::query_as::<_, PortalSession>(
        "SELECT id, tenant_id, portal_user_id, token_hash, ip, user_agent, \
                created_at, expires_at, last_seen_at \
         FROM portal_sessions WHERE token_hash = $1 AND expires_at > now()",
    )
    .bind(token_hash.as_slice())
    .fetch_optional(pool)
    .await
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, session_id: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query("DELETE FROM portal_sessions WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn touch_last_seen(
    pool: &PgPool,
    tenant_id: Uuid,
    session_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query("UPDATE portal_sessions SET last_seen_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
```

> `find_valid_by_hash` intentionally runs OUTSIDE `begin_tenant_tx` (same chicken-and-egg reasoning as `tenants::find_by_slug` — the whole point of this query is to discover which tenant/user a bearer cookie belongs to). Check whether `portal_sessions` needs the same RLS carve-out investigated in Step 1 for `tenants` — if `portal_sessions` IS RLS-protected and a bare `app_role` connection can't read it without `app.tenant_id` set, this function needs the same kind of narrow policy exception, OR (preferred if it keeps RLS simpler) restructure so the session middleware first extracts a tenant hint some other way. Investigate and document whichever resolution you land on in your task report — do not silently work around an RLS zero-rows result by connecting as a superuser.

- [ ] **Step 5: Wire `lib.rs`**

Add `pub mod tenants; pub mod portal_users; pub mod portal_sessions;` and re-export their public functions/types alongside the existing `pub use bookings::{...}` block, following that established style exactly.

- [ ] **Step 6: Tests — real Postgres, `app_role`**

New test file `Backend/crates/store/tests/auth_queries.rs` (or add to `store`'s existing `#[cfg(test)]` module in `lib.rs` if that's this crate's established location for its own tests — check first). Follow the exact `insert_test_tenant`/`SET ROLE app_role`-then-`begin_tenant_tx` pattern already established in `store/src/lib.rs`'s test module (read it before writing these). Cover: `find_by_slug` finds a seeded tenant and returns `None` for an unknown slug; `find_by_username` finds a seeded user and returns `None` cross-tenant (seed the SAME username under two different tenants, confirm each only finds its own — proves tenant isolation, not just "the query runs"); `create`+`find_valid_by_hash` round-trip and an EXPIRED session (insert with `ttl = Duration::seconds(-1)` or a raw manual `expires_at` in the past) is NOT found; `delete` then `find_valid_by_hash` returns `None`.

- [ ] **Step 7: Test, clippy, commit**

```bash
cd Backend
cargo test -p store -- --test-threads=1
cargo clippy -p store --all-targets -- -D warnings
cd ..
git add Backend/crates/store
git commit -m "feat(store): tenant/portal_user/portal_session query functions (Fase 6a foundation)"
```

---

### Task 3: Session-auth middleware + `CurrentUser` extractor

**Files:**
- Create: `Backend/crates/api-gateway/src/auth/mod.rs`
- Create: `Backend/crates/api-gateway/src/auth/middleware.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs` (add `pub mod auth;`)

**Interfaces produced:**
- `pub struct CurrentUser { pub session_id: Uuid, pub tenant_id: Uuid, pub portal_user_id: Uuid, pub username: String, pub display_name: String, pub is_main_account: bool }` — inserted as a request extension by the middleware, retrievable in handlers via `axum::extract::Extension<CurrentUser>` or a small custom `FromRequestParts` impl (implementer's choice — document which). `session_id` is carried specifically so Task 5's `logout` handler can delete the EXACT session row (a user may have several concurrent sessions across devices — logout must not touch any session but the one presented).
- `pub async fn session_auth(State(state): State<AppState>, jar: CookieJar, mut req: Request, next: Next) -> Result<Response, ApiError>` — an axum middleware function: extract the session cookie (name from `state.session_cookie_name`), hash it, `store::portal_sessions::find_valid_by_hash`, `store::portal_users::find_by_username`-equivalent lookup by id to build `CurrentUser`, insert into `req.extensions_mut()`, call `next.run(req)`. Missing/invalid/expired cookie → `ApiError::Unauthorized` (401), short-circuiting before the handler runs.

- [ ] **Step 1: Add `axum-extra` for the typed cookie jar**

```bash
cd Backend && cargo add --package api-gateway axum-extra --features cookie && cd ..
```

Verify the resolved `axum-extra` version is compatible with the workspace's pinned `axum 0.8.9` (check its own `Cargo.toml`/docs.rs for the matching major version — `axum-extra` versions track `axum` majors closely; if `cargo add` pulls something that doesn't compile against `axum 0.8`, pin explicitly).

- [ ] **Step 2: Write `middleware.rs`**

```rust
// Backend/crates/api-gateway/src/auth/middleware.rs
//! Session-cookie auth middleware. Runs before every route it's applied to
//! (mounted per-router-group in Task 5's login/me/logout wiring and by every
//! later sub-phase's protected routes) — NOT applied to `/healthz` or the
//! Task 6e quick-accept routes (those are explicitly session-free, per the
//! design doc).
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::session_token::hash_session_token;

#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub portal_user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_main_account: bool,
}

pub async fn session_auth(
    State(state): State<AppState>,
    jar: CookieJar,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = jar
        .get(&state.session_cookie_name)
        .map(|c| c.value().to_string())
        .ok_or(ApiError::Unauthorized)?;
    let hash = hash_session_token(&token);

    let session = store::portal_sessions::find_valid_by_hash(&state.poller.pool, hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::Unauthorized)?;

    // Look up the user WITHIN the session's own tenant (not `state.tenant_id`
    // — defense in depth in case a future multi-tenant change reintroduces
    // per-request tenant variance; today they're always equal since only one
    // tenant exists, but the session row is the source of truth here).
    let user = store::portal_users::find_by_id(&state.poller.pool, session.tenant_id, session.portal_user_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::Unauthorized)?;
    if !user.enabled {
        return Err(ApiError::Unauthorized);
    }

    let _ = store::portal_sessions::touch_last_seen(&state.poller.pool, session.tenant_id, session.id).await;

    req.extensions_mut().insert(CurrentUser {
        session_id: session.id,
        tenant_id: session.tenant_id,
        portal_user_id: user.id,
        username: user.username,
        display_name: user.display_name,
        is_main_account: user.is_main_account,
    });

    Ok(next.run(req).await)
}
```

> This references `store::portal_users::find_by_id` — NOT built in Task 2 (which only wrote `find_by_username`). Add it to `store/src/portal_users.rs` now as part of THIS task (same tenant-scoped pattern as `find_by_username`, just `WHERE tenant_id = $1 AND id = $2`), and re-export it from `store::lib.rs`. Do not skip this — the middleware needs a by-id lookup since the session only carries `portal_user_id`, not a username.

- [ ] **Step 3: Write `auth/mod.rs`**

```rust
// Backend/crates/api-gateway/src/auth/mod.rs
pub mod middleware;

pub use middleware::{session_auth, CurrentUser};
```

- [ ] **Step 4: Wire into `lib.rs`**

Add `pub mod auth;`. Do NOT yet apply `session_auth` to any route (Task 5's login/me/logout wiring is where it first gets mounted, since `/auth/portal-login` itself must be UNPROTECTED — you can't require a session to obtain one).

- [ ] **Step 5: Test — real Postgres, a route-level test server**

New file `Backend/crates/api-gateway/tests/session_auth.rs`. Stand up a real `axum::serve` instance with a tiny test router: one route WITHOUT the middleware (control) and one WITH `axum::middleware::from_fn_with_state(state.clone(), session_auth)` applied, both just echoing 200 if reached. Seed a real Postgres tenant+user+session (reuse Task 2's test-seeding pattern). Cases: (1) no cookie → protected route 401, unprotected route 200; (2) a valid, unexpired session cookie → protected route 200, and the handler can read `CurrentUser` via an extension extractor and see the right `username`; (3) an expired session (seed one with a past `expires_at`) → 401; (4) a well-formed but nonexistent token (never stored) → 401, not a 500 (hashing a bogus token and finding no row must be a clean "not found", never an error path).

- [ ] **Step 6: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -p store -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway Backend/crates/store
git commit -m "feat(api-gateway): session-cookie auth middleware + CurrentUser extension"
```

---

### Task 4: `Permission` enum + `require_permission`

**Files:**
- Create: `Backend/crates/api-gateway/src/auth/permission.rs`
- Modify: `Backend/crates/api-gateway/src/auth/mod.rs`

**Interfaces produced:**
- `pub enum Permission { ManageSubUsers, ManageSpxCredentials, ManageBotSettings, ArmAutoAccept, ManagePrices, ManageBranding, ManageLocations, ManageRules }` (per the design doc's Global Constraints — one flat enum, every variant currently gated identically on `is_main_account`; do not build a database-backed ACL).
- `pub fn require_permission(user: &CurrentUser, perm: Permission) -> Result<(), ApiError>` — today: `if user.is_main_account { Ok(()) } else { Err(ApiError::Forbidden) }` for every variant (a `match` over all variants doing the same thing IS the point — it's the one place a future finer-grained rule would change, not premature abstraction to collapse it to a bare boolean check; keep the `match` so the enum's variants are genuinely load-bearing as documentation of every gated action, even though their bodies are identical today).

- [ ] **Step 1: Write `permission.rs`**

```rust
// Backend/crates/api-gateway/src/auth/permission.rs
//! Centralized permission gate (master spec: "RBAC require_permission
//! terpusat"). The reference scatters ad hoc `if (!session.isMainAccount)`
//! checks across every route file; this enum is the one place that logic
//! lives instead. Every variant is uniformly main-account-gated today (the
//! reference has no finer-grained permission table) — the payoff is that a
//! future finer-grained rule changes ONE function, not N call sites.
use crate::auth::CurrentUser;
use crate::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ManageSubUsers,
    ManageSpxCredentials,
    ManageBotSettings,
    ArmAutoAccept,
    ManagePrices,
    ManageBranding,
    ManageLocations,
    ManageRules,
}

pub fn require_permission(user: &CurrentUser, perm: Permission) -> Result<(), ApiError> {
    let allowed = match perm {
        Permission::ManageSubUsers
        | Permission::ManageSpxCredentials
        | Permission::ManageBotSettings
        | Permission::ArmAutoAccept
        | Permission::ManagePrices
        | Permission::ManageBranding
        | Permission::ManageLocations
        | Permission::ManageRules => user.is_main_account,
    };
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}
```

- [ ] **Step 2: Wire into `auth/mod.rs`**

Add `pub mod permission; pub use permission::{require_permission, Permission};`.

- [ ] **Step 3: Unit tests**

In `permission.rs`'s own `#[cfg(test)] mod tests`, no DB/Redis needed — construct a `CurrentUser` literal directly. Assert: every `Permission` variant is `Ok(())` for `is_main_account: true` and `Err(ApiError::Forbidden)` for `is_main_account: false` (loop over an explicit `[Permission::ManageSubUsers, ...]` array covering all 8 variants — if a 9th variant is ever added and this array isn't updated, that's a future maintainer's problem to notice via a failing assert count, not silently skipped; do not use a wildcard `_ =>` in the test array construction).

- [ ] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): Permission enum + require_permission (centralized RBAC)"
```

---

### Task 5: `POST /auth/portal-login`, `GET /auth/me`, `POST /auth/logout`

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/mod.rs`
- Create: `Backend/crates/api-gateway/src/routes/auth.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs`

**Interfaces produced:**
- `pub fn auth_router(state: AppState) -> Router` — `/auth/portal-login` (POST, NO session_auth layer), `/auth/me` (GET) + `/auth/logout` (POST) (both WITH `session_auth` applied via `.route_layer(...)` on a nested sub-router, so only these two require a session while `/auth/portal-login` doesn't).
- `pub struct LoginRequest { pub username: String, pub password: String }`, `pub struct LoginResponse { pub username: String, pub display_name: String, pub is_main_account: bool }` (the session cookie is set via a `Set-Cookie` header, NOT returned in the JSON body — mirrors the reference's opaque-cookie pattern, never put the token in a response body field a JS logger could accidentally capture).

- [ ] **Step 1: Write `routes/auth.rs`**

```rust
// Backend/crates/api-gateway/src/routes/auth.rs
//! POST /auth/portal-login, GET /auth/me, POST /auth/logout.
use axum::extract::{Extension, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use chrono::Duration;
use serde::{Deserialize, Serialize};

use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::password::verify_password;
use spx_client::crypto::session_token::generate_session_token;

const SESSION_TTL: Duration = Duration::hours(12);

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub username: String,
    pub display_name: String,
    pub is_main_account: bool,
}

async fn portal_login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> Result<(CookieJar, Json<MeResponse>), ApiError> {
    let user = store::portal_users::find_by_username(&state.poller.pool, state.tenant_id, &body.username)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if !user.enabled || !verify_password(&body.password, &user.password_hash) {
        return Err(ApiError::Unauthorized);
    }

    let (token, hash) = generate_session_token().map_err(|e| ApiError::Internal(format!("{e:?}")))?;
    store::portal_sessions::create(&state.poller.pool, state.tenant_id, user.id, hash, None, None, SESSION_TTL)
        .await?;

    let cookie = Cookie::build((state.session_cookie_name.to_string(), {
        use spx_client::crypto::secret::ExposeSecret;
        token.expose_secret().to_string()
    }))
    .http_only(true)
    .secure(true)
    .same_site(SameSite::Strict)
    .path("/")
    .build();

    Ok((
        jar.add(cookie),
        Json(MeResponse {
            username: user.username,
            display_name: user.display_name,
            is_main_account: user.is_main_account,
        }),
    ))
}

async fn me(Extension(user): Extension<CurrentUser>) -> Json<MeResponse> {
    Json(MeResponse {
        username: user.username,
        display_name: user.display_name,
        is_main_account: user.is_main_account,
    })
}

async fn logout(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    jar: CookieJar,
) -> Result<CookieJar, ApiError> {
    // Delete the EXACT session row `session_id` names — a user may have
    // several concurrent sessions across devices; logout must not touch any
    // session but the one presented (never "delete all of this user's
    // sessions").
    store::portal_sessions::delete(&state.poller.pool, user.tenant_id, user.session_id).await?;
    Ok(jar.remove(state.session_cookie_name.to_string()))
}

pub fn auth_router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/me", get(me))
        .route("/logout", post(logout))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), session_auth));

    Router::new()
        .route("/portal-login", post(portal_login))
        .merge(protected)
}
```

> Verify `axum_extra`'s exact `Cookie`/`CookieJar`/`SameSite` API against the resolved version (best-effort above) before finalizing — the builder pattern (`Cookie::build(...).http_only(true)...build()`) may differ slightly by version.
>
> `secure(true)` requires HTTPS in production; this project's Docker Compose setup terminates TLS at the Caddy/Traefik edge per the master spec's architecture, so `reactor-core` itself sees plain HTTP internally — confirm whether `secure(true)` breaks local dev (where the edge might not be HTTPS) and if so, gate it behind an env var/config flag (`COOKIE_SECURE`, default `true`, override for local dev only) rather than hardcoding `false`.

- [ ] **Step 2: Wire into `lib.rs`**

```rust
pub mod auth;
pub mod error;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use state::AppState;

use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/auth", routes::auth::auth_router(state.clone()))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "api-gateway" }))
}
```

(`routes/mod.rs` is just `pub mod auth;`.)

- [ ] **Step 3: Route-level tests**

New file `Backend/crates/api-gateway/tests/auth_routes.rs`. Real `axum::serve` + real HTTP client (this crate's established convention). Seed a real Postgres user with a KNOWN password (via `spx_client::crypto::password::hash_password` at seed time). Cases: (1) `POST /auth/portal-login` with correct credentials → 200, `Set-Cookie` header present, body has the right `username`/`is_main_account`; (2) wrong password → 401, no `Set-Cookie`; (3) unknown username → 401 (same response shape as wrong password — do NOT let the API distinguish "user doesn't exist" from "wrong password" in status/body, that's a username-enumeration leak); (4) `GET /auth/me` with the cookie from case 1 → 200 with matching user data; without any cookie → 401; (5) `POST /auth/logout` then a subsequent `GET /auth/me` with the SAME (now-deleted) cookie → 401 (proves server-side session deletion actually happened, not just a client-side cookie clear).

- [ ] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): POST /auth/portal-login, GET /auth/me, POST /auth/logout"
```

---

### Task 6: Security headers (incl. real CSP)

**Files:**
- Create: `Backend/crates/api-gateway/src/middleware/mod.rs`
- Create: `Backend/crates/api-gateway/src/middleware/security_headers.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs`

**Interfaces produced:**
- `pub fn security_headers_layer() -> impl tower::Layer<...> + Clone` (or a hand-rolled `axum::middleware::from_fn` — implementer's choice, pick whichever composes more simply with the rest of this crate's layers in Task 5-8) that sets, on every response: `Strict-Transport-Security`, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `X-XSS-Protection: 0` (modern guidance: explicitly disable the legacy header's filter, don't omit it — verify this is still the right value, not the reference's possibly-stale `1; mode=block`), `Referrer-Policy: strict-origin-when-cross-origin`, `Permissions-Policy` (a reasonable restrictive default — e.g. `geolocation=(), camera=(), microphone=()`), and a REAL `Content-Security-Policy` header (the reference has none — this is a genuine improvement per the design doc's correction #1, not parity).

- [ ] **Step 1: Design the CSP value**

TOWER's frontend (Fase 7, not yet built) is a SvelteKit app served separately (per the master spec's architecture, `tower-web` is its own container, `reactor-core` only serves the API + WS). So `api-gateway`'s CSP is for ITS OWN JSON/HTML responses (mainly the quick-accept HTML confirmation pages Fase 6e will add) — a strict default is appropriate: `default-src 'none'; frame-ancestors 'none'; base-uri 'none'`. Do NOT try to author a CSP permissive enough for the SvelteKit app's own assets — that's Fase 7's/the edge proxy's concern for ITS responses, not `reactor-core`'s API responses.

- [ ] **Step 2: Write `security_headers.rs`**

```rust
// Backend/crates/api-gateway/src/middleware/security_headers.rs
//! Fixed security headers on every api-gateway response, incl. a real CSP
//! (the reference has none — master spec explicitly requires it; see design
//! doc correction #1).
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("Strict-Transport-Security", HeaderValue::from_static("max-age=31536000; includeSubDomains"));
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    h.insert("X-XSS-Protection", HeaderValue::from_static("0"));
    h.insert("Referrer-Policy", HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.insert("Permissions-Policy", HeaderValue::from_static("geolocation=(), camera=(), microphone=()"));
    h.insert(
        "Content-Security-Policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; base-uri 'none'"),
    );
    res
}
```

- [ ] **Step 3: Wire `middleware/mod.rs`, apply globally in `lib.rs`**

`middleware/mod.rs`: `pub mod security_headers; pub use security_headers::security_headers;`. In `lib.rs`'s `build_router`, apply via `.layer(axum::middleware::from_fn(security_headers))` on the OUTERMOST layer (so it runs on every response including error responses from `ApiError`, and including `/healthz`).

- [ ] **Step 4: Test**

Route-level test: any request (even a 401/404) has all 7 headers present with the exact values above. Assert this on at least 2 different routes (e.g. `/healthz` and a deliberately-401'd `/auth/me`) to prove it's genuinely global, not attached to one handler.

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): security headers incl. real CSP (reference has none)"
```

---

### Task 7: CORS allowlist + body-limit

**Files:**
- Create: `Backend/crates/api-gateway/src/middleware/cors.rs`
- Modify: `Backend/crates/api-gateway/src/middleware/mod.rs`, `Backend/crates/api-gateway/src/lib.rs`, `Backend/crates/api-gateway/src/state.rs`

**Interfaces produced:**
- `pub fn cors_layer(origins: &[String]) -> tower_http::cors::CorsLayer` — EXACT-match allowlist (`HeaderValue`s built from the configured origin strings), credentials allowed (cookies cross-origin need `Access-Control-Allow-Credentials: true` + a non-wildcard origin — `tower_http`'s `CorsLayer` refuses `Any` + credentials together, which is the correct safety rail, not a bug to work around), `GET/POST/PUT/DELETE`, common headers incl. `Content-Type`.
- Body-limit: `tower_http::limit::RequestBodyLimitLayer::new(1_500_000)` applied GLOBALLY (1.5MB default, matching the reference), with NO override in this task (the 15MB branding carve-out is Task 8 of the 6d plan, since the branding route doesn't exist until then — do not build dead code for a route that isn't mounted yet).

- [ ] **Step 1: Add `tower-http`**

```bash
cd Backend && cargo add --package api-gateway tower-http --features cors,limit && cd ..
```

**Verify against the actually-resolved version** (not read from installed source ahead of time for this plan — genuinely new to this workspace): `cargo doc -p tower-http --open` or check docs.rs for the resolved version's `cors::CorsLayer`/`limit::RequestBodyLimitLayer` exact builder API before writing Step 2 — the snippet below is best-effort from published API shape, confirm method names match.

- [ ] **Step 2: Write `cors.rs`**

```rust
// Backend/crates/api-gateway/src/middleware/cors.rs
//! Exact-match CORS allowlist — NO wildcard origin. The reference's own code
//! comments flag a near-miss ngrok-wildcard CSRF finding; this project does
//! not repeat it. `cargo_origins` come from `AppState.cors_origins`
//! (populated at boot from an env var, Task 9).
use axum::http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;

pub fn cors_layer(origins: &[String]) -> CorsLayer {
    let allowed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(allowed)
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([axum::http::header::CONTENT_TYPE])
}
```

> Verify `allow_origin` accepts a `Vec<HeaderValue>` directly in the resolved `tower_http` version (some versions want `AllowOrigin::list(...)` explicitly) — adjust to match. `filter_map` silently drops a malformed configured origin string rather than panicking at boot on a typo'd env var; log a `tracing::warn!` for each dropped one so a misconfiguration is visible in the logs even though it doesn't crash the process.

- [ ] **Step 3: Add `cors_origins` config to boot + wire both layers into `lib.rs`**

For THIS task, read allowed origins from a `CORS_ALLOWED_ORIGINS` env var (comma-separated) wherever `AppState` gets constructed today (Task 1's placeholder construction in `reactor-core`'s `main()` — Task 9 will replace that whole construction site properly, but wire the env var reading in NOW so Task 9 doesn't have to revisit this). Apply both layers in `build_router`: `.layer(cors_layer(&state.cors_origins)).layer(RequestBodyLimitLayer::new(1_500_000))`.

- [ ] **Step 4: Test**

Route-level: a request with `Origin: https://allowed.example` (configured in the test's `AppState`) gets `Access-Control-Allow-Origin` echoing that exact origin + `Access-Control-Allow-Credentials: true`; a request with an UNLISTED `Origin` header gets NO `Access-Control-Allow-Origin` header (proving rejection, not a permissive fallback). A request with a body over 1.5MB gets `413 Payload Too Large` (construct a real oversized body in the test, don't just check the layer is present).

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): CORS exact-match allowlist + 1.5MB body-limit"
```

---

### Task 8: Rate limiting (`tower_governor`)

**Files:**
- Create: `Backend/crates/api-gateway/src/middleware/rate_limit.rs`
- Modify: `Backend/crates/api-gateway/src/middleware/mod.rs`, `Backend/crates/api-gateway/src/lib.rs`

**Interfaces produced:**
- `pub fn public_rate_limit_layer() -> GovernorLayer<...>` — 120 requests/minute per IP, applied to whichever routes are genuinely unauthenticated-public in THIS sub-phase (today: none yet — 6d's `prices`/`branding` GETs are the reference's actual public-rate-limited routes, not built until 6d). For 6a, apply this layer to `/auth/portal-login` specifically instead (a login endpoint is exactly the kind of route brute-force/credential-stuffing attempts target, and it's the one public-ish route this sub-phase actually ships) — a stricter budget than 120/min is appropriate here; use 20/min/IP for login attempts specifically, disclosed as a deliberate divergence from the reference's undifferentiated public-GET limiter (login POST attempts are a different threat model than public GET reads).

- [ ] **Step 1: Add `tower_governor`**

```bash
cd Backend && cargo add --package api-gateway tower_governor && cd ..
```

**Verify against the actually-resolved version.** `tower_governor` is genuinely new to this workspace and Rust's rate-limiter-middleware crate landscape has real API churn across versions — check the resolved version's own docs.rs page for its current `GovernorLayer`/`GovernorConfigBuilder` construction pattern (per-IP key extractor, burst size, replenish interval) before writing real code. Do not assume the snippet below is exactly right; it is a best-effort sketch of the *shape*, not a verified API call.

- [ ] **Step 2: Write `rate_limit.rs`**

```rust
// Backend/crates/api-gateway/src/middleware/rate_limit.rs
//! Per-IP rate limiting via tower_governor. Login gets a stricter budget
//! (20/min) than the reference's undifferentiated public-GET limiter
//! (120/min, which 6d's prices/branding routes will use) — a login POST is a
//! credential-stuffing target, a different threat model than a public read.
use std::time::Duration;

use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;

pub fn login_rate_limit_layer() -> GovernorLayer<'static, tower_governor::key_extractor::PeerIpKeyExtractor> {
    // VERIFY this builder call against the resolved tower_governor version —
    // best-effort sketch: burst_size ~ allowed instantaneous burst,
    // per(Duration) ~ steady-state replenish rate. Aim for an effective ~20
    // requests/minute/IP; the exact burst/period split is an implementation
    // detail to tune against the crate's real API, not a value to guess
    // blindly — read its docs for the correct construction.
    let config = GovernorConfigBuilder::default()
        .per_second(3)
        .burst_size(20)
        .finish()
        .expect("valid governor config");
    GovernorLayer { config: Box::leak(Box::new(config)) }
}
```

> The `Box::leak` pattern (or an `Arc`, if the resolved version's `GovernorLayer` accepts one instead of a `'static` reference) is a common `tower_governor` idiom because its config needs a `'static` lifetime for the layer — verify this is still how the resolved version wants it, don't copy blindly if the API has moved to `Arc`.

- [ ] **Step 3: Apply to `/auth/portal-login` specifically (not globally)**

In `routes/auth.rs`'s `auth_router`, apply `login_rate_limit_layer()` via `.route_layer(...)` scoped to JUST the `/portal-login` route (not the whole router — `/me`/`/logout` don't need this budget).

- [ ] **Step 4: Test**

Route-level: hammer `POST /auth/portal-login` from the SAME client (so it's recognized as one IP) more than the configured burst allows in quick succession; assert a `429 Too Many Requests` appears once the budget is exhausted, and that a request to a DIFFERENT unthrottled route (e.g. `/healthz`) in the same test still returns 200 (proves the limiter is scoped to the login route, not global). If `tokio::time::pause`/`advance` composes with the resolved `tower_governor` version's internal clock (verify — some rate-limiter crates use `std::time::Instant` directly and don't compose with Tokio's paused clock, in which case a real-time test with a short, deliberately-tight budget for the test only is the documented, accepted fallback per this project's established real-I/O-timing-test precedent).

- [ ] **Step 5: Test, clippy, deny, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/crates/api-gateway Backend/Cargo.lock
git commit -m "feat(api-gateway): per-IP rate limiting on portal-login (tower_governor)"
```

---

### Task 9: `reactor-core` binary assembly — real `PollerShared` + account bootstrap

**Files:**
- Modify: `Backend/bin/reactor-core/src/main.rs`
- Modify: `Backend/crates/store/migrations/` (add a migration if Task 2's Step 1 investigation found `app_role` needs promoting to `LOGIN`, or if it's already `LOGIN` from an earlier fase, verify and skip)
- Create: `Backend/.env.example` additions (document new env vars — modify if this file exists at repo root instead, check first)

**Interfaces produced:** none new — this task WIRES existing interfaces into a real running binary for the first time.

- [ ] **Step 1: Verify/promote `app_role` to `LOGIN` with its own password**

```bash
grep -n "app_role" Backend/crates/store/migrations/*.sql
```

Find the migration that creates `app_role` (Fase 2, per the master spec's own note). If it's `NOLOGIN` today, write a new migration `Backend/crates/store/migrations/00XX_app_role_login.sql`:
```sql
ALTER ROLE app_role LOGIN PASSWORD '__SET_VIA_ENV_AT_DEPLOY_TIME__';
```
Actually setting a real password via a migration file checked into git is a secrets-hygiene violation (Aturan Keras #5) — instead, the migration should just `ALTER ROLE app_role LOGIN;` (grant login capability, no password in the file) and the ACTUAL password gets set separately via `ALTER ROLE app_role PASSWORD '<from Docker secret>'` run once at deploy/first-boot time by an operational script — OR, simpler and consistent with this project's existing Docker-secrets pattern (Fase 3 Task 5), read the current Docker Compose setup's secrets wiring (`Docker/docker-compose.yml`, `.env.example`) to see how `tower`'s password is already supplied, and mirror that exact mechanism for `app_role`'s password rather than inventing a new one. Document whichever approach you take clearly in the migration's own comment and in your task report.

- [ ] **Step 2: Add `store::agency_credentials` module — the account bootstrap needs a `list_enabled` query**

Create `Backend/crates/store/src/agency_credentials.rs` (parallel structure to Task 2's `portal_users.rs`) with:
```rust
pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<AgencyCredential>, sqlx::Error>
```
(a simple `SELECT * FROM agency_credentials WHERE tenant_id = $1` inside `begin_tenant_tx`, returning the existing `store::models::AgencyCredential` — verify that model's exact field list against `Backend/crates/store/src/models/agency_credential.rs` before writing the query's column list). This crate has no "enabled" boolean column on `agency_credentials` itself (verify — if it doesn't, "enabled" as a bootstrap filter doesn't apply here; every row IS an account to bootstrap, full stop, since disabling one is 6b's CRUD scope, not this task's). Wire into `store::lib.rs`.

- [ ] **Step 3: Rewrite `reactor-core`'s `main()`**

Read env vars: `DATABASE_URL` (as `app_role`, per Step 1 — NOT the `tower` superuser URL used by migrations/dev tooling), `REDIS_URL`, `SPX_BASE_URL`, `AUTH_SIDECAR_URL`, `TENANT_SLUG`, `CORS_ALLOWED_ORIGINS`. Boot sequence:
1. `store::connect(database_url)` → `PgPool`.
2. `store::tenants::find_by_slug(&pool, &tenant_slug)` → the resolved `tenant_id` (panic/exit with a clear error message if not found — this is a boot-time misconfiguration, not a runtime-recoverable condition).
3. `executor::ExecutorHandle::connect(redis_url)`, `spx_client::SpxClient::new(spx_base_url)`, `poller::login::SidecarClient::new(auth_sidecar_url)`, `poller::publish::RedisPublisher::connect(redis_url)` — build the real `PollerShared` (config from `poller::PollerConfig::from_env()`, per Fase 5's already-established pattern — reuse it, don't reinvent env-var parsing).
4. `store::agency_credentials::list_all(&pool, tenant_id)` → for each row: build a `PollerState::new(...)` (decrypt its credentials via `spx_client::crypto::envelope::decrypt_agency_password` — the row's `username` is plaintext per the schema, only the password is encrypted; wrap both in `secrecy::SecretString` per Task 7b's established pattern), load its `accept_rules` (a NEW small query — add `store::accept_rules::list_by_tenant` if it doesn't exist yet, OR note this as a disclosed gap if 6c's rules-CRUD sub-phase is the more natural home for a full rules-loading helper: for 6a's bootstrap purposes, an empty `rules`/`rule_meta` — i.e. every account starts with no matching rules until 6c's settings route lets a user configure them — is an ACCEPTABLE placeholder, since Fase 6a's DoD is "the binary boots and polls accounts," not "accounts have working rules yet"; document this explicitly as a known, intentional gap for 6c to fill, do not silently pretend rules are loaded when they aren't), then `poller::schedule::ensure_restored_then_spawn(shared.clone(), state).await`.
5. Build `AppState { poller: shared, ws_hub: Hub::new(), tenant_id, cors_origins, session_cookie_name }`.
6. `api_gateway::build_router(state)`, serve on the existing `0.0.0.0:8081` bind + graceful shutdown (keep the existing `shutdown_signal` as-is).

Keep the existing `#[cfg(test)] mod tests` `/healthz` test passing — adapt its setup to construct a minimal-but-real `AppState` (a test-only Redis/Postgres-backed one, matching this project's established real-service test convention — do NOT mock `PollerShared`'s fields here just to make this one test simpler; if that proves awkward, a `#[cfg(test)] impl AppState { pub fn test_minimal(pool, redis_url) -> Self { ... } }` constructor purely for this binary's own tests is reasonable, mirroring the "unavoidable test seam" pattern already used elsewhere in this project — e.g. `Hub::test_register` in `ws-hub`).

- [ ] **Step 4: Test — a real boot smoke test**

`Backend/bin/reactor-core/tests/boot_smoke.rs` (or extend the existing in-`main.rs` test module if that stays the established location — check first): seed a real Postgres tenant + at least one `agency_credentials` row, real Redis, boot the ACTUAL binary's assembled router (not a stub) via a real `axum::serve` on `127.0.0.1:0`, assert `/healthz` returns 200 AND that the process didn't panic during the account-bootstrap loop (a malformed/undecryptable credential row must log a warning and skip that ONE account, not crash the whole boot — add a deliberately-malformed row to this test to prove that resilience, mirroring Aturan Keras #10's "one account's failure can't take down the process" at the boot-time bootstrap loop too, not just the steady-state watchdog).

- [ ] **Step 5: Full workspace verification, commit**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
cd Backend
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend Docker
git commit -m "feat(reactor-core): real PollerShared assembly + account bootstrap from agency_credentials + app_role pool"
```

---

### Task 10: Mount `ws-hub` with real session validation

**Files:**
- Modify: `Backend/crates/ws-hub/src/hub.rs` (add a session-validation hook to the upgrade path — additive, does not break Task 12/13's existing tests)
- Modify: `Backend/bin/reactor-core/src/main.rs` (mount the ws route, wire the validation callback to `store::portal_sessions::find_valid_by_hash`)

**Interfaces produced:**
- A new, ADDITIVE way to construct the ws router with validation — do NOT change `ws_router`'s existing signature (Task 12/13's tests call `ws_router(hub)` directly and must keep compiling unmodified). Add a new function instead, e.g. `pub fn ws_router_with_auth(hub: Arc<Hub>, validate: Arc<dyn Fn(&str) -> BoxFuture<'static, bool> + Send + Sync>) -> Router` (exact shape — a boxed async closure, a trait object, or a generic `S: SessionValidator` — is the implementer's call; pick whichever is simplest to actually wire from `reactor-core` without over-engineering a trait hierarchy for one caller).

- [ ] **Step 1: Read `ws-hub`'s current `ws_handler`/`handle_socket` before changing anything**

`Backend/crates/ws-hub/src/hub.rs`'s `ws_handler`/`handle_socket` (Task 12, already shipped) upgrade unconditionally on any `?session=`/`?account=` query string, no validation. Design the additive hook so `ws_router` (existing, unchanged) keeps its current no-auth behavior — a caller who wants validation opts into the NEW function.

- [ ] **Step 2: Add validated upgrade path**

Sketch (adjust to whatever's cleanest against the actual current `hub.rs` structure — read it fully first, this is not a verbatim-paste snippet):
```rust
// in hub.rs, additive:
pub type SessionValidator = std::sync::Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send + Sync,
>;

pub async fn ws_handler_with_auth(
    ws: WebSocketUpgrade,
    State((hub, validator)): State<(Arc<Hub>, SessionValidator)>,
    Query(q): Query<WsQuery>,
) -> Response {
    if q.session.is_empty() || !(validator)(q.session.clone()).await {
        return (axum::http::StatusCode::UNAUTHORIZED, "invalid session").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, hub, q))
}

pub fn ws_router_with_auth(hub: Arc<Hub>, validator: SessionValidator) -> Router {
    Router::new()
        .route("/ws", get(ws_handler_with_auth))
        .with_state((hub, validator))
}
```

- [ ] **Step 3: Wire from `reactor-core`**

In `main()` (after Task 9's assembly), build the validator closure calling `store::portal_sessions::find_valid_by_hash` (hash the raw `q.session` value the same way `session_auth` middleware does — the ws query param IS the plaintext session token, same as the cookie, per the design doc's note that ws-hub's channel-naming already uses the session id directly). Mount `ws_router_with_auth(hub, validator)` alongside `api_gateway::build_router(state)` on the same top-level `Router` (`.merge(...)`).

- [ ] **Step 4: Test**

New `Backend/crates/ws-hub/tests/session_validated_ws.rs`, following `local_broadcast.rs`'s established real-WS-client pattern (Task 12). A validator closure backed by a real Postgres session (seed one, same pattern as Task 3's tests) — a client connecting with a VALID session token as `?session=` succeeds and gets the `connected` greeting; a client connecting with a bogus/expired token gets rejected (the upgrade never completes — assert the HTTP response status before any WS handshake, or that the connection attempt errors, per whatever `tokio_tungstenite::connect_async` returns for a non-101 response).

- [ ] **Step 5: Full verification, commit**

```bash
cd Backend
cargo test -p ws-hub -p reactor-core -- --test-threads=1
cargo clippy -p ws-hub -p reactor-core --all-targets -- -D warnings
cd ..
git add Backend/crates/ws-hub Backend/bin/reactor-core
git commit -m "feat(ws-hub,reactor-core): session-validated WS upgrade, mounted into reactor-core"
```

---

### Task 11: Fase 6a final verification + sign-off

**Files:** None created — verification + plan checkbox sign-off only.

- [ ] **Step 1: Full workspace verification**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
cd Backend
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cd ..
```

- [ ] **Step 2: Cross-check this plan's scope against the design doc's DoD items relevant to 6a**

Not all 8 DoD items in the design doc are 6a's responsibility — 6a is responsible for laying DoD items #2 (session auth + `require_permission` enforcement — prove with the route-level tests already written), #4 (real `PollerShared`/non-superuser pool/binary boot — Task 9/10's tests), and #5 (security headers/CORS/body-limit/rate-limit — Tasks 6-8's tests). DoD #1 (all reference routes), #3 (OTP gate), #6 (quick-accept), #7 (workspace-clean, this step), #8 (dependency-footprint) are NOT fully satisfiable until 6b-6e ship — do not falsely check them off; note in your report which DoD items 6a genuinely closes vs. which remain open for later sub-phases.

- [ ] **Step 3: Mark this plan's checkboxes — same corruption-risk warning as every prior fase's sign-off**

Convert ONLY lines matching `^- \[ \] \*\*Step` to `- [x] **Step`. Do NOT use a blind find/replace. Guard:
```bash
grep -nE '^- \[ \] \*\*Step' Docs/superpowers/plans/2026-07-15-fase-6a-api-gateway-foundation.md
echo "checked: $(grep -cE '^- \[x\] \*\*Step' Docs/superpowers/plans/2026-07-15-fase-6a-api-gateway-foundation.md)"
echo "steps:   $(grep -cE '^- \[.\] \*\*Step' Docs/superpowers/plans/2026-07-15-fase-6a-api-gateway-foundation.md)"
```
`git diff` and manually eyeball every changed line before committing.

- [ ] **Step 4: Commit**

```bash
git add Backend Docs/superpowers/plans/2026-07-15-fase-6a-api-gateway-foundation.md
git commit -m "test(fase-6a): api-gateway foundation sign-off — full verification"
```

Fase 6a is done once this commits clean. Fase 6b (spx-creds + OTP gate) is next — it gets its own plan against the SAME design doc, consuming this sub-phase's `AppState`/`session_auth`/`require_permission`/`AppState.poller` as its foundation.

---
