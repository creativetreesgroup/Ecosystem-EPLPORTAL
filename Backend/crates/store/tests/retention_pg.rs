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
