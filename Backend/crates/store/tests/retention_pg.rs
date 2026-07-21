// Integration tests for store::retention against real Postgres. Each test seeds a
// uniquely-named tenant + rows and is self-cleaning/parallel-safe. DATABASE_URL must
// point at the `tower` superuser (tests run migrations and delete from accept_events,
// which app_role cannot).
//
// Retention is a GLOBAL (system-wide, not tenant-scoped) destructive operation, so these
// integration tests over the shared bookings/accept_events/notifications tables cannot safely
// interleave (one test's real delete would remove another's just-seeded rows). Each test is
// therefore `#[serial]` (serial_test) — they run one-at-a-time WITHIN this test binary while
// other crates' binaries still run in parallel, so `cargo test --workspace` stays clean with
// NO `--test-threads=1` flag. Each run_cycle test also passes a unique advisory_key (via
// `key_for`) so the single-runner advisory lock is exercised without a shared global key.
use chrono::{Duration, Utc};
use serial_test::serial;
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
async fn vacuum_runs_without_error() {
    let pool = test_pool().await;
    vacuum(&pool, RetentionTable::Notifications).await.expect("vacuum");
}

#[tokio::test]
#[serial]
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
#[serial]
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

use store::retention::{run_cycle, RetentionConfig, RetentionTable as RT, RunStatus};

/// A unique advisory-lock key per test, derived from its tenant id, so parallel
/// run_cycle tests never contend for one global lock.
fn key_for(t: Uuid) -> i64 {
    i64::from_le_bytes(t.as_bytes()[0..8].try_into().unwrap())
}

fn cfg(dir: &std::path::Path, dry_run: bool, table: RT, days: i64, advisory_key: i64) -> RetentionConfig {
    RetentionConfig {
        dry_run,
        archive_dir: dir.to_path_buf(),
        delete_batch: 5000,
        windows: vec![(table, days)],
        advisory_key,
    }
}

#[tokio::test]
#[serial]
async fn dry_run_archives_but_deletes_nothing() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("dry-{tenant_id}"), cutoff - Duration::days(1)).await;
    let dir = std::env::temp_dir().join(format!("ret-dry-{tenant_id}"));

    let outcomes = run_cycle(&pool, &cfg(&dir, true, RT::Bookings, 90, key_for(tenant_id))).await.expect("cycle");
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
#[serial]
async fn real_run_archives_then_deletes_captured() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("real-{tenant_id}"), cutoff - Duration::days(1)).await;
    let dir = std::env::temp_dir().join(format!("ret-real-{tenant_id}"));

    let outcomes = run_cycle(&pool, &cfg(&dir, false, RT::Bookings, 90, key_for(tenant_id))).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).expect("bookings outcome");
    assert!(o.deleted >= 1);
    assert!(matches!(o.status, RunStatus::Completed));

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(!exists, "captured row deleted after real run");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
#[serial]
async fn skips_when_advisory_lock_held() {
    let pool = test_pool().await;
    // A key unique to this test, held on a separate connection, then passed to run_cycle.
    let lock_key = key_for(Uuid::new_v4());
    let mut holder = pool.acquire().await.unwrap();
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lock_key).fetch_one(&mut *holder).await.unwrap();
    assert!(got, "test acquired the lock first");

    let dir = std::env::temp_dir().join(format!("ret-locked-{lock_key}"));
    let outcomes = run_cycle(&pool, &cfg(&dir, true, RT::Bookings, 90, lock_key)).await.expect("cycle");
    assert!(outcomes.is_empty(), "run_cycle skips entirely when the lock is held");

    // release
    let _: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(lock_key).fetch_one(&mut *holder).await.unwrap();
}

#[tokio::test]
#[serial]
async fn window_zero_disables_table() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("z-{tenant_id}"), cutoff - Duration::days(1)).await;
    let dir = std::env::temp_dir().join(format!("ret-zero-{tenant_id}"));

    let outcomes = run_cycle(&pool, &cfg(&dir, false, RT::Bookings, 0, key_for(tenant_id))).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).expect("bookings outcome present");
    assert!(matches!(o.status, RunStatus::Skipped), "days=0 → table Skipped");
    assert_eq!(o.deleted, 0);
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(exists, "row untouched when window disabled");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
#[serial]
async fn archived_count_equals_captured_on_success() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    for i in 0..3 {
        seed_booking_at(&pool, tenant_id, &format!("eq-{tenant_id}-{i}"), cutoff - Duration::days(1)).await;
    }
    let dir = std::env::temp_dir().join(format!("ret-eq-{tenant_id}"));
    let outcomes = run_cycle(&pool, &cfg(&dir, true, RT::Bookings, 90, key_for(tenant_id))).await.expect("cycle");
    let o = outcomes.iter().find(|o| o.table == RT::Bookings).unwrap();
    assert_eq!(o.captured, o.archived, "verify gate: archived count equals captured count");
    assert!(o.captured >= 3);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
#[serial]
async fn a_failing_table_does_not_abort_the_cycle() {
    // Important-finding regression: an error on one table must NOT abort the cycle
    // (design invariant "each table is independent"), and a failed archive must never
    // be followed by a delete.
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    let cutoff = Utc::now() - Duration::days(90);
    let id = seed_booking_at(&pool, tenant_id, &format!("failtab-{tenant_id}"), cutoff - Duration::days(1)).await;

    // Force stream_archive to fail: point archive_dir at a subdir UNDER a regular file,
    // so create_dir_all() errors (ENOTDIR) before any row is archived or deleted.
    let blocker = std::env::temp_dir().join(format!("ret-blocker-{tenant_id}"));
    std::fs::write(&blocker, b"x").unwrap();
    let dir = blocker.join("archive");

    let config = RetentionConfig {
        dry_run: false,
        archive_dir: dir,
        delete_batch: 5000,
        windows: vec![(RT::Bookings, 90), (RT::Notifications, 30)],
        advisory_key: key_for(tenant_id),
    };
    let outcomes = run_cycle(&pool, &config).await.expect("cycle returns Ok even when a table errors");
    assert_eq!(outcomes.len(), 2, "both tables attempted — the first table's failure didn't abort the cycle");
    let bookings = outcomes.iter().find(|o| o.table == RT::Bookings).unwrap();
    assert!(matches!(bookings.status, RunStatus::Failed), "bookings archive failed → Failed outcome");

    // Row NOT deleted: archive failed before the delete step, so nothing was removed.
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE id = $1)")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(exists, "row survives a failed archive — no delete without a verified archive");

    std::fs::remove_file(&blocker).ok();
}
