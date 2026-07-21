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
    // sqlx 0.9's SqlSafeStr requires dynamic SQL to be explicitly asserted safe.
    let sql = format!("SELECT id FROM {} WHERE created_at < $1", table.name());
    let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(cutoff)
        .fetch_all(pool)
        .await?;
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
        let res = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(chunk)
            .execute(pool)
            .await?;
        total += res.rows_affected();
    }
    Ok(total)
}

/// VACUUM the table. Must run outside a transaction (VACUUM cannot run in a txn
/// block); `execute` on the pool runs it as an auto-commit simple statement.
pub async fn vacuum(pool: &PgPool, table: RetentionTable) -> Result<(), sqlx::Error> {
    let sql = format!("VACUUM {}", table.name());
    sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
    Ok(())
}

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

    // table.name() is a &'static str from the closed enum — safe to format in.
    // sqlx 0.9's SqlSafeStr requires dynamic SQL to be explicitly asserted safe.
    let count_sql = format!(
        "SELECT count(*) FROM {} t JOIN _ret_ids r USING (id)",
        table.name()
    );
    let archived: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql.as_str()))
        .fetch_one(&mut *tx)
        .await?;

    let copy_sql = format!(
        "COPY (SELECT t.* FROM {} t JOIN _ret_ids r USING (id)) TO STDOUT WITH (FORMAT csv, HEADER true)",
        table.name()
    );
    {
        // copy_out_raw takes a plain &str (not the SqlSafeStr-guarded query API), so no
        // AssertSqlSafe wrapper is needed here.
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
