//! Real-Postgres tests for migration 0021's generated columns — this project's standing
//! testing convention is a real database, not mocks (see store's other integration tests).
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
        .bind("Spx Derived Columns Test Tenant")
        .bind(format!("spx-derived-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}

async fn insert_booking(pool: &PgPool, tenant_id: Uuid, spx_id: &str, raw: serde_json::Value) {
    sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(spx_id)
        .bind(raw)
        .execute(pool)
        .await
        .expect("insert booking");
}

#[tokio::test]
async fn vehicle_type_prefers_name_and_discards_numeric_code() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "veh-name", serde_json::json!({"vehicle_type_name": "CDD LONG (6WH)", "vehicle_type": "3"})).await;
    insert_booking(&pool, tenant_id, "veh-numeric", serde_json::json!({"vehicle_type": "3"})).await;

    let name: Option<String> = sqlx::query_scalar("SELECT spx_vehicle_type FROM bookings WHERE tenant_id = $1 AND spx_id = 'veh-name'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(name.as_deref(), Some("CDD LONG (6WH)"));

    let numeric: Option<String> = sqlx::query_scalar("SELECT spx_vehicle_type FROM bookings WHERE tenant_id = $1 AND spx_id = 'veh-numeric'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(numeric, None);
}

#[tokio::test]
async fn deadline_at_converts_seconds_and_ms_correctly() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    // Seconds (<= 1e12): 1_800_000_000 seconds.
    insert_booking(&pool, tenant_id, "ddl-seconds", serde_json::json!({"deadline_at": 1_800_000_000})).await;
    // Already ms (> 1e12): 1_800_000_000_000 ms == the same instant.
    insert_booking(&pool, tenant_id, "ddl-ms", serde_json::json!({"deadline_at": 1_800_000_000_000i64})).await;
    // Zero -> NULL (no real deadline).
    insert_booking(&pool, tenant_id, "ddl-zero", serde_json::json!({"deadline_at": 0})).await;

    let seconds: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT spx_deadline_at FROM bookings WHERE tenant_id = $1 AND spx_id = 'ddl-seconds'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    let ms: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT spx_deadline_at FROM bookings WHERE tenant_id = $1 AND spx_id = 'ddl-ms'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(seconds, ms);

    let zero: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT spx_deadline_at FROM bookings WHERE tenant_id = $1 AND spx_id = 'ddl-zero'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(zero, None);
}

#[tokio::test]
async fn pickup_time_falls_back_to_deadline_at() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "no-pickup-key", serde_json::json!({"deadline_at": 1_800_000_000})).await;

    let (deadline, pickup): (Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT spx_deadline_at, spx_pickup_time FROM bookings WHERE tenant_id = $1 AND spx_id = 'no-pickup-key'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("query");
    assert_eq!(deadline, pickup);
}

#[tokio::test]
async fn tx_id_falls_back_to_spx_id_when_no_name_key_present() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPXID_FALLBACK_1", serde_json::json!({})).await;

    let tx_id: String = sqlx::query_scalar("SELECT spx_tx_id FROM bookings WHERE tenant_id = $1 AND spx_id = 'SPXID_FALLBACK_1'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(tx_id, "SPXID_FALLBACK_1");
}

#[tokio::test]
async fn origin_dest_station_reads_first_and_last_route_node() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "route-stations", serde_json::json!({
        "route_detail_list": [
            {"node_info_list": [{"name": "Cikarang DC"}]},
            {"node_info_list": [{"name": "Semarang DC"}]}
        ]
    })).await;

    let (origin, dest): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT spx_origin_station, spx_dest_station FROM bookings WHERE tenant_id = $1 AND spx_id = 'route-stations'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("query");
    assert_eq!(origin.as_deref(), Some("Cikarang DC"));
    assert_eq!(dest.as_deref(), Some("Semarang DC"));
}

#[tokio::test]
async fn trip_type_absent_is_null_not_zero() {
    let pool = test_pool().await;
    let tenant_id = seed_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "no-trip-type", serde_json::json!({})).await;

    let trip_type: Option<i32> = sqlx::query_scalar("SELECT spx_trip_type FROM bookings WHERE tenant_id = $1 AND spx_id = 'no-trip-type'")
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("query");
    assert_eq!(trip_type, None);
}
