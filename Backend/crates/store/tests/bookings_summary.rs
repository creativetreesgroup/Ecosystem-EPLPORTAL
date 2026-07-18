use sqlx::PgPool;
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
        .bind("Bookings Summary Test Tenant")
        .bind(format!("bookings-summary-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

#[tokio::test]
async fn summary_counts_todays_buckets_correctly() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;

    // 2 incoming (any status), 1 accepted+auto, 1 accepted+manual, 1 taken_by_other.
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted, accept_latency_ms) VALUES ($1, 'b1', '{}', 'pending', false, NULL)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted, accept_latency_ms) VALUES ($1, 'b2', '{}', 'accepted', true, 120)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted, accept_latency_ms) VALUES ($1, 'b3', '{}', 'accepted', false, NULL)")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status, auto_accepted) VALUES ($1, 'b4', jsonb_build_object('accept_reason', 'taken_by_other'), 'failed', false)")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let s = store::bookings::summary(&pool, tenant_id).await.expect("summary");
    assert_eq!(s.incoming_today, 4);
    assert_eq!(s.accepted_auto_today, 1);
    assert_eq!(s.accepted_manual_today, 1);
    assert_eq!(s.taken_by_other_today, 1);
    assert_eq!(s.latency_p99_ms, Some(120.0));
}

#[tokio::test]
async fn summary_latency_is_none_with_no_auto_accepts_today() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data, status) VALUES ($1, 'b1', '{}', 'pending')")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let s = store::bookings::summary(&pool, tenant_id).await.expect("summary");
    assert_eq!(s.latency_p99_ms, None);
}

#[tokio::test]
async fn list_vehicle_types_returns_distinct_non_null_sorted() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v1', jsonb_build_object('vehicle_type_name', 'TRONTON'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v2', jsonb_build_object('vehicle_type_name', 'CDD'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v3', jsonb_build_object('vehicle_type_name', 'TRONTON'))")
        .bind(tenant_id).execute(&pool).await.expect("insert");
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, 'v4', '{}')")
        .bind(tenant_id).execute(&pool).await.expect("insert");

    let types = store::bookings::list_vehicle_types(&pool, tenant_id).await.expect("list");
    assert_eq!(types, vec!["CDD".to_string(), "TRONTON".to_string()]);
}
