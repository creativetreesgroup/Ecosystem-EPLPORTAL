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
