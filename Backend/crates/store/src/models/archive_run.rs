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
