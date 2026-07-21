// Integration tests for store::retention against real Postgres. Each test seeds a
// uniquely-named tenant + rows and is self-cleaning/parallel-safe. DATABASE_URL must
// point at the `tower` superuser (tests run migrations and delete from accept_events,
// which app_role cannot).
use chrono::{Duration, Utc};
use sqlx::PgPool;
use std::io::Read;
use store::retention::{capture_ids, delete_by_ids, insert_run, mark_archived, stream_archive, vacuum, RetentionTable};
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
