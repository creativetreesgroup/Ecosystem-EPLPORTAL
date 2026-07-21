# Fase 8-Deploy-lokal — Working Local `docker compose up` Go-Live Design

**Status:** approved (brainstorming, 2026-07-21)
**Scope:** the second sub-phase of Fase 8. Make `docker compose up` bring the WHOLE TOWER stack up healthy end-to-end on a fresh machine, so the operator can log in at `http://localhost:8080` and use all 7 surfaces — a real **local** go-live they can run and revise. This closes master-spec DoD #6 ("`docker compose up` lokal end-to-end jalan; overlay VPS Traefik tanpa perubahan kode") for the local half, and ships the VPS overlay as a ready (un-VPS-tested) config for the other half.

Sub-phase **8-Deploy-lokal** of Fase 8. User direction (2026-07-21): finish the project to a genuinely runnable **local** go-live so it can be revised; production go-live gates (8-Cutover diff-vs-TS, 8-Soak) are explicitly deferred as they need external prerequisites.

---

## The gap: the stack does NOT currently come up

Verified against source:
1. **Migrations never run in the compose stack.** `reactor-core`'s runtime boot (`bin/reactor-core/src/main.rs:100`) only `store::connect`s; every `store::run_migrations` call in that file is under `#[cfg(test)]`. A fresh `tower-postgres` volume therefore has NO schema when `reactor-core` boots.
2. **Container runtime env is unset.** `Docker/.env` has only `POSTGRES_PASSWORD` + `RUST_LOG`. `reactor-core` falls back to its `env_or` defaults: `DATABASE_URL=postgres://app_role:app_role_dev_only@127.0.0.1:15432/tower` (wrong host inside the container network — must be `tower-postgres:5432`) and `TENANT_SLUG=""` (an unresolvable slug is a boot-time panic, by design).
3. **`app_role` has LOGIN but no password.** Migration `0019` deliberately sets no password (Aturan Keras #5 — no secret in git); it must be provisioned out-of-band. Nothing does that in the stack, so `app_role` auth fails.
4. **No tenant/admin is seeded.** `TENANT_SLUG` can't resolve to a row, and there is no account to log in as.

So on a fresh machine, `docker compose up` fails to produce a usable system. This sub-phase fixes all four with an idempotent init step, and adds a first-run seed so the operator can log in immediately.

---

## Architecture: a one-shot `tower-migrate` init service

A new one-shot Compose service `tower-migrate` runs a new operator binary `bin/tower-admin` as the `tower` **superuser**, then exits. `reactor-core`, `auth-sidecar`, and `tower-retention` gate on it via `depends_on: { tower-migrate: { condition: service_completed_successfully } }` (and `tower-postgres`/`tower-redis` healthy). Every step is idempotent, so repeated `docker compose up` is safe.

**Why a one-shot init service, not migrate-at-boot inside `reactor-core`:** `reactor-core` must connect as `app_role` (the whole point of migration `0019` — a superuser silently bypasses RLS, the Fase-2-flagged gap). Running migrations + `ALTER ROLE` + seeding requires superuser, which `reactor-core` must NOT hold at runtime. A separate init service keeps `reactor-core` `app_role`-only while giving the one-time superuser work a clean, run-once home. It also makes the init independently runnable in dev/CI (`docker compose run --rm tower-migrate …`).

### `bin/tower-admin` — the operator/init binary

A small Rust binary with subcommands (mirrors the `reactor-core`/`retention` bin pattern; reuses `store` + `spx-client` crypto):

- **`init`** — the compose init step. In order, idempotently:
  1. `store::run_migrations(&superuser_pool)` — creates/upgrades the schema (incl. `app_role` via `0008`/`0019`, `retention_role` via `0022`). `sqlx` tracks applied migrations, so re-runs are no-ops.
  2. Provision `app_role`'s password from `APP_ROLE_PASSWORD`: `ALTER ROLE app_role PASSWORD '<escaped>'` (single-quotes doubled — DDL can't bind a parameter; the value is operator-set env, and escaping closes the only injection vector). Idempotent (sets the same value every run).
- **`seed-admin`** — idempotently create the go-live tenant + admin. If a tenant with slug `TENANT_SLUG` does not exist, create it; if a `portal_user` `ADMIN_USERNAME` does not exist in it, create it as a `is_main_account=true` user with `hash_password(ADMIN_PASSWORD)` (argon2id, the real verifier's params). Never overwrites an existing admin (so an operator who changed the password keeps it). Prints a one-line "admin ready: <username> @ <slug>" (never the password).
- **`seed-e2e`** — idempotently create the Playwright fixtures: tenant `tower-dev` + `e2e-test-user` (main) / `e2e-readonly-user` (non-main), password `correct-horse-battery-staple`. This is the committed replacement for the ad-hoc `#[ignore]` seed that has bitten every e2e session. **Not run by the production compose up** — invoked by the e2e harness / a documented dev command (`docker compose run --rm tower-migrate seed-e2e`, or `cargo run -p tower-admin -- seed-e2e` against the host DB).

All subcommands take the superuser DSN from `TOWER_ADMIN_DATABASE_URL` (or `DATABASE_URL`), defaulting to the `tower` superuser. The binary is deliberately the ONLY place that connects as superuser at deploy time.

---

## Configuration

Two files, split by concern (secrets/passwords in `.env`; topology in Compose `environment:`):

**`Docker/.env.example`** (committed template; the real `Docker/.env` is gitignored) gains dev-only defaults + documentation:
- `APP_ROLE_PASSWORD=app_role_dev_only` (already documented there; now actually consumed by `tower-migrate`).
- `TENANT_SLUG=tower-local` — the single deployment tenant's slug.
- `ADMIN_USERNAME=admin`, `ADMIN_PASSWORD=changeme-admin` — the first-run admin. Documented as "change these before any non-local use; change the password in-app via /settings/sub-users after first login."

**`Docker/docker-compose.yml`** — `environment:` blocks (topology, not secrets):
- `tower-migrate` (new): `TOWER_ADMIN_DATABASE_URL=postgres://tower:${POSTGRES_PASSWORD}@tower-postgres:5432/tower`, `APP_ROLE_PASSWORD`, `TENANT_SLUG`, `ADMIN_USERNAME`, `ADMIN_PASSWORD` (via `env_file: .env`), command `["init"]` then a second run of `seed-admin` (either two commands via an entrypoint script, or `tower-migrate` runs `init` and `reactor-core` boot is gated on it while a tiny `command: ["sh","-c","tower-admin init && tower-admin seed-admin"]` does both — see plan for the exact shape).
- `reactor-core` / `auth-sidecar`: add `DATABASE_URL=postgres://app_role:${APP_ROLE_PASSWORD}@tower-postgres:5432/tower`, `REDIS_URL=redis://tower-redis:6379`, `TENANT_SLUG`, `AUTH_SIDECAR_URL=http://tower-auth-sidecar:8082`, `SPX_BASE_URL` (its existing default is the real SPX host — fine), and their `depends_on` gains `tower-migrate: service_completed_successfully`.
- `tower-retention`: its `DATABASE_URL` stays the `tower` superuser for local (per 8-Retention's decision); add `depends_on: tower-migrate` so the schema exists first.

**`Docker/docker-compose.prod.yml`** (new, config-only, per DoD #6 "nol perubahan kode"): an overlay applied with `-f docker-compose.yml -f docker-compose.prod.yml` that swaps `tower-caddy` for a Traefik service (ACME/TLS), joins an external shared network, and label-routes `/api /auth /ws → reactor-core, else → web` mirroring the Caddyfile's explicit path list. Shipped as a ready artifact; **not** tested against a real VPS this phase.

---

## retention_role login-password provisioning (bounded slice of 8-Retention's deferral)

Since `tower-migrate` already owns role provisioning, this phase provisions ONE more thing while it's there: `retention_role`'s login password (`RETENTION_ROLE_PASSWORD`, dev default in `.env.example`), via the same idempotent `ALTER ROLE retention_role LOGIN PASSWORD '<escaped>'` in `tower-admin init`. That is the crisp, in-scope slice.

**Explicitly OUT of this sub-phase (kept as a tracked follow-up, unchanged from 8-Retention's deferral):** the RLS-maintenance path that would let `retention_role` actually SELECT/DELETE under `FORCE ROW LEVEL SECURITY` and VACUUM, AND switching the retention worker's runtime `DATABASE_URL` from the `tower` superuser to `retention_role`. For local go-live the worker keeps the `tower` DSN (per 8-Retention's decision), which works today. Doing the RLS-maintenance-policy migration + the DSN switch + proving it end-to-end is a self-contained hardening task that would enlarge this phase's blast radius and its acceptance without advancing the user's actual goal (a runnable local stack). It stays tracked; provisioning the password here just means the role is deploy-ready when that task is picked up.

---

## Testing & acceptance

- **`tower-admin` unit/integration tests** (against real Postgres, `#[serial]` where they touch global tenant/role state): `init` is idempotent (run twice, migrations + `ALTER ROLE` both no-op-safe); `seed-admin` creates the tenant+admin once and does not overwrite on re-run (change the admin's hash, re-run, assert unchanged); `seed-e2e` creates the two e2e users idempotently; the `ALTER ROLE` escaping handles a password containing a single quote.
- **The acceptance smoke — the whole point:** from a clean state (`docker compose down -v` to drop volumes, then `docker compose up -d --build`), assert: all services reach healthy; `curl http://localhost:8080/healthz` (via Caddy → reactor-core) returns 200; a scripted login `POST http://localhost:8080/auth/portal-login` with the seeded admin returns 200 + a session cookie; and `GET http://localhost:8080/` serves the SvelteKit app. This is a documented, runnable acceptance script (`Docker/smoke.sh` or a plan step), proving a fresh-machine `docker compose up` is a working local go-live. (Because it drops volumes and rebuilds images, it is a manual/plan acceptance step, not part of `cargo test`.)
- Existing gates stay green: `cargo test --workspace`, `cargo clippy --workspace -D warnings`, `cargo deny`, frontend `pnpm check`/`vitest`/`build`.

---

## Global constraints inherited

- **Aturan Keras #5** — no plaintext secret in git. Dev-only passwords (`APP_ROLE_PASSWORD`, `ADMIN_PASSWORD`, etc.) live in `.env` (gitignored) with documented defaults in `.env.example`, exactly like the existing `POSTGRES_PASSWORD`. The envelope master key stays a Docker file-secret. No real secret is baked into a committed file or a migration.
- **Aturan Keras #8** — unique container names, single-origin, no published ports except the edge (the 15432/16379 host publishes are the existing dev-tools exception, `127.0.0.1`-scoped). `tower-migrate` publishes nothing.
- **RLS non-bypass** — `reactor-core`/`auth-sidecar` connect as `app_role` (RLS-observing), NEVER `tower`. Only `tower-migrate` (one-shot) uses the superuser.
- Forward-only idempotent migrations; the ops binary is idempotent on every subcommand.
- Reference: master spec `Docs/tower-master-spec.md` (DoD #6, Fase 8 section, Aturan Keras #5/#8), and `Docs/superpowers/specs/2026-07-21-fase-8-retention-design.md` (the retention_role deferral this phase picks up).

---

## Open Questions for the Implementer

None blocking. Confirm during planning: (1) whether `Docker/.env` is gitignored (expected yes; `.env.example` is the committed template) — the plan adds vars to `.env.example` and documents that the operator copies it to `.env`; (2) the exact `store` API for creating a tenant (`store::tenants` — use an existing `create`/`insert` if present, else a raw `INSERT ... ON CONFLICT DO NOTHING`), and `store::portal_users::create` + `spx_client::crypto::password::hash_password` for the admin (both used already by the Fase-7k e2e seed); (3) the two-command shape for the `tower-migrate` service (`init` then `seed-admin`) — an entrypoint that runs both, or the plan's chosen `sh -c` form.

## Tracked, deliberately-deferred follow-ups

- **Retention worker running as `retention_role` under RLS end-to-end** — if the maintenance-RLS-policy migration proves non-trivial, the worker keeps the `tower` DSN for local and the full switch is tracked for a hardened-deploy follow-up (the role + password are provisioned here regardless).
- **VPS overlay validated against a real VPS** — `docker-compose.prod.yml` ships as a ready config; a real VPS bring-up (DNS, ACME issuance, external network) happens only when the operator actually deploys.
- **Production secret management** — dev-default passwords in `.env` are for local go-live; a real deploy injects them from a secret store (documented, not built here).
