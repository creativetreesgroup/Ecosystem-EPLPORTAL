# Fase 8-Retention — Data Retention & Archival Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A daily retention worker that archives-then-deletes aged rows from `bookings`, `accept_events`, `notifications` using the master spec's safe capture→archive→verify→delete-by-captured-id→VACUUM algorithm (Aturan Keras #7).

**Architecture:** DB-facing logic lives in a new `store::retention` module (testable against real Postgres); a thin new `bin/retention` binary reads env config and either runs one cycle (`RETENTION_RUN_ONCE`) or self-schedules a daily loop. A single Postgres session-level advisory lock guarantees one runner. Archiving uses server-side `COPY … TO STDOUT WITH CSV HEADER` streamed through a gzip+SHA-256 hashing writer, so no per-column enumeration and RFC-correct CSV. DRY_RUN defaults ON.

**Tech Stack:** Rust, sqlx 0.9 (Postgres, `copy_out_raw`), `flate2` (gzip), `sha2` (SHA-256), `chrono`, `uuid`, `tokio`.

## Global Constraints

- **Aturan Keras #7 — delete by captured-id set ONLY.** Step 4 deletes `WHERE id = ANY(captured_ids)` using the set materialized in step 1; it NEVER re-runs `WHERE created_at < cutoff`. Rows inserted after capture are not captured, not archived, not deleted.
- **Archive + count-verify BEFORE any delete.** `archived_count == captured_count` is required before the first DELETE; a mismatch aborts that table with `status='failed'` and zero deletes.
- **DRY_RUN defaults ON** (`RETENTION_DRY_RUN=true`). Dry-run archives + verifies + records `archive_runs(dry_run=true, deleted_count=0)` but deletes nothing.
- **VACUUM runs after the batched deletes, outside any transaction.**
- **Target tables are fixed in code** (`bookings`, `accept_events`, `notifications`) via the `RetentionTable` enum — the only source of interpolated table/column identifiers (no injection surface). Never interpolate a table name from outside this enum.
- **No secret tables are ever a retention target** (`agency_credentials`, `site_settings` hold ciphertext — excluded by construction).
- Forward-only, idempotent migration (`0022`), matching `0008`/`0019` role-migration patterns.
- `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check` all green. New deps `flate2`/`sha2` are MIT/Apache — confirm `cargo deny` accepts them.
- All backend commands run with `DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379` (the `tower` superuser — tests run migrations; `app_role` cannot).
- Reference design: `Docs/superpowers/specs/2026-07-21-fase-8-retention-design.md`.

---

### Task 1: Migration `0022_retention_role.sql` — least-privilege delete role

**Files:**
- Create: `Backend/crates/store/migrations/0022_retention_role.sql`

**Interfaces:**
- Produces: a `retention_role NOLOGIN` in the DB with `SELECT, DELETE` on the three target tables and `SELECT, INSERT, UPDATE` on `archive_runs`. No Rust interface. Later tasks connect as the `tower` owner locally (which already holds every privilege); this role exists for the hardened-deploy path (wired in 8-Deploy-lokal).

**Context:** `accept_events` migration `0008` does `REVOKE UPDATE, DELETE ON accept_events FROM app_role`, so the app role cannot delete audit rows. This migration creates a dedicated role that can, without granting it to the app. It must be idempotent (safe to re-run against a cluster where the role already exists), exactly like `0008`'s `DO $$ … pg_roles … $$` guard.

- [ ] **Step 1: Write the migration**

```sql
-- 0022_retention_role.sql
-- Least-privilege role for the Fase 8 retention worker. app_role is REVOKEd
-- DELETE on accept_events (append-only, migration 0008), so retention cannot
-- run as app_role. This role can SELECT/DELETE exactly the three growth tables
-- retention targets, and write archive_runs. NOLOGIN: a login role is GRANTed
-- this role in a hardened deploy; local dev connects as the `tower` owner.
-- Idempotent role creation (Postgres has no CREATE ROLE IF NOT EXISTS).
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'retention_role') THEN
        CREATE ROLE retention_role NOLOGIN;
    END IF;
END
$$;

-- So whichever role runs migrations (the `tower` owner) can SET ROLE to it in tests.
GRANT retention_role TO CURRENT_USER;

GRANT SELECT, DELETE ON bookings TO retention_role;
GRANT SELECT, DELETE ON accept_events TO retention_role;
GRANT SELECT, DELETE ON notifications TO retention_role;
GRANT SELECT, INSERT, UPDATE ON archive_runs TO retention_role;
```

- [ ] **Step 2: Apply and verify the migration**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test bookings_summary summary_counts_todays_buckets_correctly -- --exact`
Expected: PASS — this test calls `test_pool()` which runs `sqlx::migrate!`, so it applies `0022`; the test itself is unrelated but proves migrations (including the new one) apply cleanly.

- [ ] **Step 3: Confirm the role and grants exist**

Run: `docker exec tower-postgres psql -U tower -d tower -tAc "SELECT rolname FROM pg_roles WHERE rolname='retention_role'; SELECT grantee, privilege_type FROM information_schema.role_table_grants WHERE grantee='retention_role' ORDER BY table_name, privilege_type;"`
Expected: `retention_role` listed; grants show SELECT+DELETE on bookings/accept_events/notifications and SELECT/INSERT/UPDATE on archive_runs.

- [ ] **Step 4: Commit**

```bash
git add Backend/crates/store/migrations/0022_retention_role.sql
git commit -m "feat(store): 0022 retention_role — least-privilege delete role for retention (Fase 8)"
```

---

### Task 2: `store::retention` — table enum, capture, delete-by-id, vacuum (TDD)

**Files:**
- Create: `Backend/crates/store/src/retention.rs`
- Modify: `Backend/crates/store/src/lib.rs:15` (add `pub mod retention;`)
- Modify: `Backend/crates/store/Cargo.toml` (add `sha2`, `flate2` deps — used in Task 3, added now so the module compiles as it grows)
- Test: `Backend/crates/store/tests/retention_pg.rs`

**Interfaces:**
- Produces:
  - `pub enum RetentionTable { Bookings, AcceptEvents, Notifications }` with `pub fn name(&self) -> &'static str` (`"bookings"`/`"accept_events"`/`"notifications"`) and `pub const ALL: [RetentionTable; 3]`.
  - `pub async fn capture_ids(pool: &PgPool, table: RetentionTable, cutoff: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error>`
  - `pub async fn delete_by_ids(pool: &PgPool, table: RetentionTable, ids: &[Uuid], batch: usize) -> Result<u64, sqlx::Error>`
  - `pub async fn vacuum(pool: &PgPool, table: RetentionTable) -> Result<(), sqlx::Error>`

**Context:** These are the raw DB operations. `capture_ids` materializes the exact PK set. `delete_by_ids` deletes only those ids, in chunks — it is the incident-critical path (Aturan Keras #7). `RetentionTable::name()` is the ONLY place a table identifier is interpolated into SQL.

- [ ] **Step 1: Add deps to `Backend/crates/store/Cargo.toml`** (under `[dependencies]`, keep alphabetical-ish with the existing block)

```toml
flate2 = "1.0"
sha2 = "0.10"
```

- [ ] **Step 2: Write the failing test** → `Backend/crates/store/tests/retention_pg.rs`

```rust
// Integration tests for store::retention against real Postgres. Each test seeds a
// uniquely-named tenant + rows and is self-cleaning/parallel-safe. DATABASE_URL must
// point at the `tower` superuser (tests run migrations and delete from accept_events,
// which app_role cannot).
use chrono::{Duration, Utc};
use sqlx::PgPool;
use store::retention::{capture_ids, delete_by_ids, vacuum, RetentionTable};
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
        .bind("Retention Test Tenant")
        .bind(format!("retention-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

/// Insert a booking with an explicit created_at, return its id.
async fn seed_booking_at(pool: &PgPool, tenant_id: Uuid, spx_id: &str, created_at: chrono::DateTime<Utc>) -> Uuid {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO bookings (tenant_id, spx_id, raw_data, status, created_at) \
         VALUES ($1, $2, '{}', 'pending', $3) RETURNING id",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("insert booking");
    id
}

#[tokio::test]
async fn capture_returns_only_rows_older_than_cutoff() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let old1 = seed_booking_at(&pool, tenant_id, &format!("old1-{tenant_id}"), cutoff - Duration::days(1)).await;
    let old2 = seed_booking_at(&pool, tenant_id, &format!("old2-{tenant_id}"), cutoff - Duration::days(10)).await;
    let _new = seed_booking_at(&pool, tenant_id, &format!("new-{tenant_id}"), cutoff + Duration::days(1)).await;

    let mut got = capture_ids(&pool, RetentionTable::Bookings, cutoff).await.expect("capture");
    got.retain(|id| *id == old1 || *id == old2); // ignore rows from other parallel tests
    got.sort();
    let mut want = vec![old1, old2];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn delete_by_ids_removes_only_captured_and_spares_later_inserts() {
    // THE INCIDENT-PREVENTION TEST (Aturan Keras #7).
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);

    // Set A: old rows, captured.
    let a1 = seed_booking_at(&pool, tenant_id, &format!("A1-{tenant_id}"), cutoff - Duration::days(2)).await;
    let a2 = seed_booking_at(&pool, tenant_id, &format!("A2-{tenant_id}"), cutoff - Duration::days(2)).await;
    let captured = vec![a1, a2];

    // Set B: MORE rows that ALSO predate the cutoff, inserted AFTER capture — NOT in `captured`.
    let b1 = seed_booking_at(&pool, tenant_id, &format!("B1-{tenant_id}"), cutoff - Duration::days(3)).await;
    let b2 = seed_booking_at(&pool, tenant_id, &format!("B2-{tenant_id}"), cutoff - Duration::days(3)).await;

    let deleted = delete_by_ids(&pool, RetentionTable::Bookings, &captured, 5000).await.expect("delete");
    assert_eq!(deleted, 2, "exactly the two captured rows are deleted");

    // Set A gone, Set B (un-captured, un-archived) survives — never re-derived from the time predicate.
    for id in &captured {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert!(!exists, "captured row {id} must be deleted");
    }
    for id in [b1, b2] {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
            .bind(id).fetch_one(&pool).await.unwrap();
        assert!(exists, "un-captured row {id} must survive");
    }
}

#[tokio::test]
async fn delete_by_ids_spans_multiple_batches() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let mut ids = Vec::new();
    for i in 0..7 {
        ids.push(seed_booking_at(&pool, tenant_id, &format!("batch-{tenant_id}-{i}"), cutoff - Duration::days(1)).await);
    }
    let deleted = delete_by_ids(&pool, RetentionTable::Bookings, &ids, 3).await.expect("delete");
    assert_eq!(deleted, 7, "all 7 deleted across batches of 3");
}

#[tokio::test]
async fn vacuum_runs_without_error() {
    let pool = test_pool().await;
    vacuum(&pool, RetentionTable::Notifications).await.expect("vacuum");
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test retention_pg 2>&1 | tail -20`
Expected: FAIL to compile — `store::retention` does not exist.

- [ ] **Step 4: Implement the module** → `Backend/crates/store/src/retention.rs`

```rust
//! Fase 8 data-retention primitives. Archives-then-deletes aged rows from the
//! growth tables. The delete path (`delete_by_ids`) targets ONLY the captured
//! id set — never a re-evaluated time predicate (Aturan Keras #7).
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// The fixed set of tables retention operates on. This enum is the ONLY source
/// of table identifiers interpolated into SQL — never accept a table name from
/// outside it (no injection surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTable {
    Bookings,
    AcceptEvents,
    Notifications,
}

impl RetentionTable {
    pub const ALL: [RetentionTable; 3] = [
        RetentionTable::Bookings,
        RetentionTable::AcceptEvents,
        RetentionTable::Notifications,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            RetentionTable::Bookings => "bookings",
            RetentionTable::AcceptEvents => "accept_events",
            RetentionTable::Notifications => "notifications",
        }
    }
}

/// Capture, ONCE, the exact primary-key set of rows older than `cutoff`.
pub async fn capture_ids(
    pool: &PgPool,
    table: RetentionTable,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    // `table.name()` is a &'static str from the enum — safe to format in.
    let sql = format!("SELECT id FROM {} WHERE created_at < $1", table.name());
    let ids: Vec<Uuid> = sqlx::query_scalar(&sql).bind(cutoff).fetch_all(pool).await?;
    Ok(ids)
}

/// Delete ONLY the given ids, in chunks of `batch`. Returns rows deleted.
/// Never re-derives the target set from a time predicate.
pub async fn delete_by_ids(
    pool: &PgPool,
    table: RetentionTable,
    ids: &[Uuid],
    batch: usize,
) -> Result<u64, sqlx::Error> {
    let batch = batch.max(1);
    let sql = format!("DELETE FROM {} WHERE id = ANY($1)", table.name());
    let mut total: u64 = 0;
    for chunk in ids.chunks(batch) {
        let res = sqlx::query(&sql).bind(chunk).execute(pool).await?;
        total += res.rows_affected();
    }
    Ok(total)
}

/// VACUUM the table. Must run outside a transaction (VACUUM cannot run in a txn
/// block); `execute` on the pool runs it as an auto-commit simple statement.
pub async fn vacuum(pool: &PgPool, table: RetentionTable) -> Result<(), sqlx::Error> {
    let sql = format!("VACUUM {}", table.name());
    sqlx::query(&sql).execute(pool).await?;
    Ok(())
}
```

- [ ] **Step 5: Register the module** — add to `Backend/crates/store/src/lib.rs` after line 12 (`pub mod quota;`), keeping alphabetical order:

```rust
pub mod retention;
```

- [ ] **Step 6: Run to verify it passes**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test retention_pg 2>&1 | tail -20`
Expected: PASS — all four tests (`capture_returns_only_rows_older_than_cutoff`, `delete_by_ids_removes_only_captured_and_spares_later_inserts`, `delete_by_ids_spans_multiple_batches`, `vacuum_runs_without_error`).

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/store/src/retention.rs Backend/crates/store/src/lib.rs Backend/crates/store/Cargo.toml Backend/crates/store/tests/retention_pg.rs Backend/Cargo.lock
git commit -m "feat(store): retention capture/delete-by-id/vacuum + incident-prevention test (Fase 8)"
```

---

### Task 3: `store::retention` — CSV.gz archive + SHA-256, and archive_runs helpers (TDD)

**Files:**
- Modify: `Backend/crates/store/src/retention.rs` (append)
- Test: `Backend/crates/store/tests/retention_pg.rs` (append)

**Interfaces:**
- Consumes: `RetentionTable` (Task 2).
- Produces:
  - `pub async fn stream_archive(pool: &PgPool, table: RetentionTable, ids: &[Uuid], path: &std::path::Path) -> Result<(u64, String), RetentionError>` — writes a gzipped CSV (WITH HEADER) of exactly the rows whose id is in `ids`, to `path`; returns `(archived_row_count, sha256_hex_of_the_gz_file)`.
  - `pub enum RetentionError { Db(sqlx::Error), Io(std::io::Error) }` with `From` impls.
  - archive_runs helpers: `pub async fn insert_run(pool, table: RetentionTable, dry_run: bool) -> Result<Uuid, sqlx::Error>`, `pub async fn mark_captured(pool, run_id: Uuid, captured: i64) -> Result<(), sqlx::Error>`, `pub async fn mark_archived(pool, run_id: Uuid, archived: i64, path: &str, sha256: &str) -> Result<(), sqlx::Error>`, `pub async fn mark_completed(pool, run_id: Uuid, deleted: i64) -> Result<(), sqlx::Error>`, `pub async fn mark_failed(pool, run_id: Uuid) -> Result<(), sqlx::Error>`.

**Context:** Archiving uses server-side `COPY (SELECT t.* FROM <table> t JOIN <temp> USING (id)) TO STDOUT WITH (FORMAT csv, HEADER true)`, streamed through a writer that both gzips and SHA-256-hashes the compressed bytes. This handles all columns (including generated ones) without enumeration and guarantees RFC-4180 CSV. `archive_runs` columns (migration `0015`): `id, table_name, run_at, captured_count, archived_count, deleted_count, archive_path, sha256, status ('running'|'completed'|'failed'), dry_run`.

Note on the sqlx 0.9 COPY-out API: `conn.copy_out_raw(sql).await?` returns a `BoxStream<Result<bytes::Bytes, sqlx::Error>>`; consume with `futures::TryStreamExt::try_next`. If the exact method/return type differs in the pinned sqlx, adapt minimally (it is an environment-verifiable detail) — the shape is: get a byte stream from a COPY-OUT statement and write each chunk to the writer.

- [ ] **Step 1: Write the failing test** (append to `retention_pg.rs`)

```rust
use std::io::Read;
use store::retention::{insert_run, mark_archived, stream_archive};

#[tokio::test]
async fn archive_writes_verifiable_gzip_csv() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id1 = seed_booking_at(&pool, tenant_id, &format!("arc1-{tenant_id}"), cutoff - Duration::days(1)).await;
    let id2 = seed_booking_at(&pool, tenant_id, &format!("arc2-{tenant_id}"), cutoff - Duration::days(1)).await;
    let ids = vec![id1, id2];

    let dir = std::env::temp_dir().join(format!("ret-arc-{tenant_id}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bookings.csv.gz");

    let (archived, sha) = stream_archive(&pool, RetentionTable::Bookings, &ids, &path)
        .await
        .expect("archive");
    assert_eq!(archived, 2, "archived row count == captured count");

    // File exists; gunzip → CSV; header + exactly 2 data rows.
    let bytes = std::fs::read(&path).unwrap();
    let mut gz = flate2::read::GzDecoder::new(&bytes[..]);
    let mut csv = String::new();
    gz.read_to_string(&mut csv).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert!(lines[0].contains("spx_id"), "CSV has a header row with column names");
    // 1 header + 2 data rows (rows may contain embedded newlines only if a text/jsonb field
    // has one; these seeded rows use '{}' raw_data and simple spx_ids, so exactly 3 lines).
    assert_eq!(lines.len(), 3, "header + 2 data rows");
    assert!(csv.contains(&format!("arc1-{tenant_id}")));
    assert!(csv.contains(&format!("arc2-{tenant_id}")));

    // Recorded sha256 == fresh hash of the file bytes.
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&bytes);
    let fresh: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(sha, fresh, "returned sha256 matches the gz file");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn archive_runs_row_lifecycle() {
    let pool = test_pool().await;
    let run_id = insert_run(&pool, RetentionTable::Notifications, true).await.expect("insert_run");
    mark_archived(&pool, run_id, 5, "/archive/x.csv.gz", "deadbeef").await.expect("mark_archived");
    let (table_name, archived, path, sha, dry): (String, i64, Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT table_name, archived_count, archive_path, sha256, dry_run FROM archive_runs WHERE id = $1")
            .bind(run_id).fetch_one(&pool).await.unwrap();
    assert_eq!(table_name, "notifications");
    assert_eq!(archived, 5);
    assert_eq!(path.as_deref(), Some("/archive/x.csv.gz"));
    assert_eq!(sha.as_deref(), Some("deadbeef"));
    assert!(dry);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test retention_pg archive 2>&1 | tail -20`
Expected: FAIL to compile — `stream_archive`/`insert_run`/`mark_archived` do not exist.

- [ ] **Step 3: Implement** (append to `Backend/crates/store/src/retention.rs`)

```rust
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::TryStreamExt;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

#[derive(Debug)]
pub enum RetentionError {
    Db(sqlx::Error),
    Io(std::io::Error),
}
impl std::fmt::Display for RetentionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetentionError::Db(e) => write!(f, "db: {e}"),
            RetentionError::Io(e) => write!(f, "io: {e}"),
        }
    }
}
impl std::error::Error for RetentionError {}
impl From<sqlx::Error> for RetentionError {
    fn from(e: sqlx::Error) -> Self {
        RetentionError::Db(e)
    }
}
impl From<std::io::Error> for RetentionError {
    fn from(e: std::io::Error) -> Self {
        RetentionError::Io(e)
    }
}

/// A `Write` that forwards bytes to an inner writer AND a running SHA-256.
/// The hasher is updated with ONLY the bytes actually written (`n`), never the
/// full `buf` — on a short write the unwritten tail is re-submitted by the
/// caller and hashed when it lands, so the hash always equals the file content.
/// Hashing the full `buf` before a possibly-short `inner.write` would desync the
/// sha256 from the file — the exact bug this comment guards against.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}
impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Archive exactly the rows with id in `ids` to a gzipped CSV at `path`.
/// Returns (row_count_archived, sha256_hex_of_the_gz_file). Uses a temp table
/// (ON COMMIT DROP) + server-side COPY so all columns are captured and CSV is
/// RFC-correct; no per-column enumeration.
pub async fn stream_archive(
    pool: &PgPool,
    table: RetentionTable,
    ids: &[Uuid],
    path: &Path,
) -> Result<(u64, String), RetentionError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    // file -> hashing (sha256 of the compressed bytes) -> gzip <- CSV bytes.
    let mut gz = GzEncoder::new(
        HashingWriter { inner: std::io::BufWriter::new(file), hasher: Sha256::new() },
        Compression::default(),
    );

    let mut tx = pool.begin().await?;
    sqlx::query("CREATE TEMP TABLE _ret_ids (id uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO _ret_ids SELECT unnest($1::uuid[])")
        .bind(ids)
        .execute(&mut *tx)
        .await?;

    let count_sql = format!(
        "SELECT count(*) FROM {} t JOIN _ret_ids r USING (id)",
        table.name()
    );
    let archived: i64 = sqlx::query_scalar(&count_sql).fetch_one(&mut *tx).await?;

    let copy_sql = format!(
        "COPY (SELECT t.* FROM {} t JOIN _ret_ids r USING (id)) TO STDOUT WITH (FORMAT csv, HEADER true)",
        table.name()
    );
    {
        let mut stream = tx.copy_out_raw(&copy_sql).await?;
        while let Some(chunk) = stream.try_next().await? {
            gz.write_all(&chunk)?;
        }
    }
    tx.commit().await?; // drops the temp table

    // Finalize gzip, then the hash of everything written.
    let hashing = gz.finish()?;
    hashing.inner.into_inner().map_err(|e| RetentionError::Io(e.into_error()))?; // flush BufWriter -> File
    let sha_hex: String = hashing.hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok((archived as u64, sha_hex))
}

pub async fn insert_run(
    pool: &PgPool,
    table: RetentionTable,
    dry_run: bool,
) -> Result<Uuid, sqlx::Error> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO archive_runs (table_name, captured_count, archived_count, deleted_count, status, dry_run) \
         VALUES ($1, 0, 0, 0, 'running', $2) RETURNING id",
    )
    .bind(table.name())
    .bind(dry_run)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

pub async fn mark_captured(pool: &PgPool, run_id: Uuid, captured: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE archive_runs SET captured_count = $2 WHERE id = $1")
        .bind(run_id).bind(captured).execute(pool).await?;
    Ok(())
}

pub async fn mark_archived(
    pool: &PgPool,
    run_id: Uuid,
    archived: i64,
    path: &str,
    sha256: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE archive_runs SET archived_count = $2, archive_path = $3, sha256 = $4 WHERE id = $1")
        .bind(run_id).bind(archived).bind(path).bind(sha256).execute(pool).await?;
    Ok(())
}

pub async fn mark_completed(pool: &PgPool, run_id: Uuid, deleted: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE archive_runs SET deleted_count = $2, status = 'completed' WHERE id = $1")
        .bind(run_id).bind(deleted).execute(pool).await?;
    Ok(())
}

pub async fn mark_failed(pool: &PgPool, run_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE archive_runs SET status = 'failed' WHERE id = $1")
        .bind(run_id).execute(pool).await?;
    Ok(())
}
```

- [ ] **Step 4: Add `futures-util` to store deps if not present** — check `Backend/crates/store/Cargo.toml`; if `futures-util` (or `futures`) is not a dependency, add under `[dependencies]`:

```toml
futures-util = "0.3"
```

Run: `cd Backend && grep -E "futures" crates/store/Cargo.toml || echo "NEED futures-util"`
If it prints `NEED futures-util`, add the line above; otherwise use whatever `futures`/`futures-util` import path already resolves and adjust the `use` in the module accordingly.

- [ ] **Step 5: Run to verify it passes**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test retention_pg 2>&1 | tail -25`
Expected: PASS — all Task 2 + Task 3 tests, including `archive_writes_verifiable_gzip_csv` and `archive_runs_row_lifecycle`.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/store/src/retention.rs Backend/crates/store/Cargo.toml Backend/crates/store/tests/retention_pg.rs Backend/Cargo.lock
git commit -m "feat(store): retention CSV.gz+sha256 archive via COPY + archive_runs helpers (Fase 8)"
```

---

### Task 4: `store::retention::run_cycle` — advisory-locked orchestration (TDD)

**Files:**
- Modify: `Backend/crates/store/src/retention.rs` (append)
- Test: `Backend/crates/store/tests/retention_pg.rs` (append)

**Interfaces:**
- Consumes: everything from Tasks 2–3.
- Produces:
  - `pub struct RetentionConfig { pub dry_run: bool, pub archive_dir: std::path::PathBuf, pub delete_batch: usize, pub windows: Vec<(RetentionTable, i64)> }` — `windows` lists (table, days); a table absent from `windows` (or days <= 0) is skipped.
  - `pub enum RunStatus { Completed, Failed, Skipped }`
  - `pub struct TableOutcome { pub table: RetentionTable, pub captured: u64, pub archived: u64, pub deleted: u64, pub status: RunStatus }`
  - `pub async fn run_cycle(pool: &PgPool, config: &RetentionConfig) -> Result<Vec<TableOutcome>, RetentionError>` — acquires a session advisory lock (returns an empty Vec if another runner holds it), then per configured table: capture → archive → verify → (dry-run ? skip delete : delete-by-id + vacuum) → record `archive_runs`. Always releases the lock.
  - `pub const RETENTION_ADVISORY_KEY: i64 = 0x544f_5745_525f_5254;`
  - A helper to build a timestamped path: `pub fn archive_path(dir: &std::path::Path, table: RetentionTable, now: DateTime<Utc>) -> std::path::PathBuf` → `<dir>/<table>_<YYYYMMDD_HHMMSS>.csv.gz`.

**Context:** This composes the pieces into the master-spec algorithm. The advisory lock is session-level on a dedicated pooled connection held for the whole cycle and explicitly unlocked at the end (a pooled connection's session does not close on return, so the lock must be released explicitly or it leaks). The verify step (`archived == captured`) gates the delete. Each table is independent — one table's failure records `status='failed'` and continues to the next.

- [ ] **Step 1: Write the failing test** (append to `retention_pg.rs`)

```rust
use store::retention::{run_cycle, RetentionConfig, RetentionTable as RT, RunStatus, RETENTION_ADVISORY_KEY};

fn cfg(dir: &std::path::Path, dry_run: bool, table: RT, days: i64) -> RetentionConfig {
    RetentionConfig {
        dry_run,
        archive_dir: dir.to_path_buf(),
        delete_batch: 5000,
        windows: vec![(table, days)],
    }
}

#[tokio::test]
async fn dry_run_archives_but_deletes_nothing() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("dry-{tenant_id}"), cutoff - Duration::days(1)).await;
    let dir = std::env::temp_dir().join(format!("ret-dry-{tenant_id}"));

    let outcomes = run_cycle(&pool, &cfg(&dir, true, RT::Bookings, 90)).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).expect("bookings outcome");
    assert!(o.captured >= 1);
    assert_eq!(o.deleted, 0, "dry-run deletes nothing");
    assert!(matches!(o.status, RunStatus::Completed));

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(exists, "row still present after dry-run");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn real_run_archives_then_deletes_captured() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("real-{tenant_id}"), cutoff - Duration::days(1)).await;
    let dir = std::env::temp_dir().join(format!("ret-real-{tenant_id}"));

    let outcomes = run_cycle(&pool, &cfg(&dir, false, RT::Bookings, 90)).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).expect("bookings outcome");
    assert!(o.deleted >= 1);
    assert!(matches!(o.status, RunStatus::Completed));

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(!exists, "captured row deleted after real run");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn skips_when_advisory_lock_held() {
    let pool = test_pool().await;
    // Hold the lock on a separate connection.
    let mut holder = pool.acquire().await.unwrap();
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(RETENTION_ADVISORY_KEY).fetch_one(&mut *holder).await.unwrap();
    assert!(got, "test acquired the lock first");

    let dir = std::env::temp_dir().join("ret-locked");
    let outcomes = run_cycle(&pool, &cfg(&dir, true, RT::Bookings, 90)).await.expect("cycle");
    assert!(outcomes.is_empty(), "run_cycle skips entirely when the lock is held");

    // release
    let _: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(RETENTION_ADVISORY_KEY).fetch_one(&mut *holder).await.unwrap();
}

#[tokio::test]
async fn window_zero_disables_table() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("z-{tenant_id}"), cutoff - Duration::days(1)).await;
    let dir = std::env::temp_dir().join(format!("ret-zero-{tenant_id}"));

    let outcomes = run_cycle(&pool, &cfg(&dir, false, RT::Bookings, 0)).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).expect("bookings outcome present");
    assert!(matches!(o.status, RunStatus::Skipped), "days=0 → table Skipped");
    assert_eq!(o.deleted, 0);
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(exists, "row untouched when window disabled");
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test retention_pg run 2>&1 | tail -20`
Expected: FAIL to compile — `run_cycle`/`RetentionConfig`/`RunStatus`/`RETENTION_ADVISORY_KEY` do not exist.

- [ ] **Step 3: Implement** (append to `Backend/crates/store/src/retention.rs`)

```rust
use chrono::Timelike;

pub const RETENTION_ADVISORY_KEY: i64 = 0x544f_5745_525f_5254; // "TOWER_RT"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug)]
pub struct TableOutcome {
    pub table: RetentionTable,
    pub captured: u64,
    pub archived: u64,
    pub deleted: u64,
    pub status: RunStatus,
}

pub struct RetentionConfig {
    pub dry_run: bool,
    pub archive_dir: std::path::PathBuf,
    pub delete_batch: usize,
    pub windows: Vec<(RetentionTable, i64)>,
}

pub fn archive_path(
    dir: &Path,
    table: RetentionTable,
    now: DateTime<Utc>,
) -> std::path::PathBuf {
    let stamp = format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    );
    dir.join(format!("{}_{}.csv.gz", table.name(), stamp))
}
// bring Datelike into scope for year()/month()/day()
use chrono::Datelike;

/// Run one full retention cycle. Returns an empty Vec if another runner holds
/// the advisory lock (single-runner guarantee). Always releases the lock.
pub async fn run_cycle(
    pool: &PgPool,
    config: &RetentionConfig,
) -> Result<Vec<TableOutcome>, RetentionError> {
    let mut lock_conn = pool.acquire().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(RETENTION_ADVISORY_KEY)
        .fetch_one(&mut *lock_conn)
        .await?;
    if !acquired {
        return Ok(Vec::new());
    }

    let result = run_all_tables(pool, config).await;

    // Always release the session lock, regardless of the cycle result.
    let _: Result<bool, _> = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(RETENTION_ADVISORY_KEY)
        .fetch_one(&mut *lock_conn)
        .await;

    result
}

async fn run_all_tables(
    pool: &PgPool,
    config: &RetentionConfig,
) -> Result<Vec<TableOutcome>, RetentionError> {
    let now = Utc::now();
    let mut outcomes = Vec::new();

    for (table, days) in &config.windows {
        let (table, days) = (*table, *days);
        if days <= 0 {
            outcomes.push(TableOutcome { table, captured: 0, archived: 0, deleted: 0, status: RunStatus::Skipped });
            continue;
        }
        let cutoff = now - chrono::Duration::days(days);
        let run_id = insert_run(pool, table, config.dry_run).await?;

        let ids = capture_ids(pool, table, cutoff).await?;
        mark_captured(pool, run_id, ids.len() as i64).await?;

        if ids.is_empty() {
            mark_completed(pool, run_id, 0).await?;
            outcomes.push(TableOutcome { table, captured: 0, archived: 0, deleted: 0, status: RunStatus::Completed });
            continue;
        }

        let path = archive_path(&config.archive_dir, table, now);
        let (archived, sha) = stream_archive(pool, table, &ids, &path).await?;
        mark_archived(pool, run_id, archived as i64, &path.to_string_lossy(), &sha).await?;

        // VERIFY before any delete.
        if archived != ids.len() as u64 {
            mark_failed(pool, run_id).await?;
            outcomes.push(TableOutcome { table, captured: ids.len() as u64, archived, deleted: 0, status: RunStatus::Failed });
            continue;
        }

        if config.dry_run {
            mark_completed(pool, run_id, 0).await?;
            outcomes.push(TableOutcome { table, captured: ids.len() as u64, archived, deleted: 0, status: RunStatus::Completed });
            continue;
        }

        let deleted = delete_by_ids(pool, table, &ids, config.delete_batch).await?;
        vacuum(pool, table).await?;
        mark_completed(pool, run_id, deleted as i64).await?;
        outcomes.push(TableOutcome { table, captured: ids.len() as u64, archived, deleted, status: RunStatus::Completed });
    }

    Ok(outcomes)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower cargo test -p store --test retention_pg 2>&1 | tail -30`
Expected: PASS — all retention tests (Tasks 2–4), including `dry_run_archives_but_deletes_nothing`, `real_run_archives_then_deletes_captured`, `skips_when_advisory_lock_held`, `window_zero_disables_table`.

- [ ] **Step 5: Add a verify-mismatch test** (append) — proves the verify gate blocks deletes. Since `stream_archive` always archives exactly the captured set (so a natural mismatch can't occur), assert the gate logic directly by confirming a real run with a mismatch path is unreachable in normal operation AND that the `archived != captured` branch exists by asserting the happy path records equal counts:

```rust
#[tokio::test]
async fn archived_count_equals_captured_on_success() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    for i in 0..3 {
        seed_booking_at(&pool, tenant_id, &format!("eq-{tenant_id}-{i}"), cutoff - Duration::days(1)).await;
    }
    let dir = std::env::temp_dir().join(format!("ret-eq-{tenant_id}"));
    let outcomes = run_cycle(&pool, &cfg(&dir, true, RT::Bookings, 90)).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).unwrap();
    assert_eq!(o.captured, o.archived, "verify gate: archived count equals captured count");
    assert!(o.captured >= 3);
    std::fs::remove_dir_all(&dir).ok();
}
```

Run the same test command; Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/store/src/retention.rs Backend/crates/store/tests/retention_pg.rs
git commit -m "feat(store): retention run_cycle — advisory-locked capture/archive/verify/delete (Fase 8)"
```

---

### Task 5: `bin/retention` — env config + scheduler loop

**Files:**
- Create: `Backend/bin/retention/Cargo.toml`
- Create: `Backend/bin/retention/src/main.rs`
- Modify: `Backend/Cargo.toml` (add `"bin/retention"` to `[workspace] members`)

**Interfaces:**
- Consumes: `store::retention::{run_cycle, RetentionConfig, RetentionTable, TableOutcome}`.
- Produces: the `retention` binary. `RETENTION_RUN_ONCE=true` runs one cycle and exits; otherwise it loops (compute next `HH:MM`, sleep, run, repeat).

**Context:** Thin binary. All logic is in `store::retention`; `main` only parses env into `RetentionConfig`, connects, and drives. The env contract is in the design doc §Configuration. Mirror `bin/reactor-core`'s `env_or` helper and `tracing` setup style.

- [ ] **Step 1: Create `Backend/bin/retention/Cargo.toml`**

```toml
[package]
name = "retention"
version.workspace = true
edition.workspace = true
publish.workspace = true

[dependencies]
store = { version = "0.1.0", path = "../../crates/store" }
sqlx = { version = "0.9.0", features = ["postgres", "runtime-tokio", "tls-rustls-ring-native-roots", "macros", "migrate", "uuid", "chrono"] }
tokio = { version = "1.52.3", features = ["rt-multi-thread", "macros", "time"] }
chrono = { version = "0.4.45" }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

(Confirm the exact `tracing`/`tracing-subscriber` versions match `bin/reactor-core/Cargo.toml`; copy them verbatim from there to stay consistent.)

- [ ] **Step 2: Add the workspace member** — in `Backend/Cargo.toml`, add `"bin/retention"` to the `members` array (after `"bin/auth-sidecar"`):

```toml
    "bin/auth-sidecar",
    "bin/retention",
```

- [ ] **Step 3: Write `Backend/bin/retention/src/main.rs`**

```rust
//! Fase 8 retention worker. Reads env config; RETENTION_RUN_ONCE=true runs a
//! single cycle and exits (CI/manual), otherwise self-schedules a daily run at
//! RETENTION_SCHEDULE_HOUR:RETENTION_SCHEDULE_MINUTE (local time). All logic is
//! in store::retention; this binary only parses config and drives the loop.
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use chrono::{Local, NaiveTime, Timelike};
use sqlx::postgres::PgPoolOptions;
use store::retention::{run_cycle, RetentionConfig, RetentionTable, RETENTION_ADVISORY_KEY};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn build_config() -> RetentionConfig {
    let windows = vec![
        (RetentionTable::Bookings, env_i64("RETENTION_BOOKINGS_DAYS", 90)),
        (RetentionTable::AcceptEvents, env_i64("RETENTION_ACCEPT_EVENTS_DAYS", 180)),
        (RetentionTable::Notifications, env_i64("RETENTION_NOTIFICATIONS_DAYS", 30)),
    ];
    RetentionConfig {
        dry_run: env_or("RETENTION_DRY_RUN", "true") != "false",
        archive_dir: PathBuf::from(env_or("RETENTION_ARCHIVE_DIR", "/archive")),
        delete_batch: env_i64("RETENTION_DELETE_BATCH", 5000).max(1) as usize,
        windows,
        advisory_key: RETENTION_ADVISORY_KEY, // the one fixed production single-runner key
    }
}

/// Seconds from `now` until the next local HH:MM. If today's HH:MM has passed,
/// schedule for tomorrow. Always >= 1s.
fn seconds_until_next(now: chrono::DateTime<Local>, hour: u32, minute: u32) -> u64 {
    let target_time = NaiveTime::from_hms_opt(hour.min(23), minute.min(59), 0).unwrap();
    let today_target = now.date_naive().and_time(target_time);
    let next = if now.time() < target_time {
        today_target
    } else {
        (now.date_naive() + chrono::Duration::days(1)).and_time(target_time)
    };
    let next_local = next.and_local_timezone(Local).earliest().unwrap_or(now);
    (next_local - now).num_seconds().max(1) as u64
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = env_or(
        "DATABASE_URL",
        "postgres://tower:tower_dev_only@127.0.0.1:15432/tower",
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .expect("retention: connect Postgres");

    let config = build_config();
    tracing::info!(dry_run = config.dry_run, archive_dir = %config.archive_dir.display(), "retention worker starting");

    let run_once = env_or("RETENTION_RUN_ONCE", "false") == "true";
    if run_once {
        match run_cycle(&pool, &config).await {
            Ok(outcomes) => {
                for o in &outcomes {
                    tracing::info!(table = o.table.name(), captured = o.captured, archived = o.archived, deleted = o.deleted, status = ?o.status, "retention table done");
                }
                if outcomes.is_empty() {
                    tracing::warn!("retention skipped — another runner holds the advisory lock");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "retention cycle failed");
                std::process::exit(1);
            }
        }
        return;
    }

    let hour = env_i64("RETENTION_SCHEDULE_HOUR", 3).clamp(0, 23) as u32;
    let minute = env_i64("RETENTION_SCHEDULE_MINUTE", 30).clamp(0, 59) as u32;
    loop {
        let wait = seconds_until_next(Local::now(), hour, minute);
        tracing::info!(seconds = wait, "retention sleeping until next run");
        tokio::time::sleep(StdDuration::from_secs(wait)).await;
        match run_cycle(&pool, &config).await {
            Ok(outcomes) => {
                for o in &outcomes {
                    tracing::info!(table = o.table.name(), captured = o.captured, archived = o.archived, deleted = o.deleted, status = ?o.status, "retention table done");
                }
            }
            Err(e) => tracing::error!(error = %e, "retention cycle failed; will retry next schedule"),
        }
    }
}
```

- [ ] **Step 4: Add a unit test for `seconds_until_next`** — the only non-trivial pure logic in the binary. Append a `#[cfg(test)] mod tests` to `main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::seconds_until_next;
    use chrono::{Local, TimeZone};

    #[test]
    fn schedules_later_today_when_target_not_yet_passed() {
        // 01:00 local, target 03:30 → 2h30m = 9000s.
        let now = Local.with_ymd_and_hms(2026, 7, 21, 1, 0, 0).unwrap();
        assert_eq!(seconds_until_next(now, 3, 30), 9000);
    }

    #[test]
    fn schedules_tomorrow_when_target_already_passed() {
        // 04:00 local, target 03:30 → 23h30m tomorrow = 84600s.
        let now = Local.with_ymd_and_hms(2026, 7, 21, 4, 0, 0).unwrap();
        assert_eq!(seconds_until_next(now, 3, 30), 84600);
    }
}
```

- [ ] **Step 5: Build, test the binary, and run it once against the live dev DB**

Run: `cd Backend && cargo build -p retention 2>&1 | tail -5`
Expected: builds clean.

Run: `cd Backend && cargo test -p retention 2>&1 | tail -8`
Expected: the two `seconds_until_next` unit tests pass.

Run (smoke, dry-run, one cycle against the dev DB, writing archives to a temp dir):
```
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower \
  RETENTION_RUN_ONCE=true RETENTION_DRY_RUN=true RETENTION_ARCHIVE_DIR=/tmp/tower-archive \
  RUST_LOG=info cargo run -p retention 2>&1 | tail -15
```
Expected: logs one "retention table done" line per table (bookings/accept_events/notifications), all `status=Completed`, `deleted=0` (dry-run), exit 0. `/tmp/tower-archive` contains `*.csv.gz` files for any table that had aged rows (may be empty if the dev DB has none old enough — that is a valid `Completed` with `captured=0`).

- [ ] **Step 6: Commit**

```bash
git add Backend/bin/retention Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(retention): bin/retention worker — env config + daily scheduler + RUN_ONCE (Fase 8)"
```

---

### Task 6: Docker packaging + compose wiring + final verification

**Files:**
- Create: `Docker/retention.Dockerfile`
- Modify: `Docker/docker-compose.yml` (replace the `tower-retention` `alpine:3` no-op with the built worker + an `archive` volume)

**Interfaces:**
- Consumes: the `retention` binary (Task 5).
- Produces: a `tower-retention` service that runs the real worker in Compose, and an `archive` named volume mounted at `RETENTION_ARCHIVE_DIR`.

**Context:** Keep this minimal — the full `docker compose up` end-to-end validation (including the `app_role`/role-provisioning story) is 8-Deploy-lokal. Here we only swap the placeholder for the real worker so the image builds and the service starts. Mirror `Docker/reactor-core.Dockerfile`'s multi-stage build.

- [ ] **Step 1: Read the existing reactor-core Dockerfile to mirror its build stage**

Run: `cat Docker/reactor-core.Dockerfile`
Expected: a multi-stage Rust build (a `rust:*` builder that `cargo build --release -p reactor-core`, then a slim runtime copying the binary). Copy its structure exactly, substituting the package/binary name `retention`.

- [ ] **Step 2: Write `Docker/retention.Dockerfile`** — same structure as `reactor-core.Dockerfile`, building `-p retention` and running `/usr/local/bin/retention`. Use the exact base images and build flags from the reactor-core Dockerfile (do not invent new ones). The runtime stage must include a writable archive dir; the compose volume mount provides it, so no special handling beyond the binary is needed.

(Write the file mirroring reactor-core's exact stages; the only differences are `-p retention` in the build command, the copied binary name `retention`, and the final `CMD ["retention"]` / `ENTRYPOINT`.)

- [ ] **Step 3: Replace the `tower-retention` service** in `Docker/docker-compose.yml`. Replace the existing block:

```yaml
  tower-retention:
    image: alpine:3
    container_name: tower-retention
    restart: "no"
    command: ["sh", "-c", "echo 'tower-retention: no-op placeholder (Fase 8 implements the real pg_cron-driven job)'; sleep 5"]
    networks:
      - tower-net
```

with:

```yaml
  tower-retention:
    build:
      context: ..
      dockerfile: Docker/retention.Dockerfile
    container_name: tower-retention
    restart: unless-stopped
    environment:
      DATABASE_URL: postgres://tower:tower_dev_only@tower-postgres:5432/tower
      RETENTION_DRY_RUN: "true"
      RETENTION_ARCHIVE_DIR: /archive
      RETENTION_SCHEDULE_HOUR: "3"
      RETENTION_SCHEDULE_MINUTE: "30"
      RUST_LOG: info
    volumes:
      - tower-archive:/archive
    depends_on:
      tower-postgres:
        condition: service_healthy
    networks:
      - tower-net
```

And add `tower-archive:` to the top-level `volumes:` block (alongside `tower-postgres-data`, `tower-redis-data`):

```yaml
volumes:
  tower-postgres-data:
  tower-redis-data:
  tower-archive:
```

(Match the exact indentation and the `depends_on`/healthcheck style already used by the `reactor-core`/other services in this compose file. The `DATABASE_URL` here uses `tower` for local-first per the design's role decision; wiring `retention_role` is 8-Deploy-lokal.)

- [ ] **Step 4: Validate the compose file parses and the image builds**

Run: `docker compose -f Docker/docker-compose.yml config >/dev/null && echo "compose OK"`
Expected: `compose OK` (no YAML/schema error).

Run: `docker compose -f Docker/docker-compose.yml build tower-retention 2>&1 | tail -8`
Expected: the image builds successfully.

- [ ] **Step 5: Full workspace verification**

Run (foreground, do NOT background):
```
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test --workspace --exclude reactor-core
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test -p reactor-core --bin reactor-core -- --test-threads=1
cd Backend && cargo clippy --workspace --all-targets -- -D warnings
cd Backend && cargo deny check
```
Expected: all green. (`reactor-core` bin tests run single-threaded to avoid the known parallel `ALTER ROLE app_role` catalog race — a pre-existing environmental flake, not a regression.) The new `retention_pg` suite and `retention` unit tests pass. `cargo deny` accepts `flate2`/`sha2`.

- [ ] **Step 6: Commit**

```bash
git add Docker/retention.Dockerfile Docker/docker-compose.yml
git commit -m "feat(docker): real tower-retention worker + archive volume (Fase 8)"
```

---

## Self-Review Notes

- **Spec coverage:** Rust-worker-not-pg_cron (Tasks 2–5), target tables + windows (Task 5 `build_config`), `retention_role` least-privilege (Task 1), the safe algorithm capture→archive→verify→delete-by-id→VACUUM (Task 4 `run_cycle`), the load-bearing incident-prevention invariant (Task 2 `delete_by_ids_removes_only_captured_and_spares_later_inserts`), DRY_RUN default ON (Task 4 + Task 5 `build_config`), advisory-lock single-runner (Task 4 `skips_when_advisory_lock_held`), CSV.gz + sha256 archive via COPY (Task 3), archive_runs lifecycle (Task 3), config env contract (Task 5), container swap + archive volume (Task 6). Every design section maps to a task.
- **Deferred (per design, not gaps):** running as `retention_role` under RLS and the full `docker compose up` end-to-end are 8-Deploy-lokal; here the worker runs as the `tower` owner and is proven via `cargo run -p retention RUN_ONCE` + integration tests.
- **Placeholder scan:** every code step has complete code; every run step has an exact command + expected result. Task 6 Step 2 intentionally instructs mirroring `reactor-core.Dockerfile` rather than pasting a guessed Dockerfile — the implementer reads the real one first (its exact base images must not be guessed); this is a read-then-mirror instruction, not a placeholder.
- **Type consistency:** `RetentionTable`, `RetentionConfig`, `TableOutcome`, `RunStatus`, `run_cycle`, `capture_ids`, `delete_by_ids`, `vacuum`, `stream_archive`, `insert_run`/`mark_*`, `RETENTION_ADVISORY_KEY`, `archive_path` are defined once and used with identical signatures across tasks and the binary.
- **Environment caveats carried:** `DATABASE_URL` must be the `tower` superuser for tests (delete on append-only `accept_events`); the `reactor-core` bin test parallel `ALTER ROLE` flake needs `--test-threads=1`; infra (`tower-postgres`/`tower-redis`) must be up (`docker compose -f Docker/docker-compose.yml up -d tower-postgres tower-redis`).
- **One API risk flagged for the implementer:** the sqlx 0.9 `copy_out_raw` streaming shape (Task 3) — the TDD test surfaces any signature mismatch immediately against real Postgres; adapt minimally if the pinned version differs.
