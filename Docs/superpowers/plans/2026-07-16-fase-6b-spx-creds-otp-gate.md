# Fase 6b — spx-creds + OTP gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task is executed by a FRESH implementer who sees ONLY that task's text — so every task is self-contained.

**Goal:** Ship SPX-credential management (`GET/PUT/DELETE /auth/spx-credentials`, envelope-encrypted), a connectivity-test login route (`POST /auth/spx-login`), the OTP gate (`POST /auth/request-aa-otp` + `POST /auth/verify-aa-otp`, Redis-backed, WAHA-delivered to a personal number), and sub-user CRUD (`/auth/portal-users*`). This is sub-phase 2 of 5 for Fase 6 — it builds directly on Fase 6a's `AppState`/session-auth/`Permission`/`require_permission` foundation, already merged to `main`.

**Architecture:** All new routes live in `api-gateway`'s existing `routes::auth` module (or sibling modules within it), gated behind the ALREADY-shipped `session_auth` middleware + `require_permission`. `store` gains full CRUD for `agency_credentials` and `portal_users` (today only reads exist). `AppState` gains two new fields: `master_key: Arc<MasterKey>` (Fase 3's envelope-encryption key, needed by every spx-creds route to encrypt/decrypt) and `redis: redis::aio::ConnectionManager` (a dedicated connection for OTP state — `api-gateway` cannot reach into `poller::PollerShared.executor`'s Redis pool, which is `pub(crate)` to the `executor` crate; this mirrors the exact precedent `poller::publish::RedisPublisher` already established in Fase 5: open a fresh, purpose-scoped Redis connection rather than reaching across a crate boundary). The OTP gate is pure Redis state (no new Postgres table) — a 6-digit code (180s TTL), a 60s resend cooldown, a 5-attempt cap (180s window), and on success a single-use `spx:pwverify:<portal_user_id>` proof (120s) that a LATER sub-phase (6c's `PUT /bookings/settings`) will consume to authorize the `autoAccept:false→true` transition — 6b only PRODUCES that proof, it does not consume it (no route in this sub-phase reads it back).

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and the shared design doc [`Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md`](../specs/2026-07-15-fase-6-api-gateway-design.md) — **read it before starting; it is the source of truth for ALL FIVE of Fase 6's sub-phases, not just 6b.** Pay special attention to its "Corrections vs. the reference" section and its two tracked-but-not-yet-resolved notes (WS auth/cookie coherence, `docker compose up` provisioning) — neither is 6b's concern, do not attempt to fix them here.

**Scope (from the design doc).** This plan (6b) builds ONLY: spx-credentials CRUD, the spx-login connectivity test, the OTP request/verify routes, and sub-user CRUD. It does NOT build: bookings/rules routes (6c), prices/branding/locations/bot-settings CRUD (6d — this means the OTP gate's WAHA delivery reads `site_settings` for bot config that **no route can yet write** in this sub-phase; tests seed that row directly against real Postgres, matching the exact precedent Fase 6a Task 9 already established for reading `agency_credentials` before its own CRUD routes existed), or quick-accept HMAC (6e). The `spx:pwverify:<portal_user_id>` proof this sub-phase mints is not consumed by anything until 6c ships — that is expected and correct, not a bug to "complete."

**Reuse the REAL Fase 1-6a signatures (do NOT guess — read from source for this plan):**
- `spx_client::crypto::envelope::{MasterKey::load_default() -> Result<MasterKey, CryptoError>, encrypt_agency_password(master: &MasterKey, tenant_id: Uuid, password: &str) -> Result<Ciphertext, CryptoError>, decrypt_agency_password(master: &MasterKey, tenant_id: Uuid, ciphertext: &[u8], nonce: &[u8;12]) -> Result<SecretString, CryptoError>, Ciphertext{bytes: Vec<u8>, nonce: [u8;12]}, KEY_VERSION: i32 = 1}`.
- `store::models::AgencyCredential{id, tenant_id, label: String, username: String, ciphertext: Vec<u8>, nonce: Vec<u8>, key_version: i32, created_at, updated_at}` — unique `(tenant_id, label)`. `store::agency_credentials::list_all(pool, tenant_id) -> Result<Vec<AgencyCredential>, sqlx::Error>` already exists (Fase 6a Task 9) — this plan ADDS `create`/`update`/`delete`/`find_by_label` alongside it in the same file, same `begin_tenant_tx` pattern.
- `store::models::PortalUser{id, tenant_id, username, password_hash, display_name, is_main_account, enabled, created_at, updated_at}` — unique `(tenant_id, username)`. `store::portal_users::{find_by_username, find_by_id}` already exist (Fase 6a) — this plan ADDS `create`/`list_all`/`delete` alongside them.
- `spx_client::crypto::password::hash_password(password: &str) -> Result<String, CryptoError>` (Fase 3, already shipped) — for sub-user creation.
- `poller::login::{auto_login(sidecar: &SidecarClient, client: &SpxClient, account_id: &str, username: &str, password: &str) -> Option<(SpxCookies, LoginTier)>, LoginTier}` (Fase 5, already shipped) — tiers 1→2→3 in order; tier 1 needs a reachable `auth-sidecar`, tiers 2/3 are in-process HTTP. `AppState.poller.sidecar`/`AppState.poller.client` are the exact `&SidecarClient`/`&SpxClient` this needs.
- `notifier::{BotSettings{enabled, webhook_url, wa_group, waha_url, waha_api_key, waha_session, portal_label}, waha::send_to_waha_many(s: &BotSettings, group: &str, text: &str) -> (usize, usize), waha::parse_chat_ids}` — this plan ADDS a `wa_number: String` field to `BotSettings` (the reference's personal-number OTP target, distinct from `wa_group`) and threads it through every existing call site that constructs `BotSettings` (grep first — Fase 5's `notifier` crate itself has none beyond its own tests, since nothing in `poller`/`reactor-core` constructs a live `BotSettings` yet per Fase 5's own disclosed gap; this plan's OTP route is the FIRST real caller).
- `api_gateway::{AppState, ApiError (incl. Conflict for 23505), auth::{session_auth, CurrentUser}, auth::permission::{Permission, require_permission}}` — all Fase 6a, already shipped. `Permission::ManageSpxCredentials` and `Permission::ManageSubUsers` already exist as enum variants (Task 4) — this plan is the FIRST to actually call `require_permission` with them in a real handler.
- `store::pool::begin_tenant_tx(pool, tenant_id) -> Result<Transaction<'static, Postgres>, sqlx::Error>` — every new tenant-scoped query in this plan MUST use it.

**RBAC.** Every route in this plan mutates tenant-wide state (credentials, sub-users) or triggers the OTP gate that arms auto-accept — ALL of them are `require_permission`-gated (`ManageSpxCredentials` for the creds/login routes, `ManageSubUsers` for sub-user CRUD, `ArmAutoAccept` for both OTP routes — request AND verify, since even *requesting* a code should be main-account-only, matching the reference's `requireMainAccount` on `/request-aa-otp`). `GET /auth/spx-credentials` (a read) still requires a valid session (via `session_auth`) but does NOT need `require_permission` beyond that — reading which labels exist (never the decrypted password) is reasonable for any logged-in staff member of the tenant, consistent with the Fase 6a design doc's documented single-tenant data-visibility model (any portal_user of a tenant can see that tenant's own account data).

**Secrets discipline (unchanged since Fase 3, binding here specifically):** `GET /auth/spx-credentials` returns `label`+`username` ONLY — NEVER the ciphertext, nonce, or (obviously) a decrypted password. `POST /auth/spx-login`'s response reports success/failure/tier ONLY — never the password, never the SPX session cookies it produces (they are not persisted anywhere; this route is a connectivity TEST, not a login-and-store operation — no cookie-persistence table exists in this schema, and this plan does not add one). OTP codes are short-lived Redis values (not `SecretString` — they're not long-lived credentials, but must never appear in a log line or an error response body); use `subtle`-crate-style or at minimum non-short-circuiting comparison for the submitted-vs-stored code check (mirror the reference's `timingSafeEqual` intent — check what's already used for password verification in this codebase, `spx_client::crypto::password::verify_password`'s doc comment, for the established convention on this project).

**Redis key convention (new to this sub-phase, keyed by `portal_user_id` — a UUID, globally unique, so no tenant-prefix is needed for collision-safety, though include it anyway for operational grep-ability):**
- `spx:aa_otp:<tenant_id>:<portal_user_id>` → the 6-digit code, `EX 180`.
- `spx:aa_otp_rl:<tenant_id>:<portal_user_id>` → resend-cooldown marker, `SET NX EX 60` (a `request` call fails closed with a clear "too soon" error if this key already exists).
- `spx:aa_otp_att:<tenant_id>:<portal_user_id>` → attempt counter, `INCR` + `EXPIRE 180` on first increment (mirrors the reference's atomic Lua INCR+EXPIRE — Redis's `INCR` then conditionally `EXPIRE` only `IF ttl == -1` is the standard non-Lua equivalent; a simple two-command sequence is acceptable here since a lost race just means an attempt-window occasionally resets a few seconds later, not a security-relevant race, unlike the accept-gate's Lua atomicity requirement in `executor`).
- `spx:pwverify:<tenant_id>:<portal_user_id>` → the single-use proof, `EX 120`, written by `verify-aa-otp` on success. This plan does not build anything that reads it — that is 6c's job.

**Real-service testing standard (unchanged):** real Postgres (`127.0.0.1:15432`) + real Redis (`127.0.0.1:16379`) for anything DB/Redis-touching; `wiremock` for WAHA HTTP; route-level tests drive a real `axum::serve` + real HTTP client (established convention, do not call handlers directly). Unique tenant/user ids per test.

**Workflow.** Run all `cargo` commands from `Backend/`. `export PATH="$HOME/.cargo/bin:$PATH"` if `cargo` is not found. Bring up services with `cd Docker && docker compose up -d tower-postgres tower-redis`. Commit only when a task's steps say to.

---

### Task 1: `AppState` gains `master_key` + `redis`; `store` CRUD for `agency_credentials` and `portal_users`

**Files:**
- Modify: `Backend/crates/api-gateway/src/state.rs`
- Modify: `Backend/bin/reactor-core/src/main.rs` (thread the already-loaded `MasterKey` and a new Redis connection into `AppState`)
- Modify: `Backend/crates/store/src/agency_credentials.rs` (add `create`, `update`, `delete`, `find_by_label`)
- Modify: `Backend/crates/store/src/portal_users.rs` (add `create`, `list_all`, `delete`)

**Interfaces produced:**
- `AppState.master_key: Arc<spx_client::crypto::envelope::MasterKey>`, `AppState.redis: redis::aio::ConnectionManager`.
- `pub async fn agency_credentials::create(pool: &PgPool, tenant_id: Uuid, label: &str, username: &str, ciphertext: &[u8], nonce: &[u8], key_version: i32) -> Result<AgencyCredential, sqlx::Error>` (an `INSERT ... RETURNING`; a duplicate `(tenant_id, label)` surfaces as `sqlx::Error::Database` with code `23505`, which `api-gateway`'s `ApiError: From<sqlx::Error>` (Fase 6a Task 1) already maps to `409 Conflict` — do not add special-case handling here, let it propagate).
- `pub async fn agency_credentials::update(pool: &PgPool, tenant_id: Uuid, label: &str, username: &str, ciphertext: &[u8], nonce: &[u8], key_version: i32) -> Result<Option<AgencyCredential>, sqlx::Error>` (an `UPDATE ... WHERE tenant_id=$1 AND label=$2 RETURNING ...`; `None` if no row matched — the caller maps this to `404`).
- `pub async fn agency_credentials::delete(pool: &PgPool, tenant_id: Uuid, label: &str) -> Result<bool, sqlx::Error>` (`true` if a row was actually deleted).
- `pub async fn agency_credentials::find_by_label(pool: &PgPool, tenant_id: Uuid, label: &str) -> Result<Option<AgencyCredential>, sqlx::Error>`.
- `pub async fn portal_users::create(pool: &PgPool, tenant_id: Uuid, username: &str, password_hash: &str, display_name: &str, is_main_account: bool) -> Result<PortalUser, sqlx::Error>` (`INSERT ... RETURNING`, `23505` on duplicate username → `ApiError::Conflict` via the existing `From` impl, same pattern as above).
- `pub async fn portal_users::list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<PortalUser>, sqlx::Error>`.
- `pub async fn portal_users::delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error>`.

- [x] **Step 1: Add `redis` as a direct `api-gateway` dependency**

```bash
cd Backend
cargo add --package api-gateway redis --features tokio-comp,connection-manager
cd ..
```

Match the version to what's ALREADY resolved elsewhere in the workspace (`redis = "1.3.0"` per `executor`/`poller`'s `Cargo.toml` — if `cargo add` resolves something different, pin explicitly; a duplicate `redis` major version is a real problem to avoid).

- [x] **Step 2: `agency_credentials.rs` — add CRUD**

```rust
// Add to Backend/crates/store/src/agency_credentials.rs, below the existing `list_all`.

pub async fn find_by_label(
    pool: &PgPool,
    tenant_id: Uuid,
    label: &str,
) -> Result<Option<AgencyCredential>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AgencyCredential>(
        "SELECT id, tenant_id, label, username, ciphertext, nonce, key_version, \
         created_at, updated_at FROM agency_credentials WHERE tenant_id = $1 AND label = $2",
    )
    .bind(tenant_id)
    .bind(label)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    label: &str,
    username: &str,
    ciphertext: &[u8],
    nonce: &[u8],
    key_version: i32,
) -> Result<AgencyCredential, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AgencyCredential>(
        "INSERT INTO agency_credentials (tenant_id, label, username, ciphertext, nonce, key_version) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, tenant_id, label, username, ciphertext, nonce, key_version, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(label)
    .bind(username)
    .bind(ciphertext)
    .bind(nonce)
    .bind(key_version)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

#[allow(clippy::too_many_arguments)]
pub async fn update(
    pool: &PgPool,
    tenant_id: Uuid,
    label: &str,
    username: &str,
    ciphertext: &[u8],
    nonce: &[u8],
    key_version: i32,
) -> Result<Option<AgencyCredential>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AgencyCredential>(
        "UPDATE agency_credentials SET username = $3, ciphertext = $4, nonce = $5, \
         key_version = $6, updated_at = now() \
         WHERE tenant_id = $1 AND label = $2 \
         RETURNING id, tenant_id, label, username, ciphertext, nonce, key_version, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(label)
    .bind(username)
    .bind(ciphertext)
    .bind(nonce)
    .bind(key_version)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, label: &str) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM agency_credentials WHERE tenant_id = $1 AND label = $2")
        .bind(tenant_id)
        .bind(label)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
```

- [x] **Step 3: `portal_users.rs` — add CRUD**

```rust
// Add to Backend/crates/store/src/portal_users.rs, below the existing `find_by_id`.
use crate::models::PortalUser; // already imported above in the real file — don't duplicate the `use`

pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
    password_hash: &str,
    display_name: &str,
    is_main_account: bool,
) -> Result<PortalUser, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, PortalUser>(
        "INSERT INTO portal_users (tenant_id, username, password_hash, display_name, is_main_account) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, tenant_id, username, password_hash, display_name, is_main_account, enabled, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(username)
    .bind(password_hash)
    .bind(display_name)
    .bind(is_main_account)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<PortalUser>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, PortalUser>(
        "SELECT id, tenant_id, username, password_hash, display_name, is_main_account, \
         enabled, created_at, updated_at FROM portal_users WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM portal_users WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
```

- [x] **Step 4: Wire `lib.rs` re-exports**

Add `create`/`update`/`delete`/`find_by_label` and `create`/`list_all`/`delete` to the existing `pub use agency_credentials::{...}`/`pub use portal_users::{...}` blocks in `store/src/lib.rs` (they exist already for the current functions — extend, don't duplicate).

- [x] **Step 5: `AppState` + `reactor-core` wiring**

`state.rs`: add the two new fields with doc comments matching this file's existing style (see `cookie_secure`'s doc comment for the tone/detail level expected).

`main.rs`'s `build_state()`: the `MasterKey` is ALREADY loaded (read the current code — Task 9 loads it locally inside the account-bootstrap section) — lift that load to happen once, earlier, and store the result in `AppState.master_key` (an `Arc::new(master_key)`) INSTEAD OF (or in addition to, if the bootstrap loop's own borrow needs restructuring — read the actual current code to decide the minimal-diff approach) using a separately-loaded copy; do not load the master key file twice. Add a new `redis` connection: `let redis = redis::Client::open(redis_url.as_str())?.get_connection_manager().await` — handle a Redis-unreachable-at-boot condition the SAME way `RedisPublisher::connect`'s caller already does elsewhere in this file (check whether that's a hard `.expect()` or a graceful `None`/retry — match the established convention for a similarly-critical-but-not-boot-blocking dependency; OTP requests genuinely need Redis to function, so a hard `.expect()` on initial connect is likely correct here, unlike `RedisPublisher`'s optional-at-boot design — use your judgment and disclose which you chose and why).

- [x] **Step 6: Tests**

New tests in `store`'s existing test module (or a new `tests/agency_credentials_crud.rs` / `tests/portal_users_crud.rs` file if that fits this crate's established file-vs-inline-module convention better — check first): round-trip `create`→`find_by_label`→`update`→`find_by_label` (confirm updated fields)→`delete`→`find_by_label` (confirm `None`) for `agency_credentials`; the same shape for `portal_users` via `create`→`list_all`(confirm present)→`delete`→`list_all`(confirm absent). A duplicate-label `create` and a duplicate-username `create` each assert the real Postgres `23505` error comes back (not a different error code — confirm via `sqlx::Error::Database` + `.code()`). Tenant-isolation test: seed the same label/username under two different tenants, confirm each tenant's `list_all`/`find_by_label`/`find_by_username` only sees its own row.

- [x] **Step 7: Test, clippy, commit**

```bash
cd Backend
cargo test -p store -p api-gateway -p reactor-core -- --test-threads=1
cargo clippy -p store -p api-gateway -p reactor-core --all-targets -- -D warnings
cd ..
git add Backend/crates/store Backend/crates/api-gateway/src/state.rs Backend/bin/reactor-core Backend/Cargo.lock
git commit -m "feat(store,api-gateway): agency_credentials/portal_users CRUD, AppState gains master_key + redis"
```

---

### Task 2: `GET/PUT/DELETE /auth/spx-credentials`

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/spx_credentials.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`, `Backend/crates/api-gateway/src/lib.rs`

**Interfaces produced:**
- `pub fn spx_credentials_router(state: AppState) -> Router<AppState>` — nested at `/auth/spx-credentials`, `session_auth`-protected (same `route_layer` pattern Task 5's `auth_router` already established — read it first and match its exact style).
- `GET /` → `Vec<{label: String, username: String}>` (NEVER ciphertext/nonce/decrypted password).
- `PUT /:label` → upsert (create-or-update by label; body `{username: String, password: String}`), `require_permission(ManageSpxCredentials)`, encrypts via `encrypt_agency_password`, returns the same `{label, username}` shape (201 on create, 200 on update — check the label existed first via `find_by_label` to pick the status code, or just always return 200 if `PUT`'s semantics as "idempotent upsert" don't need to distinguish — your call, document it).
- `DELETE /:label` → `require_permission(ManageSpxCredentials)`, `204` on success, `404` if the label didn't exist.

- [x] **Step 1: Write `routes/spx_credentials.rs`**

```rust
// Backend/crates/api-gateway/src/routes/spx_credentials.rs
//! GET/PUT/DELETE /auth/spx-credentials — envelope-encrypted SPX login
//! storage. The decrypted password NEVER appears in any response body.
use axum::extract::{Extension, Path, State};
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::{encrypt_agency_password, KEY_VERSION};

#[derive(Debug, Serialize)]
pub struct CredentialSummary {
    pub label: String,
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct UpsertCredential {
    pub username: String,
    pub password: String,
}

async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<CredentialSummary>>, ApiError> {
    let rows = store::agency_credentials::list_all(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| CredentialSummary { label: r.label, username: r.username })
            .collect(),
    ))
}

async fn upsert(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
    Json(body): Json<UpsertCredential>,
) -> Result<Json<CredentialSummary>, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    if body.username.trim().is_empty() || body.password.is_empty() {
        return Err(ApiError::BadRequest("username and password are required".to_string()));
    }
    let ct = encrypt_agency_password(&state.master_key, user.tenant_id, &body.password)
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?;

    let existing = store::agency_credentials::find_by_label(&state.poller.pool, user.tenant_id, &label).await?;
    let row = if existing.is_some() {
        store::agency_credentials::update(
            &state.poller.pool, user.tenant_id, &label, &body.username,
            &ct.bytes, &ct.nonce, KEY_VERSION,
        ).await?.ok_or(ApiError::NotFound)?
    } else {
        store::agency_credentials::create(
            &state.poller.pool, user.tenant_id, &label, &body.username,
            &ct.bytes, &ct.nonce, KEY_VERSION,
        ).await?
    };
    Ok(Json(CredentialSummary { label: row.label, username: row.username }))
}

async fn remove(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    let deleted = store::agency_credentials::delete(&state.poller.pool, user.tenant_id, &label).await?;
    if deleted { Ok(axum::http::StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound) }
}

pub fn spx_credentials_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/{label}", put(upsert).delete(remove))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

> Verify axum 0.8's path-parameter syntax (`/{label}` vs `/:label` — this workspace is pinned to axum 0.8.9, which uses the `{param}` syntax, NOT the older `:param` syntax; confirm against how any EXISTING route in this crate with a path param is written, if one exists, or against axum 0.8's actual routing docs — do not assume from memory/older axum versions).
>
> `existing.is_some()` then a SEPARATE `update`/`create` call is a benign TOCTOU (another request could create the row between the check and the write) — the `create` branch's `23505` would then surface as `ApiError::Conflict`, which is an ACCEPTABLE outcome for a rare race on an admin-only, low-traffic endpoint (not a security issue, just a slightly confusing error on a very unlikely double-submit) — do not over-engineer a transactional upsert (`INSERT ... ON CONFLICT DO UPDATE`) unless you judge it trivially easy to add; if you do, use it instead of the two-step check, but do not spend excessive effort here.

- [x] **Step 2: Wire `routes/mod.rs` + `lib.rs`**

`routes/mod.rs`: add `pub mod spx_credentials;`. `lib.rs`'s `build_router`: `.nest("/auth/spx-credentials", routes::spx_credentials::spx_credentials_router(state.clone()))`.

- [x] **Step 3: Route-level tests**

New `Backend/crates/api-gateway/tests/spx_credentials_routes.rs`, following `auth_routes.rs`'s established real-server + real-client pattern. Seed a real tenant + main-account user + session. Cases: (1) `PUT /auth/spx-credentials/agency1` with a valid body → 200/201, response body has `{label, username}` and NO password/ciphertext field anywhere (parse the response as JSON and assert the keys present are EXACTLY `label`/`username`, not just "doesn't contain the string"); (2) `GET /` → the created credential appears, `username` correct, still no password; (3) a SUB-USER (non-main-account) session attempting `PUT`/`DELETE` → 403 (`require_permission` rejection) — but the SAME sub-user's `GET /` still succeeds (200) confirming the read/write RBAC split; (4) `DELETE /agency1` then `GET /` → the credential is gone; `DELETE` on a nonexistent label → 404. (5) Round-trip verification: after `PUT`, directly query `store::agency_credentials::find_by_label` and `spx_client::crypto::envelope::decrypt_agency_password` the stored ciphertext with the SAME master key the test server used — assert the decrypted password matches what was PUT, proving the encryption round-trip genuinely works end-to-end through the HTTP layer, not just that SOME bytes got stored.

- [x] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): GET/PUT/DELETE /auth/spx-credentials (envelope-encrypted)"
```

---

### Task 3: `POST /auth/spx-login` (connectivity test)

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/spx_login.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`, `Backend/crates/api-gateway/src/lib.rs`

**Design note (scope, read before implementing):** this Rust rewrite has NO cookie-persistence table (poller keeps live SPX session cookies in-memory per running account task — Fase 5's design). The reference's `/spx-login` in a Node/Bun single-process world could plausibly feed a freshly-obtained cookie jar directly into a live in-memory session object; that concept doesn't map cleanly here without inventing new persistence this plan is NOT scoped to build. This route is therefore a **connectivity test**: given a `label` naming a stored, already-encrypted credential, decrypt it, attempt tiers 2/3 login (NOT tier 1/browser — tier 1 needs the `auth-sidecar` process and a full browser automation round-trip, which is disproportionate for a synchronous HTTP request/response; if tier 2/3 both fail, report failure — do NOT fall through to tier 1 in this route, unlike `poller::login::auto_login`'s full 3-tier chain used elsewhere), and report success/failure + which tier worked. No cookies are persisted or returned to the client.

**Interfaces produced:**
- `POST /auth/spx-login/:label` → `{ok: bool, tier: Option<String>}` — `require_permission(ManageSpxCredentials)`.

- [x] **Step 1: Write `routes/spx_login.rs`**

```rust
// Backend/crates/api-gateway/src/routes/spx_login.rs
//! POST /auth/spx-login/:label — a CONNECTIVITY TEST for a stored SPX
//! credential (tiers 2/3 only, no browser/tier-1, no cookie persistence —
//! see this task's design note in the plan for why). Never returns the
//! password or the resulting session cookies.
use axum::extract::{Extension, Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::decrypt_agency_password;
use spx_client::crypto::secret::ExposeSecret;

#[derive(Debug, Serialize)]
pub struct SpxLoginResult {
    pub ok: bool,
    pub tier: Option<&'static str>,
}

async fn test_login(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
) -> Result<Json<SpxLoginResult>, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    let cred = store::agency_credentials::find_by_label(&state.poller.pool, user.tenant_id, &label)
        .await?
        .ok_or(ApiError::NotFound)?;
    let nonce: [u8; 12] = cred.nonce.as_slice().try_into()
        .map_err(|_| ApiError::Internal("stored nonce is not 12 bytes".to_string()))?;
    let password = decrypt_agency_password(&state.master_key, user.tenant_id, &cred.ciphertext, &nonce)
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?;

    // Tiers 2/3 only (see this task's design note — no tier 1 in a
    // synchronous HTTP route).
    if let Some(mut jar) = state.poller.client.api_login(&cred.username, password.expose_secret()).await {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return Ok(Json(SpxLoginResult { ok: true, tier: Some("api") }));
    }
    if let Some(mut jar) = state.poller.client.form_login(&cred.username, password.expose_secret()).await {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return Ok(Json(SpxLoginResult { ok: true, tier: Some("form") }));
    }
    Ok(Json(SpxLoginResult { ok: false, tier: None }))
}

pub fn spx_login_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{label}", post(test_login))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

> Verify `spx_client::SpxClient::{api_login, form_login, fetch_spx_cid}`'s exact signatures against the real source (`Backend/crates/spx-client/src/client.rs` or wherever `poller::login::auto_login` calls them from — read that call site, since it already uses these exact methods correctly) before finalizing — the snippet above is written from `poller::login::auto_login`'s known shape but confirm argument order/types match exactly (e.g. does `api_login` take `&str, &str` or something else).

- [x] **Step 2: Wire `routes/mod.rs` + `lib.rs`**

`routes/mod.rs`: add `pub mod spx_login;`. `lib.rs`: `.nest("/auth/spx-login", routes::spx_login::spx_login_router(state.clone()))`.

- [x] **Step 3: Route-level tests**

New `Backend/crates/api-gateway/tests/spx_login_routes.rs`. Use `wiremock` to stand up a fake SPX server (mirroring however `spx-client`'s OWN tests already mock login endpoints — check `spx-client/tests/login_mock.rs` for the established request/response shape and REUSE that mocking pattern, don't invent a new one). Seed a real encrypted credential via `store::agency_credentials::create` (test-side, using the SAME master key the test server's `AppState` uses). Cases: (1) wiremock returns a successful API-login response → `{ok: true, tier: "api"}`; (2) wiremock returns failure on all tiers → `{ok: false, tier: null}`; (3) a nonexistent label → 404; (4) a sub-user (non-main-account) → 403.

- [x] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): POST /auth/spx-login/:label (tier 2/3 connectivity test)"
```

---

### Task 4: `notifier::BotSettings.wa_number` + OTP Redis module

**Files:**
- Modify: `Backend/crates/notifier/src/lib.rs` (add `wa_number` field)
- Create: `Backend/crates/api-gateway/src/otp.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs`

**Interfaces produced:**
- `notifier::BotSettings.wa_number: String` (new field, `Default` derive already covers it as `String::new()`).
- `pub struct OtpError` / an inline `Result<T, ApiError>`-friendly set of functions in `api-gateway::otp`:
  - `pub async fn request(redis: &mut redis::aio::ConnectionManager, tenant_id: Uuid, portal_user_id: Uuid) -> Result<String, OtpRequestError>` — generates a 6-digit code (`rand`-crate or `getrandom`-based, matching whatever this workspace already uses elsewhere for randomness — check `spx_client::crypto`'s `getrandom` usage precedent rather than adding a new `rand` dependency if `getrandom` alone can produce a bounded random digit string), enforces the 60s resend cooldown (fails closed with `OtpRequestError::TooSoon` if the cooldown key already exists), stores the code with `EX 180`, returns the code (the CALLER — the route handler in Task 5 — sends it via WAHA; this module is pure Redis state, no HTTP).
  - `pub async fn verify(redis: &mut redis::aio::ConnectionManager, tenant_id: Uuid, portal_user_id: Uuid, submitted_code: &str) -> Result<(), OtpVerifyError>` — increments the attempt counter (rejecting with `OtpVerifyError::TooManyAttempts` at 5), compares the submitted code against the stored one (constant-time-ish — mirror this project's established `verify_password`-style non-short-circuiting comparison intent, or note if Rust's `==` on two SAME-LENGTH short strings is judged an acceptable risk here given this project's OWN established precedent — a 6-digit numeric OTP has vastly lower entropy than a password and the 5-attempt cap is the PRIMARY defense either way; document your reasoning either way, don't silently pick one without justifying it), and on success DELETES the code+attempt-counter keys (single-use) and WRITES the `spx:pwverify:<tenant_id>:<portal_user_id>` proof (`EX 120`).
  - `#[derive(Debug)] pub enum OtpRequestError { TooSoon, Redis(redis::RedisError) }`, `pub enum OtpVerifyError { NoActiveCode, WrongCode, TooManyAttempts, Redis(redis::RedisError) }` — both need `impl From<...> for ApiError` (or the route handler maps them manually — your call, pick whichever is cleaner given this crate's established `ApiError` conventions).

- [x] **Step 1: Add `wa_number` to `BotSettings`**

```rust
// Backend/crates/notifier/src/lib.rs — extend the existing struct, don't redefine it
#[derive(Debug, Clone, Default)]
pub struct BotSettings {
    pub enabled: bool,
    pub webhook_url: String,
    pub wa_group: String,
    /// The OTP-gate's personal-number delivery target (distinct from
    /// `wa_group` — the reference explicitly rejects `@g.us` group JIDs for
    /// OTP delivery; sending a one-time code to a shared group would defeat
    /// its purpose). Fase 6b's `api-gateway::otp` module is this field's
    /// first real consumer.
    pub wa_number: String,
    pub waha_url: String,
    pub waha_api_key: String,
    pub waha_session: String,
    pub portal_label: String,
}
```

Check every EXISTING test in `notifier` that constructs a `BotSettings` literal (`..Default::default()` users are unaffected; any EXHAUSTIVE field-by-field literal needs the new field added) — grep `BotSettings {` across the crate's tests and fix any that would fail to compile.

- [x] **Step 2: Write `otp.rs`**

```rust
// Backend/crates/api-gateway/src/otp.rs
//! Redis-backed OTP state for the auto-accept arm gate (`request-aa-otp` /
//! `verify-aa-otp`, Task 5). Pure Redis logic — no HTTP, no WAHA delivery
//! (the caller sends the code; this module only manages its lifecycle).
//! Keyed by `portal_user_id` (globally unique) with a `tenant_id` prefix
//! for operational grep-ability, not collision-safety.
use redis::AsyncCommands;
use uuid::Uuid;

const CODE_TTL_SECS: u64 = 180;
const RESEND_COOLDOWN_SECS: u64 = 60;
const MAX_ATTEMPTS: u64 = 5;
const ATTEMPT_WINDOW_SECS: u64 = 180;
const PWVERIFY_TTL_SECS: u64 = 120;

fn code_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:aa_otp:{tenant_id}:{user_id}")
}
fn cooldown_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:aa_otp_rl:{tenant_id}:{user_id}")
}
fn attempts_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:aa_otp_att:{tenant_id}:{user_id}")
}
fn pwverify_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:pwverify:{tenant_id}:{user_id}")
}

#[derive(Debug)]
pub enum OtpRequestError {
    TooSoon,
    Redis(redis::RedisError),
}
impl From<redis::RedisError> for OtpRequestError {
    fn from(e: redis::RedisError) -> Self { OtpRequestError::Redis(e) }
}

#[derive(Debug)]
pub enum OtpVerifyError {
    NoActiveCode,
    WrongCode,
    TooManyAttempts,
    Redis(redis::RedisError),
}
impl From<redis::RedisError> for OtpVerifyError {
    fn from(e: redis::RedisError) -> Self { OtpVerifyError::Redis(e) }
}

fn generate_code() -> String {
    let mut buf = [0u8; 4];
    // Best-effort: verify `getrandom` is already a transitive/direct dep
    // available here (it is, via `spx_client`'s own crypto — but `api-gateway`
    // may need it added directly; check before assuming). A 6-digit code
    // needs a value in 0..=999_999 — reduce a random u32 modulo 1_000_000
    // (the tiny modulo bias here is immaterial for a 180s-lived, 5-attempt,
    // non-cryptographic-secret OTP code — do not over-engineer this).
    getrandom::fill(&mut buf).expect("getrandom for OTP code");
    let n = u32::from_le_bytes(buf) % 1_000_000;
    format!("{n:06}")
}

pub async fn request(
    redis: &mut redis::aio::ConnectionManager,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<String, OtpRequestError> {
    let cooldown = cooldown_key(tenant_id, user_id);
    let acquired: bool = redis.set_options(
        &cooldown, "1",
        redis::SetOptions::default().with_expiration(redis::SetExpiry::EX(RESEND_COOLDOWN_SECS)).conditional_set(redis::ExistenceCheck::NX),
    ).await?;
    if !acquired {
        return Err(OtpRequestError::TooSoon);
    }
    let code = generate_code();
    let _: () = redis.set_ex(code_key(tenant_id, user_id), &code, CODE_TTL_SECS).await?;
    let _: () = redis.del(attempts_key(tenant_id, user_id)).await?; // fresh code, fresh attempt budget
    Ok(code)
}

pub async fn verify(
    redis: &mut redis::aio::ConnectionManager,
    tenant_id: Uuid,
    user_id: Uuid,
    submitted_code: &str,
) -> Result<(), OtpVerifyError> {
    let attempts_k = attempts_key(tenant_id, user_id);
    let attempts: u64 = redis.incr(&attempts_k, 1).await?;
    if attempts == 1 {
        let _: () = redis.expire(&attempts_k, ATTEMPT_WINDOW_SECS as i64).await?;
    }
    if attempts > MAX_ATTEMPTS {
        return Err(OtpVerifyError::TooManyAttempts);
    }

    let stored: Option<String> = redis.get(code_key(tenant_id, user_id)).await?;
    let Some(stored) = stored else {
        return Err(OtpVerifyError::NoActiveCode);
    };
    // 6-digit numeric OTP: the 5-attempt cap (already enforced above) is the
    // primary defense against brute force, not comparison timing — a
    // same-length string `==` here is an accepted, disclosed choice, unlike
    // password verification (`spx_client::crypto::password::verify_password`),
    // which defends a much-higher-entropy, much-longer-lived secret and
    // uses argon2's own constant-time comparator for exactly that reason.
    if stored != submitted_code {
        return Err(OtpVerifyError::WrongCode);
    }

    let _: () = redis.del(code_key(tenant_id, user_id)).await?;
    let _: () = redis.del(&attempts_k).await?;
    let _: () = redis.set_ex(pwverify_key(tenant_id, user_id), "1", PWVERIFY_TTL_SECS).await?;
    Ok(())
}
```

> **Verify `redis` 1.3.0's actual `set_options`/`SetOptions`/`SetExpiry`/`ExistenceCheck` API** (best-effort above) against the resolved version — this crate's exact `SET NX EX` builder API has had real shape changes across versions; check the installed source (`~/.cargo/registry`) or docs.rs for `redis` 1.3.0 specifically before finalizing. If the builder API differs, a raw `redis::cmd("SET").arg(&cooldown).arg("1").arg("NX").arg("EX").arg(RESEND_COOLDOWN_SECS).query_async(redis)` is an always-available fallback — use whichever is idiomatic for the actual resolved version. Also verify `getrandom` is reachable here (check `api-gateway/Cargo.toml` — it may need `cargo add getrandom` if not already a transitive dep this crate can name directly; Rust's extern-prelude only exposes DIRECT dependencies).

- [x] **Step 3: Wire `lib.rs`**

Add `pub mod otp;`.

- [x] **Step 4: Unit tests (real Redis, no HTTP)**

New `Backend/crates/api-gateway/tests/otp_module.rs` (or `#[cfg(test)]` inline in `otp.rs` if this crate's convention prefers that for non-route-level logic — check `Permission`'s own test placement from Fase 6a Task 4 for precedent). Real Redis (`redis://127.0.0.1:16379`), a fresh `redis::Client::open(...).get_connection_manager().await` per test, unique `tenant_id`/`user_id` `Uuid::new_v4()` per test (no cross-test collision risk). Cases: (1) `request` succeeds, returns a 6-digit numeric string; (2) an immediate second `request` for the SAME user → `TooSoon`; (3) `verify` with the correct code → `Ok(())`, and a subsequent `verify` with the SAME (now-deleted) code → `NoActiveCode`; (4) `verify` with a wrong code 5 times → the 6th attempt (or whichever exact count the implementation enforces — confirm against the actual `MAX_ATTEMPTS` boundary) → `TooManyAttempts`; (5) after a successful `verify`, directly read `spx:pwverify:<tenant>:<user>` from Redis and confirm it exists with the expected TTL ballpark (`TTL` command, assert it's `> 0` and `<= 120`).

- [x] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p notifier -p api-gateway -- --test-threads=1
cargo clippy -p notifier -p api-gateway --all-targets -- -D warnings
cd ..
git add Backend/crates/notifier Backend/crates/api-gateway
git commit -m "feat(notifier,api-gateway): BotSettings.wa_number + Redis-backed OTP module (request/verify)"
```

---

### Task 5: `POST /auth/request-aa-otp` + `POST /auth/verify-aa-otp`

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/otp.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`, `Backend/crates/api-gateway/src/lib.rs`

**Design note:** these routes need a `BotSettings` to send the OTP text via WAHA — read it from `site_settings` (key TBD by 6d's own CRUD, e.g. `"bot"` — check whether Fase 3's existing WAHA-key encryption work already established a specific `site_settings.key` convention for bot config; grep `site_settings` usage across the whole codebase, especially Fase 3's `waha_settings_pg.rs` test, for the EXACT key string already in use, and reuse it verbatim rather than inventing a new one). No route in THIS plan writes that `site_settings` row (6d's job) — tests seed it directly via `sqlx::query` against real Postgres, matching Task 9's established precedent for reading a table before its own CRUD ships.

**Interfaces produced:**
- `POST /auth/request-aa-otp` → `require_permission(ArmAutoAccept)`, `{ok: true}` on success, maps `OtpRequestError::TooSoon` to a `409 Conflict` (or `429` — your call, document which and why; `429 Too Many Requests` arguably fits a resend-cooldown better than `409`, but this project doesn't have an established precedent for this exact case — pick one, disclose it), a missing/malformed bot config to a clear `500`-with-safe-message or a `400` if you judge "OTP delivery not configured" is more a client-facing config problem than a server fault (your call).
- `POST /auth/verify-aa-otp` → body `{code: String}`, `require_permission(ArmAutoAccept)`, `{ok: true}` on success, maps `OtpVerifyError` variants to appropriate statuses (`WrongCode`/`NoActiveCode` → `401` or `400` — pick one consistently, matching the SAME "don't distinguish exact failure reason in a way that helps an attacker" caution Task 5's login route already established for password checks, though a 6-digit OTP's threat model is different — disclose your reasoning; `TooManyAttempts` → `429`).

- [x] **Step 1: Write `routes/otp.rs`**

```rust
// Backend/crates/api-gateway/src/routes/otp.rs
//! POST /auth/request-aa-otp, POST /auth/verify-aa-otp — the OTP gate that
//! (once 6c ships) authorizes the autoAccept:false->true transition. This
//! task only PRODUCES the spx:pwverify:<tenant>:<user> proof on success;
//! nothing in this plan consumes it yet.
use axum::extract::{Extension, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::otp::{self, OtpRequestError, OtpVerifyError};
use crate::state::AppState;
use notifier::waha::send_to_waha_many;

#[derive(Debug, Serialize)]
pub struct OtpOk {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct VerifyOtpRequest {
    pub code: String,
}

async fn request_otp(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<OtpOk>, ApiError> {
    require_permission(&user, Permission::ArmAutoAccept)?;

    let code = otp::request(&mut state.redis, user.tenant_id, user.portal_user_id)
        .await
        .map_err(|e| match e {
            OtpRequestError::TooSoon => ApiError::Conflict("otp already requested, try again shortly".to_string()),
            OtpRequestError::Redis(e) => ApiError::Internal(e.to_string()),
        })?;

    let bot = load_bot_settings(&state, user.tenant_id).await?;
    if bot.wa_number.trim().is_empty() {
        return Err(ApiError::BadRequest("OTP delivery is not configured for this tenant".to_string()));
    }
    let text = format!("Kode verifikasi TOWER Anda: {code} (berlaku 3 menit)");
    let (sent, _failed) = send_to_waha_many(&bot, &bot.wa_number, &text).await;
    if sent == 0 {
        tracing::warn!(tenant_id = %user.tenant_id, "OTP WAHA send reported zero delivered");
    }
    Ok(Json(OtpOk { ok: true }))
}

async fn verify_otp(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<VerifyOtpRequest>,
) -> Result<Json<OtpOk>, ApiError> {
    require_permission(&user, Permission::ArmAutoAccept)?;
    otp::verify(&mut state.redis, user.tenant_id, user.portal_user_id, &body.code)
        .await
        .map_err(|e| match e {
            OtpVerifyError::NoActiveCode | OtpVerifyError::WrongCode => {
                ApiError::Unauthorized // uniform — don't help an attacker distinguish "no code" from "wrong code"
            }
            OtpVerifyError::TooManyAttempts => ApiError::Conflict("too many attempts, request a new code".to_string()),
            OtpVerifyError::Redis(e) => ApiError::Internal(e.to_string()),
        })?;
    Ok(Json(OtpOk { ok: true }))
}

async fn load_bot_settings(state: &AppState, tenant_id: uuid::Uuid) -> Result<notifier::BotSettings, ApiError> {
    // VERIFY the exact `site_settings.key` this project already uses for bot
    // config (grep Fase 3's `waha_settings_pg.rs` test) before finalizing —
    // this is a best-effort sketch of the shape, not a confirmed key string.
    todo!("read site_settings row, decrypt waha_api_key via decrypt_waha_key, build BotSettings — see task brief")
}

pub fn otp_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/request-aa-otp", post(request_otp))
        .route("/verify-aa-otp", post(verify_otp))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

> **The `load_bot_settings` function is explicitly a `todo!()` stub in this snippet — you must write it for real.** Steps: (1) grep the codebase for the exact `site_settings.key` string Fase 3's `waha_settings_pg.rs` test already established for bot/WAHA config (do not invent a new key name); (2) `store` has no dedicated `site_settings` query module yet — add a minimal one (`store::site_settings::get_json(pool, tenant_id, key) -> Result<Option<serde_json::Value>, sqlx::Error>` inside `begin_tenant_tx`, mirroring this plan's Task 1 CRUD style) as part of THIS task, since it's genuinely needed here and is a small, focused addition — do not build a full site_settings CRUD (that's 6d's job, this is just a read helper); (3) deserialize the JSONB value's fields into a `BotSettings` (check what shape the reference/Fase-3 test actually stored — `waha_url`, `waha_api_key` (ENCRYPTED — decrypt via `spx_client::crypto::envelope::decrypt_waha_key`), `waha_session`, `wa_group`, `wa_number`, `webhook_url`, `portal_label`, `enabled`); (4) if the row doesn't exist at all (expected in THIS sub-phase, since 6d's write route doesn't exist yet), return a clear, disclosed error (`ApiError::BadRequest("OTP delivery is not configured for this tenant")` — the SAME message the stub's caller already checks for on an empty `wa_number`, so consolidate into one code path rather than two separate empty-config checks).

- [x] **Step 2: Wire `routes/mod.rs` + `lib.rs`**

`routes/mod.rs`: add `pub mod otp;` (note: this is `crate::routes::otp`, distinct from the ALREADY-existing `crate::otp` module from Task 4 — the route HANDLERS live in `routes::otp`, the Redis LOGIC lives in the top-level `otp` module; make sure the module paths don't collide/shadow confusingly, and use clear `use` aliases if needed). `lib.rs`: `.nest("/auth", routes::otp::otp_router(state.clone()))` (mounted at `/auth` directly, not a further sub-path, since the routes are already named `/request-aa-otp`/`/verify-aa-otp` in full).

- [x] **Step 3: Route-level tests**

New `Backend/crates/api-gateway/tests/otp_routes.rs`. Seed a real tenant + main-account user + session, AND a real `site_settings` row (direct `sqlx::query` INSERT, using the exact key/shape Step 1's `load_bot_settings` reads) pointing `waha_url` at a `wiremock` server, `wa_number` at a test phone-number-shaped string. Cases: (1) `POST /request-aa-otp` → 200, wiremock recorded exactly one `/api/sendText` call whose body's `chatId` matches the configured `wa_number` (not `wa_group`); (2) an IMMEDIATE second request → 409/429 (whichever you chose) from the cooldown; (3) `POST /verify-aa-otp` with the WRONG code → uniform rejection status; (4) with the RIGHT code (you'll need to intercept/know the code — either read it directly from Redis in the test after step 1's request, matching the "verify via direct backend read" pattern this project's tests already use elsewhere, or refactor `request_otp` to make the code retrievable in test builds — pick whichever is less invasive) → 200, and a direct Redis read confirms `spx:pwverify:<tenant>:<user>` now exists; (5) a sub-user (non-main-account) → 403 on BOTH routes.

- [x] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p api-gateway -p store -- --test-threads=1
cargo clippy -p api-gateway -p store --all-targets -- -D warnings
cd ..
git add Backend/crates/api-gateway Backend/crates/store
git commit -m "feat(api-gateway): POST /auth/request-aa-otp + /auth/verify-aa-otp (WAHA-delivered, Redis-backed)"
```

---

### Task 6: Sub-user CRUD (`GET/POST/DELETE /auth/portal-users`)

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/portal_users.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`, `Backend/crates/api-gateway/src/lib.rs`

**Interfaces produced:**
- `GET /auth/portal-users` → `Vec<{id, username, display_name, is_main_account, enabled}>` (never `password_hash`) — any logged-in user of the tenant may list (matches `GET /auth/spx-credentials`'s RBAC posture — a read within one's own tenant).
- `POST /auth/portal-users` → body `{username, password, display_name, is_main_account: bool}`, `require_permission(ManageSubUsers)`, hashes the password via `hash_password`, `201` with the same summary shape (never the hash).
- `DELETE /auth/portal-users/:id` → `require_permission(ManageSubUsers)`, `204`/`404`. Guard: a user must NOT be able to delete their OWN account via this route (a real, if edge-case, self-lockout risk) — reject with `400`/`403` if `id == user.portal_user_id`.

- [x] **Step 1: Write `routes/portal_users.rs`**

```rust
// Backend/crates/api-gateway/src/routes/portal_users.rs
//! GET/POST/DELETE /auth/portal-users — sub-user management, gated by
//! ManageSubUsers (main-account-only for writes; any tenant member may list).
use axum::extract::{Extension, Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::password::hash_password;

#[derive(Debug, Serialize)]
pub struct PortalUserSummary {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_main_account: bool,
    pub enabled: bool,
}

impl From<store::models::PortalUser> for PortalUserSummary {
    fn from(u: store::models::PortalUser) -> Self {
        Self { id: u.id, username: u.username, display_name: u.display_name, is_main_account: u.is_main_account, enabled: u.enabled }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePortalUser {
    pub username: String,
    pub password: String,
    pub display_name: String,
    #[serde(default)]
    pub is_main_account: bool,
}

async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<PortalUserSummary>>, ApiError> {
    let rows = store::portal_users::list_all(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<CreatePortalUser>,
) -> Result<Json<PortalUserSummary>, ApiError> {
    require_permission(&user, Permission::ManageSubUsers)?;
    if body.username.trim().is_empty() || body.password.len() < 8 {
        return Err(ApiError::BadRequest("username required, password must be >= 8 chars".to_string()));
    }
    let hash = hash_password(&body.password).map_err(|e| ApiError::Internal(format!("{e:?}")))?;
    let row = store::portal_users::create(
        &state.poller.pool, user.tenant_id, &body.username, &hash, &body.display_name, body.is_main_account,
    ).await?;
    Ok(Json(row.into()))
}

async fn remove(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    require_permission(&user, Permission::ManageSubUsers)?;
    if id == user.portal_user_id {
        return Err(ApiError::BadRequest("cannot delete your own account".to_string()));
    }
    let deleted = store::portal_users::delete(&state.poller.pool, user.tenant_id, id).await?;
    if deleted { Ok(axum::http::StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound) }
}

pub fn portal_users_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", axum::routing::delete(remove))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [x] **Step 2: Wire `routes/mod.rs` + `lib.rs`**

`routes/mod.rs`: add `pub mod portal_users;`. `lib.rs`: `.nest("/auth/portal-users", routes::portal_users::portal_users_router(state.clone()))`.

- [x] **Step 3: Route-level tests**

New `Backend/crates/api-gateway/tests/portal_users_routes.rs`. Cases: (1) main-account session `POST`s a new sub-user → 200, response has no `password_hash` field; the created user can then actually LOG IN with the submitted password (round-trip through Task 5's `POST /auth/portal-login` from Fase 6a, or directly via `store::portal_users::find_by_username` + `verify_password`, whichever is a cleaner cross-crate test dependency — your call); (2) `GET /` lists both the main account and the new sub-user; (3) a SUB-USER session attempting `POST`/`DELETE` → 403, but `GET /` still 200; (4) `DELETE /:id` for the sub-user → 204, then `GET /` no longer lists them; (5) a main-account user attempting to `DELETE` THEIR OWN `id` → 400 (self-lockout guard).

- [x] **Step 4: Test, clippy, deny, commit**

```bash
cd Backend
cargo test -p api-gateway -- --test-threads=1
cargo clippy -p api-gateway --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/crates/api-gateway
git commit -m "feat(api-gateway): GET/POST/DELETE /auth/portal-users (sub-user CRUD)"
```

---

### Task 7: Fase 6b final verification + sign-off

**Files:** None created — verification + plan checkbox sign-off only.

- [x] **Step 1: Full workspace verification**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
cd Backend
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cd ..
```

Note: `api-gateway`'s `tests/cors_and_body_limit.rs::oversized_body_gets_413` is a KNOWN, previously-disclosed pre-existing flake under heavy parallel test load (macOS `ConnectionReset` timing race, unrelated to any 6a/6b code) — if ONLY that test fails, re-run it in isolation to confirm, note in your report, do not treat as a new regression. Any OTHER failure is real.

- [x] **Step 2: Cross-check this plan's scope against the design doc's DoD — 6b's own slice only**

6b closes real progress on design-doc DoD #1 (spx-creds/spx-login/sub-user routes are now real, per the reference route inventory), #3 (the OTP gate's REQUEST/VERIFY mechanics — the FULL DoD #3 also needs 6c's consumption of the `spx:pwverify` proof in `PUT /bookings/settings`, which is NOT this sub-phase's job; do not claim #3 fully closed). Do NOT claim #2/#4/#5 (already 6a's, unaffected) or #6/#8 (6c-6e's) as newly closed by this plan.

- [x] **Step 3: Mark this plan's checkboxes — same corruption-risk warning as every prior sign-off**

Convert ONLY lines matching `^- \[ \] \*\*Step` to `- [x] **Step`. Do NOT use a blind find/replace. Guard:
```bash
grep -nE '^- \[ \] \*\*Step' Docs/superpowers/plans/2026-07-16-fase-6b-spx-creds-otp-gate.md
echo "checked: $(grep -cE '^- \[x\] \*\*Step' Docs/superpowers/plans/2026-07-16-fase-6b-spx-creds-otp-gate.md)"
echo "steps:   $(grep -cE '^- \[.\] \*\*Step' Docs/superpowers/plans/2026-07-16-fase-6b-spx-creds-otp-gate.md)"
```
`git diff` and manually eyeball every changed line before committing.

- [x] **Step 4: Commit**

```bash
git add Backend Docs/superpowers/plans/2026-07-16-fase-6b-spx-creds-otp-gate.md
git commit -m "test(fase-6b): spx-creds + OTP gate sign-off — full verification"
```

Fase 6b is done once this commits clean. Fase 6c (bookings + rules) is next — it gets its own plan against the SAME shared Fase 6 design doc, consuming this sub-phase's `AppState.master_key`/`AppState.redis`/`store::agency_credentials` CRUD/the OTP `spx:pwverify` proof as its foundation.

---
