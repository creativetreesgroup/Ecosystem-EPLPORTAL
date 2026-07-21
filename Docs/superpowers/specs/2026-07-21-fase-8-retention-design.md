# Fase 8-Retention — Data Retention & Archival Worker Design

**Status:** approved (brainstorming, 2026-07-21)
**Scope:** the first sub-phase of Fase 8. A daily data-retention worker that archives then deletes aged rows from the growth tables (`bookings`, `accept_events`, `notifications`), following the master spec's exact safe algorithm (Aturan Keras #7: *delete by captured-id set ONLY* — the lesson from a real delete-wrong-rows incident). Local-first (Docker Compose); nothing external required.

This is decomposition sub-phase **8-Retention** of Fase 8 (Cutover parity + observability + hardening). Fase 8 was decomposed during brainstorming into: **8-Retention** (this), 8-Deploy-lokal (`docker compose up` end-to-end + `app_role` provisioning + committed e2e seed + `docker-compose.prod.yml` overlay), 8-Observability (SLO dashboard + alerts + CI decision-path bench + security-scan CI gates), 8-Debt-cleanup (7b WS producers, 7e price sign-validation, rate-limiter tuning, real error messages), 8-Cutover (observe-only diff-vs-TS infra — arm-gate deferred to a manual go-live gate pending the reference TS engine), and 8-Soak (7-day soak — a wall-clock go-live gate). Order chosen: 8-Retention first (most isolated, most testable, satisfies a hard rule with a real incident behind it).

---

## Why this is a Rust worker, not pg_cron

The master spec line 188 phrases the scheduler as "pg_cron 30 3 * * *", but the same spec also ships a dedicated `tower-retention` container (docker-compose) and lists retention as a CONTROL-plane component (line 137). This design deliberately implements the retention **logic** as a testable Rust binary (`bin/retention`) that **self-schedules** to 03:30 daily, and does **not** use the pg_cron extension. Rationale, in order of weight:

1. **The delete/verify pipeline is security-critical and must be unit/integration-testable.** Aturan Keras #7 exists because a real incident deleted the wrong rows. The single most important guarantee — *delete exactly the captured id set, never a re-evaluated time predicate* — must be proven by an automated test. That test is natural in Rust and awkward-to-impossible in plpgsql.
2. **Keeps the archive+verify+delete logic in reviewed Rust**, not split between plpgsql and `COPY TO PROGRAM` shell-outs (gzip/sha256 in pure SQL requires superuser `COPY TO PROGRAM`, which is fragile and unreviewable).
3. **No new Postgres extension dependency.** pg_cron is not enabled by default in `postgres:16` and would need image/config changes; the worker needs only a role and a volume.
4. **The `tower-retention` container already exists** (today an `alpine:3` no-op placeholder) to host exactly this worker.

The worker computes the next 03:30 (local time, configurable) and sleeps until then; a Postgres session-level advisory lock (`pg_try_advisory_lock`) guarantees a single runner even across container restarts or an accidental second instance. This honors the spec's intent (a daily scheduled retention job running the exact capture→archive→verify→delete-by-id→VACUUM algorithm) while being far more testable and dependency-light. *(If a future maintainer wants literal pg_cron, the worker's `run_once()` entrypoint is directly callable and the swap is mechanical — but the tested delete path stays in Rust regardless.)*

---

## Target tables and retention windows

Three growth tables, each aged by its `created_at` column, each window env-configurable (defaults chosen conservatively; all can be tuned without a code change):

| Table | Env var | Default | Notes |
|---|---|---|---|
| `bookings` | `RETENTION_BOOKINGS_DAYS` | 90 | Aged by `created_at`. No status filter — retention is age-based archival; stale-pending is a separate anti-drift concern already handled by the poller. |
| `accept_events` | `RETENTION_ACCEPT_EVENTS_DAYS` | 180 | Append-only audit trail — kept longest. Requires a delete-capable role (see below). |
| `notifications` | `RETENTION_NOTIFICATIONS_DAYS` | 30 | Aged by `created_at`. |

A window of `0` disables retention for that table (skip entirely). The table list is fixed in code (these three), not env-driven — adding a table is a reviewed code change, not a config toggle, since each table's delete is a real data-loss operation.

---

## The delete-capable role (`retention_role`)

`accept_events` migration `0008` does `REVOKE UPDATE, DELETE ON accept_events FROM app_role` (append-only enforcement). So the retention worker **cannot** delete `accept_events` as `app_role`. Running it as the `tower` owner/superuser would work but is over-broad.

New migration `0022_retention_role.sql` creates a least-privilege `retention_role NOLOGIN` granted **only** `SELECT, DELETE` on `bookings`, `accept_events`, `notifications` (and `SELECT, INSERT, UPDATE` on `archive_runs` to record runs). The worker connects with a login role that has been `GRANT`ed `retention_role`, or as a dedicated `retention_user` login mapped to it. For local dev, the worker's `DATABASE_URL` uses the `tower` superuser (which already holds every privilege) — the dedicated role is what a hardened deploy uses, and the migration makes it exist. The migration is forward-only and idempotent (guards `IF NOT EXISTS` on the role, matching `0008`'s pattern).

Note: `retention_role` needs privileges that let it DELETE rows subject to RLS. Retention is a system-wide maintenance operation, not tenant-scoped (like `archive_runs` itself, which has no RLS). The worker deletes across all tenants by primary key; it sets no `app.tenant_id`. `bookings`/`accept_events`/`notifications` have RLS enabled — the migration grants `retention_role` `BYPASSRLS` is **not** used (superuser-only attribute risk); instead the DELETE uses the captured PK set and `retention_role` is added to each table's RLS policy as an allowed maintenance role, OR (simpler and preferred) the worker runs its deletes in a transaction that `SET LOCAL row_security = off` — which requires the role to be a table owner or superuser. **Decision:** for local-first, the worker connects as `tower`, which is a **superuser** and therefore bypasses RLS unconditionally — note the three target tables use `FORCE ROW LEVEL SECURITY` (`0016_rls_policies.sql`), under which even the *table owner* is NOT exempt; only the superuser attribute (or `BYPASSRLS`) sees all tenants' rows. `0022` creates `retention_role` + grants for the hardened-deploy path; wiring the worker to run as `retention_role` under RLS is explicitly deferred to 8-Deploy-lokal (where the `app_role`/role-provisioning story is settled end-to-end). This keeps 8-Retention's RLS-vs-maintenance-role question from bleeding into its core deliverable while still shipping the role. This deferral is recorded in the tracked-follow-ups section.

---

## The safe algorithm (the heart of the phase)

Per table, in a single worker cycle:

```
acquire pg_try_advisory_lock(RETENTION_ADVISORY_KEY)   -- one integer key for the whole worker
  if not acquired: log "another retention run holds the lock, skipping", exit 0

for table in [bookings, accept_events, notifications] where window_days > 0:
    cutoff = now() - (window_days days)
    run_id = INSERT archive_runs(table_name, captured_count=0, archived_count=0,
                                 deleted_count=0, status='running', dry_run) RETURNING id

    -- 1. CAPTURE ONCE: the exact PK set to be removed, materialized now.
    captured_ids: Vec<Uuid> = SELECT <pk> FROM <table> WHERE created_at < cutoff
    UPDATE archive_runs SET captured_count = len(captured_ids)

    if captured_ids is empty:
        UPDATE archive_runs SET status='completed'   -- nothing to do
        continue

    -- 2. ARCHIVE those exact rows to CSV.gz, computing sha256 incrementally.
    path = <ARCHIVE_DIR>/<table>_<YYYYMMDD_HHMMSS>.csv.gz
    archived_count = stream (SELECT * FROM <table> WHERE <pk> = ANY(captured_ids)) -> gzip(csv) -> file
    sha256 = hash of the gzip bytes as written
    UPDATE archive_runs SET archived_count, archive_path=path, sha256

    -- 3. VERIFY before any delete. A mismatch aborts THIS table with no delete.
    if archived_count != len(captured_ids):
        UPDATE archive_runs SET status='failed'
        log error; continue   -- never delete when the archive is incomplete

    -- 4. DELETE by captured id set ONLY, in batches. NEVER re-derive from the time predicate.
    if dry_run:
        UPDATE archive_runs SET deleted_count=0, status='completed'   -- log what WOULD be deleted
        continue
    deleted = 0
    for batch in captured_ids.chunks(DELETE_BATCH=5000):
        n = DELETE FROM <table> WHERE <pk> = ANY(batch)   -- exact ids, tenant-agnostic
        deleted += n
    VACUUM <table>       -- reclaim space (cannot run inside the delete txn)
    UPDATE archive_runs SET deleted_count=deleted, status='completed'

release advisory lock
```

**The load-bearing invariant:** step 4 deletes `WHERE pk = ANY(captured_ids)`, using the set materialized in step 1 — it never re-runs `WHERE created_at < cutoff`. Rows inserted between capture and delete (a booking created at 03:30:02 when the run started at 03:30:00) are **not** in `captured_ids`, were **not** archived, and therefore are **not** deleted. Re-deriving the delete from the time predicate would delete those un-archived rows — that is the incident this rule prevents.

Secondary invariants:
- Archive is durable and count-verified **before** the first DELETE. A crash between archive and delete leaves rows intact (safe — a re-run re-captures and re-archives; at worst a duplicate archive file, never a lost-but-unarchived row).
- `VACUUM` runs after the batched deletes, outside any transaction (VACUUM cannot run in a transaction block).
- Each table is independent: a failure archiving `bookings` does not block `notifications`.

---

## DRY_RUN default ON

`RETENTION_DRY_RUN` defaults to `true`. In dry-run the worker captures, archives, verifies, and records `archive_runs` with `dry_run=true, deleted_count=0` — it logs exactly how many rows each table *would* delete, but deletes nothing. This mirrors the project's "gate closed until proven" discipline: a fresh deployment runs dry until an operator inspects the `archive_runs` history and the archive files, then explicitly arms it with `RETENTION_DRY_RUN=false`. The archive files ARE written in dry-run (so the operator can verify archive integrity before trusting the delete path).

---

## Configuration (env)

| Var | Default | Meaning |
|---|---|---|
| `RETENTION_DRY_RUN` | `true` | When true, archive + verify but never delete. |
| `RETENTION_SCHEDULE_HOUR` | `3` | Local hour to run (0–23). |
| `RETENTION_SCHEDULE_MINUTE` | `30` | Local minute to run (0–59). |
| `RETENTION_BOOKINGS_DAYS` | `90` | Age cutoff for `bookings`; `0` disables. |
| `RETENTION_ACCEPT_EVENTS_DAYS` | `180` | Age cutoff for `accept_events`; `0` disables. |
| `RETENTION_NOTIFICATIONS_DAYS` | `30` | Age cutoff for `notifications`; `0` disables. |
| `RETENTION_ARCHIVE_DIR` | `/archive` | Directory for `*.csv.gz` archive files (a mounted volume in compose). |
| `RETENTION_DELETE_BATCH` | `5000` | Rows per DELETE batch. |
| `DATABASE_URL` | — | Connection (local dev: the `tower` owner; hardened deploy: a `retention_role` login). |
| `RETENTION_RUN_ONCE` | `false` | When true, run one cycle immediately and exit (for CI/manual runs); when false, self-schedule the daily loop. |

---

## Component structure

- **`Backend/crates/store/src/retention.rs`** (new module in the existing `store` crate) — the pure DB-facing pieces, each independently testable: `capture_ids(pool, table, cutoff) -> Vec<Uuid>`, `stream_archive(pool, table, ids, writer) -> archived_count` (writes gzipped CSV to any `io::Write`, returns the row count), `delete_by_ids(pool, table, ids, batch) -> deleted_count`, `vacuum(pool, table)`, and the `archive_runs` insert/update helpers. A `RetentionTable` enum (`Bookings`/`AcceptEvents`/`Notifications`) maps to its table name and PK column so no table name is ever string-interpolated from outside this enum (no SQL-injection surface; the enum's `&'static str` table names are the only interpolated identifiers).
- **`Backend/bin/retention/`** (new workspace binary) — `main.rs`: reads env config, connects, and either runs one cycle (`RETENTION_RUN_ONCE`) or loops (compute next scheduled instant, sleep, run cycle, repeat). The per-cycle orchestration (advisory lock, per-table loop, dry-run branch) lives in a `run_cycle(pool, config)` function that both the loop and the tests call.
- **`Backend/crates/store/migrations/0022_retention_role.sql`** — creates `retention_role NOLOGIN` (idempotent) with least-privilege grants.
- **`Docker/retention.Dockerfile`** + **`Docker/docker-compose.yml`** `tower-retention` service — swap the `alpine:3` no-op for the built `retention` binary, mounting an `archive` volume at `RETENTION_ARCHIVE_DIR`. (Compose wiring kept minimal here; the full `docker compose up` end-to-end story is 8-Deploy-lokal.)

Files that change together (the store module + its migration + the binary that drives it) stay together in the `store` crate and a thin binary, matching the existing `reactor-core`/`store` split.

---

## Testing

Integration tests (`Backend/crates/store/tests/retention_pg.rs` and/or `bin/retention` tests) against real Postgres (`tower` superuser URL, the project's standard test DB), each seeding its own uniquely-named tenant and rows so they are self-cleaning and parallel-safe:

1. **capture correctness** — seed N rows older than cutoff + M rows newer; `capture_ids` returns exactly the N old PKs.
2. **archive integrity** — after `stream_archive`, the `.csv.gz` file exists, gunzips to exactly `captured_count` data rows, and the recorded sha256 matches a fresh hash of the file bytes.
3. **DRY_RUN deletes nothing** — a full `run_cycle` with `dry_run=true` leaves all rows present, writes the archive, and records `archive_runs(dry_run=true, deleted_count=0)`.
4. **THE INCIDENT-PREVENTION TEST (most important)** — seed set A: rows with an explicit `created_at` before the cutoff. Call `capture_ids` → it returns exactly set A. Now, *before* the delete step, seed set B: MORE rows, also with an explicit `created_at` before the cutoff (so they would match the time predicate) but created after the capture and therefore NOT in the captured id set. Run `delete_by_ids` against the captured set (A). Assert: every row in A is gone, and **every row in B survives**. This proves the delete targets the captured id set, never a re-evaluated `WHERE created_at < cutoff` (which would have wrongly deleted the un-archived set B). This is the exact incident Aturan Keras #7 forbids.
5. **verify-mismatch aborts** — force `archived_count != captured_count` (e.g., archive a truncated set) and assert `run_cycle` records `status='failed'` and deletes nothing.
6. **advisory lock single-runner** — hold the advisory lock on one connection, assert a concurrent `run_cycle` acquires nothing and skips (deletes nothing) rather than double-running.
7. **empty-set no-op** — a table with no aged rows records `status='completed'` with zero counts and writes no archive file.
8. **window=0 disables** — `RETENTION_*_DAYS=0` skips that table entirely.
9. **batch boundary** — with `DELETE_BATCH` smaller than the captured set, all captured rows are still deleted across multiple batches (off-by-one guard).

`accept_events` append-only note: tests that delete from `accept_events` connect as the `tower` owner (the test URL already does), matching the local-dev role decision above.

---

## Global constraints inherited

- Aturan Keras #7: delete by captured-id set only. (The load-bearing invariant above.)
- Aturan Keras #5: no plaintext secrets anywhere. The archived tables — `bookings.raw_data` (jsonb) could contain SPX payload but no credentials; `accept_events`/`notifications` carry no secrets. The archive is CSV of these tables as-is; **no `agency_credentials` or `site_settings` (which hold ciphertext) are ever a retention target.** Confirmed: the three target tables contain no encrypted-secret columns.
- Forward-only migrations (`0022`), idempotent role creation, matching the `0008`/`0019` role-migration patterns.
- `cargo clippy -D warnings`, `cargo test`, `cargo deny` all green; the new binary and module carry no new dependency that `deny` would reject (gzip via the `flate2` crate — already a common, license-clean dep; confirm it's acceptable or use `async-compression`/existing dep during planning).

---

## Open Questions for the Implementer

None blocking. Two items to verify during planning (not ambiguities in intent, just facts to confirm against source): (1) the exact gzip crate already in the workspace tree, if any, to avoid adding a redundant compression dependency (`flate2` is the default recommendation if none exists); (2) that `bookings`, `accept_events`, and `notifications` each expose a `created_at TIMESTAMPTZ` column and a UUID primary key named `id` (spot-checked as true for all three, but the implementer confirms against the migrations before writing `RetentionTable`'s column mapping).

## Tracked, deliberately-deferred follow-ups

- **Running the worker as `retention_role` under RLS** is deferred to **8-Deploy-lokal**, where the whole role-provisioning story (including the `app_role` login-password gap) is settled end-to-end. `0022` ships the least-privilege role so it exists; wiring the worker's runtime `DATABASE_URL` to it is that phase's concern. **Important precondition (from the security review):** as granted today, `retention_role` is NOT in any table's RLS policy and has no `BYPASSRLS`/superuser, so under `FORCE ROW LEVEL SECURITY` its `capture_ids`/`DELETE` would match ZERO rows (fail-closed, safe) and `vacuum()` would raise permission-denied. Before that wiring, 8-Deploy-lokal must give the role a maintenance path: either add it to a maintenance RLS policy (or grant PG16 `MAINTAIN` + run with `row_security=off`, which requires ownership or the bypass), and grant it the ability to VACUUM the three tables. Until then the worker MUST keep connecting as `tower` (superuser).
- **`docker compose up` including a healthy, scheduled retention container end-to-end** is validated in 8-Deploy-lokal; 8-Retention proves the worker via `cargo run -p retention` + `RETENTION_RUN_ONCE=true` and its integration tests. (The `/archive` volume ownership was already handled here — the Dockerfile `mkdir -p /archive && chown tower:tower /archive` before `USER tower` — so the non-root container can write archives even under the default `DRY_RUN=true`.)
- **Retention of other tables** (e.g., `portal_sessions` expiry cleanup) is out of scope — session expiry is handled at auth time, not by this batch job.
- **Archive encryption + rotation/pruning** (security-review Informational): the `*.csv.gz` archives contain business PII (`bookings.raw_data`, `notifications` payloads — no auth secrets) and are written unencrypted to a local named volume, and are never pruned (unbounded growth). Acceptable for local-first; a hardening item for a non-local deploy (encrypt-at-rest + an archive-retention sweep).

## Review outcomes (whole-branch quality + security, both opus)

Both returned **Ready to merge: Yes**, zero data-safety violations. The safety-critical core was verified end-to-end: delete targets only the once-captured PK set (never a re-run time predicate — proven by `delete_by_ids_removes_only_captured_and_spares_later_inserts`), the delete is hard-gated on `archived == captured`, DRY_RUN is default-ON and typo-resistant, and every misconfig fails closed (crash/skip, never over-delete). Fixes applied before merge:
- **(Important) Per-table independence on error paths:** `run_all_tables` used `?` per table, so a real error aborted the whole cycle (only the verify-mismatch branch honored the "tables are independent" invariant) and left `archive_runs` stuck at `status='running'`. Refactored into `run_one_table` that marks its run `failed` on any error and returns it; the caller records a `Failed` outcome and continues. New test `a_failing_table_does_not_abort_the_cycle` proves it (forces an archive IO error, asserts the cycle returns `Ok` with the sibling table still attempted, and the row survives).
- **(Important) Docker `/archive` write permission:** the non-root `tower` container couldn't write the root-owned volume → fixed with `mkdir -p /archive && chown tower:tower /archive` before `USER tower`.
- **(Minor) Verify snapshot:** the archive transaction now runs at `REPEATABLE READ` so the row COUNT and the COPY share one snapshot (a concurrent `tenants ON DELETE CASCADE` can no longer let a short archive pass the verify gate).
- **(Minor) Removed unused `RetentionTable::ALL`.**
- **(Security Low) Clamped `days`** to a sane ceiling so an absurd env value can't overflow/panic `chrono::Duration::days` (fail-closed either way).
- **(Doc) Corrected the RLS-exemption rationale** above (superuser bypasses `FORCE` RLS; owner does not).
- **Not fixed (accepted):** ignored `pg_advisory_unlock` result (security Low — fail-closed liveness-only; the `store` crate has no logging infra and adding `tracing` for one line is disproportionate; the `NX`-style single-runner guarantee is unaffected).
