# Fase 8-Deploy-lokal — Working Local `docker compose up` Go-Live Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `docker compose up` bring the whole TOWER stack up healthy on a fresh machine — migrations applied, `app_role` password provisioned, an initial tenant+admin seeded — so the operator logs in at `http://localhost:8080` and uses all 7 surfaces.

**Architecture:** A new `bin/tower-admin` operator binary (lib+bin: testable functions + a thin CLI) runs migrations + role-password provisioning + idempotent tenant/admin seeding, all as the `tower` superuser. A one-shot `tower-migrate` Compose service runs it before `reactor-core`/`auth-sidecar`/`tower-retention` (via `depends_on: service_completed_successfully`). Those services get their container-network runtime env, and connect as `app_role` (never superuser).

**Tech Stack:** Rust (`store`, `spx-client` crypto, sqlx 0.9), Docker Compose, Postgres 16.

## Global Constraints

- **Aturan Keras #5 — no plaintext secret in git.** Dev-only passwords (`APP_ROLE_PASSWORD`, `ADMIN_PASSWORD`, `RETENTION_ROLE_PASSWORD`) live in the gitignored `Docker/.env` with documented defaults in the tracked `Docker/.env.example`, exactly like the existing `POSTGRES_PASSWORD`. No real secret in a committed file or a migration.
- **Aturan Keras #8** — unique container names, no published ports except the edge (the `127.0.0.1:15432`/`16379` dev-tool publishes are the existing exception). `tower-migrate` publishes nothing.
- **RLS non-bypass** — `reactor-core`/`auth-sidecar` connect as `app_role`, NEVER `tower`. ONLY `tower-admin` (one-shot) uses the superuser DSN.
- **Idempotent** — every `tower-admin` subcommand is safe to re-run (migrations tracked by sqlx; `ALTER ROLE` re-sets the same value; seeds check-then-insert and never overwrite an existing admin).
- **sqlx 0.9 `SqlSafeStr`** — any `format!`-built SQL passed to `sqlx::query*` needs `sqlx::AssertSqlSafe(...)`. Here only the `ALTER ROLE` DDL is dynamic; role names are compile-time literals and the password is single-quote-escaped.
- Reference design: `Docs/superpowers/specs/2026-07-21-fase-8-deploy-lokal-design.md`.
- Backend commands: infra must be up (`docker compose -f Docker/docker-compose.yml up -d tower-postgres tower-redis`); tests use `DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower`.

**Verified facts (from source):** `store::connect(url) -> Result<PgPool>`; `store::run_migrations(&pool) -> Result<(), sqlx::migrate::MigrateError>`; `store::tenants::find_by_slug(pool, slug) -> Result<Option<Tenant>>` (Tenant has `.id: Uuid`); `store::portal_users::find_by_username(pool, tenant_id, username) -> Result<Option<PortalUser>>`; `store::portal_users::create(pool, tenant_id, username, password_hash, display_name, is_main_account) -> Result<PortalUser>`; `spx_client::crypto::password::hash_password(pw) -> Result<String, CryptoError>`; tenants insert columns `(id, name, slug)`. `tower-admin` depends on `store` + `spx-client` (for `hash_password`), so its build graph pulls `wreq`/BoringSSL → its Dockerfile builder needs `cmake` (mirror `reactor-core.Dockerfile`). `Docker/.env` is gitignored; `Docker/.env.example` is tracked.

---

### Task 1: `bin/tower-admin` crate + `init` (migrate + provision role passwords) (TDD)

**Files:**
- Create: `Backend/bin/tower-admin/Cargo.toml`
- Create: `Backend/bin/tower-admin/src/lib.rs`
- Create: `Backend/bin/tower-admin/src/main.rs`
- Modify: `Backend/Cargo.toml` (add `"bin/tower-admin"` to `members`)
- Test: `Backend/bin/tower-admin/tests/tower_admin_pg.rs`

**Interfaces:**
- Produces:
  - `pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>`
  - `pub async fn init(superuser_url: &str, app_role_password: &str, retention_role_password: &str) -> Result<()>` — runs migrations, then sets `app_role`'s password and `retention_role`'s LOGIN+password (idempotent).
- Consumes: `store::{connect, run_migrations}`.

**Context:** `init` is the one-shot deploy step. Migrations create `app_role` (LOGIN via `0019`, no password) and `retention_role` (NOLOGIN via `0022`). `init` then provisions both passwords. The role names are literals; the password is escaped (`'` → `''`) because DDL cannot bind a parameter.

- [ ] **Step 1: Create `Backend/bin/tower-admin/Cargo.toml`**

```toml
[package]
name = "tower-admin"
version.workspace = true
edition.workspace = true
publish.workspace = true

[dependencies]
store = { version = "0.1.0", path = "../../crates/store" }
spx-client = { version = "0.1.0", path = "../../crates/spx-client" }
sqlx = { version = "0.9.0", features = ["postgres", "runtime-tokio", "tls-rustls-ring-native-roots", "macros", "migrate", "uuid", "chrono"] }
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros"] }
uuid = { version = "1.23.5", features = ["v4"] }

[dev-dependencies]
serial_test = "3"
```

(Confirm the `store`/`spx-client` path deps compile; if `spx-client`'s package name differs from `spx-client`, use the name from `Backend/crates/spx-client/Cargo.toml`'s `[package] name`.)

- [ ] **Step 2: Add the workspace member** — in `Backend/Cargo.toml`, add after `"bin/retention"`:

```toml
    "bin/retention",
    "bin/tower-admin",
```

- [ ] **Step 3: Write the failing test** → `Backend/bin/tower-admin/tests/tower_admin_pg.rs`

```rust
// Integration tests for tower-admin against real Postgres. DATABASE_URL must be the `tower`
// superuser (these run migrations and ALTER ROLE). #[serial] because they mutate global roles
// and shared tenants.
use serial_test::serial;
use sqlx::PgPool;

fn superuser_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

#[tokio::test]
#[serial]
async fn init_is_idempotent_and_sets_app_role_password() {
    let url = superuser_url();
    // Run twice — must not error the second time (migrations tracked, ALTER ROLE re-set).
    tower_admin::init(&url, "app_role_dev_only", "retention_role_dev_only").await.expect("init 1");
    tower_admin::init(&url, "app_role_dev_only", "retention_role_dev_only").await.expect("init 2");

    // app_role can now authenticate with that password (proves the ALTER ROLE took effect).
    let app_url = "postgres://app_role:app_role_dev_only@127.0.0.1:15432/tower";
    let pool = PgPool::connect(app_url).await.expect("app_role connects with provisioned password");
    let one: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&pool).await.unwrap();
    assert_eq!(one, 1);
}

#[tokio::test]
#[serial]
async fn init_escapes_a_password_with_a_single_quote() {
    let url = superuser_url();
    tower_admin::init(&url, "pa'ss'word", "retention_role_dev_only").await.expect("init with quote");
    // Reset to the normal dev password so other tests/services aren't left with the odd one.
    tower_admin::init(&url, "app_role_dev_only", "retention_role_dev_only").await.expect("reset");
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p tower-admin 2>&1 | tail -15`
Expected: FAIL to compile — `tower_admin` crate / `init` do not exist.

- [ ] **Step 5: Implement `Backend/bin/tower-admin/src/lib.rs`**

```rust
//! Fase 8 operator/init tool. Runs migrations + provisions role passwords + seeds the initial
//! tenant/admin — all as the `tower` superuser, once, before reactor-core boots. Every function
//! is idempotent. This is the ONLY deploy-time superuser consumer; reactor-core stays app_role.
use sqlx::PgPool;
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Run migrations, then provision `app_role`'s password and `retention_role`'s LOGIN+password.
pub async fn init(
    superuser_url: &str,
    app_role_password: &str,
    retention_role_password: &str,
) -> Result<()> {
    let pool = store::connect(superuser_url).await?;
    store::run_migrations(&pool).await?;
    // app_role already has LOGIN (migration 0019); just set its password.
    set_role_password(&pool, "app_role", app_role_password, false).await?;
    // retention_role is NOLOGIN (migration 0022); grant LOGIN + set its password so a hardened
    // deploy CAN switch the retention worker to it (the switch itself is a tracked follow-up).
    set_role_password(&pool, "retention_role", retention_role_password, true).await?;
    Ok(())
}

/// `ALTER ROLE <role> [LOGIN] PASSWORD '<escaped>'`. `role` is always a compile-time literal
/// from this module (never external input); `password` is single-quote-escaped (the only
/// injection vector in a DDL string literal). DDL cannot bind a parameter, so this is a
/// deliberately-asserted-safe dynamic statement.
async fn set_role_password(
    pool: &PgPool,
    role: &str,
    password: &str,
    grant_login: bool,
) -> Result<()> {
    let escaped = password.replace('\'', "''");
    let login = if grant_login { "LOGIN " } else { "" };
    let sql = format!("ALTER ROLE {role} {login}PASSWORD '{escaped}'");
    sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
    Ok(())
}

/// Idempotently ensure a tenant with `slug` exists; return its id. Used by the seed functions
/// (Task 2).
pub(crate) async fn ensure_tenant(pool: &PgPool, slug: &str, name: &str) -> Result<Uuid> {
    if let Some(t) = store::tenants::find_by_slug(pool, slug).await? {
        return Ok(t.id);
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(name)
        .bind(slug)
        .execute(pool)
        .await?;
    let t = store::tenants::find_by_slug(pool, slug)
        .await?
        .ok_or("tenant creation failed (slug not found after insert)")?;
    Ok(t.id)
}
```

- [ ] **Step 6: Write a minimal `Backend/bin/tower-admin/src/main.rs`** (dispatch is fleshed out in Task 2; this compiles the bin now)

```rust
//! tower-admin CLI. Subcommands wired in Task 2; `init` works now.
use std::process::exit;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let db = std::env::var("TOWER_ADMIN_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string());

    let result = match cmd.as_str() {
        "init" => {
            tower_admin::init(
                &db,
                &env_or("APP_ROLE_PASSWORD", "app_role_dev_only"),
                &env_or("RETENTION_ROLE_PASSWORD", "retention_role_dev_only"),
            )
            .await
        }
        other => {
            eprintln!("tower-admin: unknown subcommand {other:?}; use: init|seed-admin|seed-e2e|bootstrap");
            exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("tower-admin {cmd} failed: {e}");
        exit(1);
    }
    println!("tower-admin {cmd}: ok");
}
```

- [ ] **Step 7: Run to verify it passes**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p tower-admin 2>&1 | tail -15`
Expected: PASS — both `init_*` tests. (After this, the `app_role` password on the dev DB is `app_role_dev_only`.)

- [ ] **Step 8: Commit**

```bash
git add Backend/bin/tower-admin Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(tower-admin): init — migrate + provision app_role/retention_role passwords (Fase 8)"
```

---

### Task 2: `tower-admin` `seed-admin` + `seed-e2e` + CLI dispatch (TDD)

**Files:**
- Modify: `Backend/bin/tower-admin/src/lib.rs` (append)
- Modify: `Backend/bin/tower-admin/src/main.rs` (add subcommands)
- Test: `Backend/bin/tower-admin/tests/tower_admin_pg.rs` (append)

**Interfaces:**
- Consumes: `init`, `ensure_tenant` (Task 1); `store::portal_users::{find_by_username, create}`, `spx_client::crypto::password::hash_password`.
- Produces:
  - `pub async fn seed_admin(url: &str, tenant_slug: &str, admin_username: &str, admin_password: &str) -> Result<()>`
  - `pub async fn seed_e2e(url: &str) -> Result<()>`

**Context:** `seed_admin` gives the operator a login after `docker compose up`. `seed_e2e` is the committed replacement for the ad-hoc e2e seed. Both are idempotent and never overwrite an existing user.

- [ ] **Step 1: Write the failing test** (append to `tower_admin_pg.rs`)

```rust
#[tokio::test]
#[serial]
async fn seed_admin_creates_once_and_never_overwrites() {
    let url = superuser_url();
    tower_admin::init(&url, "app_role_dev_only", "retention_role_dev_only").await.expect("init");

    let slug = format!("seed-admin-test-{}", uuid::Uuid::new_v4());
    tower_admin::seed_admin(&url, &slug, "admin", "first-password").await.expect("seed 1");

    let pool = PgPool::connect(&url).await.unwrap();
    let tenant = store::tenants::find_by_slug(&pool, &slug).await.unwrap().expect("tenant exists");
    let user = store::portal_users::find_by_username(&pool, tenant.id, "admin").await.unwrap().expect("admin exists");
    let first_hash: String = sqlx::query_scalar("SELECT password_hash FROM portal_users WHERE id = $1")
        .bind(user.id).fetch_one(&pool).await.unwrap();
    assert!(user.is_main_account, "admin is a main account");

    // Re-run with a DIFFERENT password — must NOT overwrite the existing admin.
    tower_admin::seed_admin(&url, &slug, "admin", "different-password").await.expect("seed 2");
    let second_hash: String = sqlx::query_scalar("SELECT password_hash FROM portal_users WHERE id = $1")
        .bind(user.id).fetch_one(&pool).await.unwrap();
    assert_eq!(first_hash, second_hash, "existing admin password is never overwritten");

    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant.id).execute(&pool).await.ok();
}

#[tokio::test]
#[serial]
async fn seed_e2e_creates_the_two_fixture_users_idempotently() {
    let url = superuser_url();
    tower_admin::init(&url, "app_role_dev_only", "retention_role_dev_only").await.expect("init");

    tower_admin::seed_e2e(&url).await.expect("seed_e2e 1");
    tower_admin::seed_e2e(&url).await.expect("seed_e2e 2"); // idempotent

    let pool = PgPool::connect(&url).await.unwrap();
    let tenant = store::tenants::find_by_slug(&pool, "tower-dev").await.unwrap().expect("tower-dev tenant");
    let main = store::portal_users::find_by_username(&pool, tenant.id, "e2e-test-user").await.unwrap().expect("e2e-test-user");
    let ro = store::portal_users::find_by_username(&pool, tenant.id, "e2e-readonly-user").await.unwrap().expect("e2e-readonly-user");
    assert!(main.is_main_account);
    assert!(!ro.is_main_account);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p tower-admin seed 2>&1 | tail -15`
Expected: FAIL to compile — `seed_admin`/`seed_e2e` do not exist.

- [ ] **Step 3: Implement** (append to `Backend/bin/tower-admin/src/lib.rs`)

```rust
use spx_client::crypto::password::hash_password;

/// Idempotently ensure a main-account portal_user `username` exists in the tenant `tenant_id`
/// with the given password. Never overwrites an existing user (so an operator who changed the
/// password keeps it). Returns true if it created the user, false if it already existed.
async fn ensure_user(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
    password: &str,
    display_name: &str,
    is_main_account: bool,
) -> Result<bool> {
    if store::portal_users::find_by_username(pool, tenant_id, username).await?.is_some() {
        return Ok(false);
    }
    let hash = hash_password(password)?;
    store::portal_users::create(pool, tenant_id, username, &hash, display_name, is_main_account).await?;
    Ok(true)
}

/// Create (idempotently) the go-live tenant + admin main-account. Never overwrites.
pub async fn seed_admin(
    url: &str,
    tenant_slug: &str,
    admin_username: &str,
    admin_password: &str,
) -> Result<()> {
    let pool = store::connect(url).await?;
    let tenant_id = ensure_tenant(&pool, tenant_slug, "TOWER").await?;
    let created = ensure_user(&pool, tenant_id, admin_username, admin_password, "Administrator", true).await?;
    println!(
        "seed-admin: admin {} @ tenant {} ({})",
        admin_username,
        tenant_slug,
        if created { "created" } else { "already existed" }
    );
    Ok(())
}

/// Create (idempotently) the Playwright e2e fixtures: tenant `tower-dev` + e2e-test-user (main)
/// and e2e-readonly-user (non-main), password `correct-horse-battery-staple`. NOT run by the
/// production compose up — invoked by the e2e harness / a dev command.
pub async fn seed_e2e(url: &str) -> Result<()> {
    const E2E_PW: &str = "correct-horse-battery-staple";
    let pool = store::connect(url).await?;
    let tenant_id = ensure_tenant(&pool, "tower-dev", "TOWER Dev").await?;
    ensure_user(&pool, tenant_id, "e2e-test-user", E2E_PW, "E2E Test User", true).await?;
    ensure_user(&pool, tenant_id, "e2e-readonly-user", E2E_PW, "E2E Readonly User", false).await?;
    println!("seed-e2e: tower-dev tenant + e2e-test-user/e2e-readonly-user ready");
    Ok(())
}
```

- [ ] **Step 4: Wire the subcommands into `main.rs`** — replace the `match cmd.as_str()` block:

```rust
    let result = match cmd.as_str() {
        "init" => {
            tower_admin::init(
                &db,
                &env_or("APP_ROLE_PASSWORD", "app_role_dev_only"),
                &env_or("RETENTION_ROLE_PASSWORD", "retention_role_dev_only"),
            )
            .await
        }
        "seed-admin" => {
            tower_admin::seed_admin(
                &db,
                &env_or("TENANT_SLUG", "tower-local"),
                &env_or("ADMIN_USERNAME", "admin"),
                &env_or("ADMIN_PASSWORD", "changeme-admin"),
            )
            .await
        }
        "seed-e2e" => tower_admin::seed_e2e(&db).await,
        "bootstrap" => {
            // The compose one-shot: migrate + provision + seed the go-live admin, in order.
            match tower_admin::init(
                &db,
                &env_or("APP_ROLE_PASSWORD", "app_role_dev_only"),
                &env_or("RETENTION_ROLE_PASSWORD", "retention_role_dev_only"),
            )
            .await
            {
                Ok(()) => {
                    tower_admin::seed_admin(
                        &db,
                        &env_or("TENANT_SLUG", "tower-local"),
                        &env_or("ADMIN_USERNAME", "admin"),
                        &env_or("ADMIN_PASSWORD", "changeme-admin"),
                    )
                    .await
                }
                err => err,
            }
        }
        other => {
            eprintln!("tower-admin: unknown subcommand {other:?}; use: init|seed-admin|seed-e2e|bootstrap");
            exit(2);
        }
    };
```

- [ ] **Step 5: Run to verify it passes + clippy**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p tower-admin 2>&1 | tail -15`
Expected: PASS — all four tests.

Run: `cd Backend && cargo clippy -p tower-admin --all-targets -- -D warnings 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add Backend/bin/tower-admin/src Backend/bin/tower-admin/tests
git commit -m "feat(tower-admin): seed-admin + seed-e2e + bootstrap CLI (Fase 8)"
```

---

### Task 3: `tower-admin` Docker image + `.env.example` documentation

**Files:**
- Create: `Docker/tower-admin.Dockerfile`
- Modify: `Docker/.env.example`

**Interfaces:**
- Produces: a `tower-admin` image (entrypoint `/usr/local/bin/tower-admin`) and documented env vars.

**Context:** `tower-admin` depends on `spx-client` (for `hash_password`), whose `wreq`/BoringSSL build needs `cmake` — so this Dockerfile mirrors `reactor-core.Dockerfile`'s builder (WITH cmake), NOT `retention.Dockerfile`. It is one-shot, so the runtime needs no `curl`/healthcheck and no `EXPOSE`.

- [ ] **Step 1: Read `Docker/reactor-core.Dockerfile`** to copy its exact builder stage (base image, cmake install, COPY layout, build command shape).

Run: `cat Docker/reactor-core.Dockerfile`

- [ ] **Step 2: Write `Docker/tower-admin.Dockerfile`** — mirror `reactor-core.Dockerfile`'s builder stage exactly (same `rust:1-slim-bookworm`, the `cmake` apt install, the `COPY Backend/Cargo.toml Backend/Cargo.lock ./` + `COPY Backend/crates ./crates` + `COPY Backend/bin ./bin` layout), building `--package tower-admin`. Runtime stage: `debian:bookworm-slim`, create the non-root `tower` user (same `useradd` line), copy the binary, `USER tower`, `ENTRYPOINT ["/usr/local/bin/tower-admin"]`. Do NOT add `curl`/`ca-certificates`/`EXPOSE` (one-shot, no HTTP, no outbound HTTPS). Use the reactor-core Dockerfile's exact base-image tags — do not invent versions.

```dockerfile
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
# cmake: tower-admin depends on spx-client (hash_password), whose wreq dep vendors BoringSSL
# (btls-sys) and needs cmake at build time — same reason reactor-core.Dockerfile installs it.
RUN apt-get update && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
COPY Backend/Cargo.toml Backend/Cargo.lock ./
COPY Backend/crates ./crates
COPY Backend/bin ./bin
RUN cargo build --release --package tower-admin

FROM debian:bookworm-slim AS runtime
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/target/release/tower-admin /usr/local/bin/tower-admin
USER tower
ENTRYPOINT ["/usr/local/bin/tower-admin"]
```

- [ ] **Step 3: Add the new env vars to `Docker/.env.example`** — append after the existing `APP_ROLE_PASSWORD` block:

```bash
# The single deployment tenant's slug (reactor-core resolves exactly one tenant from this).
TENANT_SLUG=tower-local

# First-run admin login, created idempotently by `tower-admin seed-admin` (via tower-migrate).
# CHANGE THESE before any non-local use. After first login you can also manage accounts in-app
# at /settings/sub-users. The seed never overwrites an existing admin, so changing the password
# in-app (or here + re-seed after deleting the row) is safe.
ADMIN_USERNAME=admin
ADMIN_PASSWORD=changeme-admin

# Password for the retention_role login (provisioned by tower-admin init). The retention worker
# runs as the `tower` superuser locally today; this is set so a hardened deploy can switch it.
RETENTION_ROLE_PASSWORD=retention_role_dev_only
```

- [ ] **Step 4: Validate the Dockerfile builds**

Run: `docker build -f Docker/tower-admin.Dockerfile -t tower-admin-test .` (from the repo root)
Expected: builds successfully (a Rust release build of `tower-admin` — slow but should succeed with cmake present).

- [ ] **Step 5: Commit**

```bash
git add Docker/tower-admin.Dockerfile Docker/.env.example
git commit -m "feat(docker): tower-admin image + documented go-live env vars (Fase 8)"
```

---

### Task 4: `docker-compose.yml` — `tower-migrate` service + runtime env + gating

**Files:**
- Modify: `Docker/docker-compose.yml`
- Modify: `Docker/.env` (local, gitignored — add the new vars so the acceptance works)

**Interfaces:**
- Consumes: the `tower-admin` image (Task 3), the `.env` vars.
- Produces: a stack where `reactor-core`/`auth-sidecar`/`retention` start only after `tower-migrate` completes and Postgres/Redis are healthy, with their container-network runtime env set.

**Context:** This is the wiring that makes `docker compose up` work. `tower-migrate` runs `bootstrap` (migrate + provision + seed-admin) as the superuser, exits 0; the app services gate on `service_completed_successfully`.

- [ ] **Step 1: Add the `tower-migrate` service** to `Docker/docker-compose.yml` (place it before `tower-reactor-core`):

```yaml
  tower-migrate:
    build:
      context: ..
      dockerfile: Docker/tower-admin.Dockerfile
    container_name: tower-migrate
    restart: "no"
    command: ["bootstrap"]
    env_file:
      - .env
    environment:
      TOWER_ADMIN_DATABASE_URL: postgres://tower:${POSTGRES_PASSWORD:-tower_dev_only}@tower-postgres:5432/tower
    depends_on:
      tower-postgres:
        condition: service_healthy
    networks:
      - tower-net
```

- [ ] **Step 2: Set `tower-reactor-core`'s runtime env + gate it on `tower-migrate`** — in its `environment:` block add (alongside the existing `TOWER_MASTER_KEY_PATH`):

```yaml
    environment:
      TOWER_MASTER_KEY_PATH: /run/secrets/tower_master_key
      DATABASE_URL: postgres://app_role:${APP_ROLE_PASSWORD:-app_role_dev_only}@tower-postgres:5432/tower
      REDIS_URL: redis://tower-redis:6379
      TENANT_SLUG: ${TENANT_SLUG:-tower-local}
      AUTH_SIDECAR_URL: http://tower-auth-sidecar:8082
```

and replace its `depends_on:` (currently the list `[tower-postgres, tower-redis]`) with:

```yaml
    depends_on:
      tower-migrate:
        condition: service_completed_successfully
      tower-postgres:
        condition: service_healthy
      tower-redis:
        condition: service_healthy
```

- [ ] **Step 3: Set `tower-auth-sidecar`'s runtime env + gate** — add to its `environment:` block:

```yaml
    environment:
      TOWER_MASTER_KEY_PATH: /run/secrets/tower_master_key
      DATABASE_URL: postgres://app_role:${APP_ROLE_PASSWORD:-app_role_dev_only}@tower-postgres:5432/tower
      REDIS_URL: redis://tower-redis:6379
      TENANT_SLUG: ${TENANT_SLUG:-tower-local}
```

and add a `depends_on:` block (it currently has none):

```yaml
    depends_on:
      tower-migrate:
        condition: service_completed_successfully
      tower-postgres:
        condition: service_healthy
      tower-redis:
        condition: service_healthy
```

(If `auth-sidecar` doesn't actually read `DATABASE_URL`/`TENANT_SLUG`, setting them is harmless; verify against `bin/auth-sidecar/src/main.rs` and drop any it ignores.)

- [ ] **Step 4: Gate `tower-retention` on `tower-migrate`** — change its `depends_on:` (currently `tower-postgres: service_healthy`) to also require the migration:

```yaml
    depends_on:
      tower-migrate:
        condition: service_completed_successfully
      tower-postgres:
        condition: service_healthy
```

- [ ] **Step 5: Add the new vars to the local `Docker/.env`** (gitignored — needed for interpolation + `tower-migrate`'s `env_file`). Ensure `Docker/.env` contains:

```bash
POSTGRES_PASSWORD=tower_dev_only
APP_ROLE_PASSWORD=app_role_dev_only
RETENTION_ROLE_PASSWORD=retention_role_dev_only
TENANT_SLUG=tower-local
ADMIN_USERNAME=admin
ADMIN_PASSWORD=changeme-admin
RUST_LOG=info
```

- [ ] **Step 6: Validate the compose file parses**

Run: `docker compose -f Docker/docker-compose.yml config >/dev/null && echo "compose OK"`
Expected: `compose OK`.

- [ ] **Step 7: Commit**

```bash
git add Docker/docker-compose.yml
git commit -m "feat(docker): tower-migrate one-shot + app_role runtime env + startup gating (Fase 8)"
```

---

### Task 5: `docker-compose.prod.yml` — Traefik/ACME VPS overlay (config-only)

**Files:**
- Create: `Docker/docker-compose.prod.yml`

**Interfaces:**
- Produces: a ready overlay (`-f docker-compose.yml -f docker-compose.prod.yml`) that swaps Caddy→Traefik+ACME, joins an external network, and label-routes to reactor-core/web. Zero code change. NOT VPS-tested this phase.

**Context:** Per master-spec DoD #6 and the Caddyfile's own note ("so the VPS Traefik overlay can mirror this list verbatim"). The overlay only overrides/adds services; it changes no application code or image.

- [ ] **Step 1: Read the Caddyfile's `@backend` path list** — the exact routes reactor-core owns (so Traefik labels mirror them):

Run: `grep -A3 '@backend' Docker/Caddyfile`

- [ ] **Step 2: Write `Docker/docker-compose.prod.yml`** — an overlay that: (a) defines an `external: true` shared network `web` (for a shared Traefik); (b) replaces `tower-caddy` with a `tower-traefik` service (image `traefik:v3`, ACME TLS-challenge resolver, ports 80/443, docker provider) OR relies on an already-running shared Traefik and just adds routing labels to `tower-reactor-core`/`tower-web`; (c) adds Traefik labels routing the Caddyfile's `@backend` paths to `tower-reactor-core:8081` and everything else to `tower-web:3000`, using a `${TOWER_DOMAIN}` env var for the host rule. Include a header comment that this is un-VPS-tested and lists the exact `docker compose -f ... -f docker-compose.prod.yml up` invocation + the `TOWER_DOMAIN`/ACME-email env it needs. Keep it minimal and faithful to the Caddy routing; do not restructure the base services.

(Write the concrete YAML mirroring the base compose's service names and the Caddyfile route list. Because it is config-only and un-VPS-tested, its acceptance is `docker compose config` validity, not a live run.)

- [ ] **Step 3: Validate the overlay parses (merged with the base)**

Run: `TOWER_DOMAIN=tower.example.com docker compose -f Docker/docker-compose.yml -f Docker/docker-compose.prod.yml config >/dev/null && echo "overlay OK"`
Expected: `overlay OK` (valid merged config; no live containers started).

- [ ] **Step 4: Commit**

```bash
git add Docker/docker-compose.prod.yml
git commit -m "feat(docker): docker-compose.prod.yml Traefik/ACME VPS overlay (config-only, Fase 8)"
```

---

### Task 6: Acceptance — clean `docker compose up` → login works + final gates

**Files:**
- Create: `Docker/smoke.sh` (a documented, runnable local-go-live acceptance script)

**Interfaces:**
- Consumes: everything above.
- Produces: proof that a fresh `docker compose up` is a working local go-live.

**Context:** This is the phase's whole point. It brings the full stack up from a clean state and asserts the operator can log in. It rebuilds all images and drops volumes, so it is a manual/plan acceptance step, not part of `cargo test`.

- [ ] **Step 1: Write `Docker/smoke.sh`**

```bash
#!/usr/bin/env bash
# Fase 8-Deploy-lokal acceptance: a fresh `docker compose up` is a working local go-live.
# Drops volumes + rebuilds images, so it is slow and destructive to local dev data — intended
# for verifying the stack end-to-end, not routine use.
set -euo pipefail
cd "$(dirname "$0")"

echo "== tearing down (including volumes) =="
docker compose -f docker-compose.yml down -v || true

echo "== building + starting the full stack =="
docker compose -f docker-compose.yml up -d --build

echo "== waiting for reactor-core to become healthy =="
for i in $(seq 1 60); do
  status=$(docker inspect -f '{{.State.Health.Status}}' tower-reactor-core 2>/dev/null || echo starting)
  if [ "$status" = "healthy" ]; then echo "reactor-core healthy"; break; fi
  sleep 3
done

echo "== healthz via Caddy edge =="
code=$(curl -s -o /dev/null -w '%{http_code}' http://localhost:8080/healthz)
echo "GET /healthz -> $code"; [ "$code" = "200" ] || { echo "FAIL healthz"; exit 1; }

echo "== admin login via Caddy edge =="
ADMIN_USERNAME=${ADMIN_USERNAME:-admin}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-changeme-admin}
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST http://localhost:8080/auth/portal-login \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$ADMIN_USERNAME\",\"password\":\"$ADMIN_PASSWORD\"}")
echo "POST /auth/portal-login -> $code"; [ "$code" = "200" ] || { echo "FAIL login"; exit 1; }

echo "== frontend served =="
code=$(curl -s -o /dev/null -w '%{http_code}' http://localhost:8080/)
echo "GET / -> $code"; [ "$code" = "200" ] || { echo "FAIL frontend"; exit 1; }

echo "== ALL SMOKE CHECKS PASSED — local go-live works =="
```

Make it executable: `chmod +x Docker/smoke.sh`.

- [ ] **Step 2: Run the acceptance smoke** (SLOW — builds all images; run foreground, wait)

Run: `bash Docker/smoke.sh 2>&1 | tail -30`
Expected: ends with `ALL SMOKE CHECKS PASSED`. If a service is unhealthy, inspect `docker compose -f Docker/docker-compose.yml logs tower-migrate tower-reactor-core --tail=80` and fix the root cause. **Known risk to watch for:** this acceptance is the FIRST time `reactor-core` actually runs as `app_role` end-to-end — every `reactor-core` boot-smoke test uses the `tower` superuser, so an `app_role` grant gap (e.g. a table `app_role` was never `GRANT`ed on, or an RLS policy that excludes it) has never been exercised and would surface here as `permission denied for table/relation …` (SQLSTATE 42501) in the `reactor-core` logs during boot or on the first login/query. The correct fix is a new forward-only grant migration (mirroring `0016`/`0017`'s pattern), NOT switching `reactor-core` back to the superuser. Other common causes: a missing/mismatched `.env` var, or `tower-migrate` itself erroring (its logs show the exact `tower-admin` failure). Do NOT mark done until this passes with `reactor-core` running as `app_role`.

- [ ] **Step 3: Restore the dev DB for the rest of the suite** — the smoke's `down -v` wiped the dev volume and the compose stack re-seeded `tower-local`/admin (not the e2e users). Bring back host infra + e2e fixtures for `cargo test`/Playwright:

Run:
```
docker compose -f Docker/docker-compose.yml up -d tower-postgres tower-redis
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo run -p tower-admin -- init
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo run -p tower-admin -- seed-e2e
```
Expected: `tower-admin init: ok` and `seed-e2e: … ready`. (This is also the documented dev workflow that replaces the old ad-hoc `#[ignore]` seed.)

- [ ] **Step 4: Final workspace + frontend gates**

Run (foreground):
```
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test --workspace --exclude reactor-core
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test -p reactor-core --bin reactor-core -- --test-threads=1
cd Backend && cargo clippy --workspace --all-targets -- -D warnings
cd Backend && cargo deny check
cd Frontend && pnpm check && pnpm vitest run && pnpm build
```
Expected: all green (`tower-admin` tests included; reactor-core bin single-threaded for the known `ALTER ROLE app_role` parallel-catalog flake — now doubly relevant since `tower-admin` also `ALTER ROLE`s, so its tests are `#[serial]`).

- [ ] **Step 5: Commit**

```bash
git add Docker/smoke.sh
git commit -m "test(fase-8-deploy): docker compose up acceptance smoke — login works end-to-end (Fase 8)"
```

---

## Self-Review Notes

- **Spec coverage:** the four gaps (migrations never run / unset container env / app_role no password / no seed) map to Task 1 (migrate + app_role password), Task 2 (seed tenant+admin), Task 4 (container env + gating). `tower-admin` binary + Docker image (Tasks 1–3), one-shot `tower-migrate` gating (Task 4), `docker-compose.prod.yml` overlay (Task 5), committed e2e seed (Task 2 `seed-e2e` + Task 6 Step 3 usage), retention_role password provisioning (Task 1), the working-`docker compose up` acceptance with a real login (Task 6). Every design section maps to a task.
- **Deferred (per design, tracked):** wiring the retention worker's DSN to `retention_role` under RLS (only its password is provisioned here); VPS-testing the overlay; production secret injection.
- **Placeholder scan:** every code step has complete code; Task 5 Step 2 is a "write the concrete YAML mirroring X" instruction (the overlay is config whose exact shape depends on the read-first Caddy route list + whether a shared Traefik is assumed) — it names the exact inputs (Caddy `@backend` list, `${TOWER_DOMAIN}`, service names) and the acceptance (`docker compose config` validity), so it is a bounded write-from-inputs step, not a vague placeholder; the reviewer checks the resulting file against the Caddy routes.
- **Type/interface consistency:** `init`/`seed_admin`/`seed_e2e`/`ensure_tenant`/`ensure_user` signatures are defined in Task 1–2 and used identically in `main.rs` and the tests; env-var names (`APP_ROLE_PASSWORD`, `TENANT_SLUG`, `ADMIN_USERNAME`, `ADMIN_PASSWORD`, `RETENTION_ROLE_PASSWORD`, `TOWER_ADMIN_DATABASE_URL`) are identical across `main.rs`, `.env.example`, `.env`, and the compose `environment:` blocks.
- **Environment caveats carried:** infra must be up for backend tests; `tower-admin` tests are `#[serial]` (they `ALTER ROLE`/seed shared global state); reactor-core bin needs `--test-threads=1`; the acceptance smoke wipes the dev volume (Task 6 Step 3 restores it + e2e fixtures).
- **Aturan Keras #5 check:** no real secret committed — `.env.example` carries dev defaults only; `.env` is gitignored; migrations set no password (the binary does, from env).
