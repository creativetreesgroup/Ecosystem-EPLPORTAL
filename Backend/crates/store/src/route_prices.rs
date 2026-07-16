// Backend/crates/store/src/route_prices.rs
//! `route_prices` CRUD. `destinations` is a JSONB array of 1-5 strings (schema
//! CHECK constraint `route_prices_destinations_1to5`, migration 0013) — this
//! module passes it through as `serde_json::Value` untouched; validating the
//! 1-5 count and shape is the ROUTE layer's job (Task 4), same "store trusts
//! its caller, the DB is the final backstop" convention every other
//! CHECK-constrained table in this crate already follows.
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::RoutePrice;

#[derive(Debug, Clone)]
pub struct NewRoutePrice {
    pub route_code: String,
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
}

pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<RoutePrice>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, RoutePrice>(
        "SELECT id, tenant_id, route_code, region, origin, destinations, price, vehicle_type, \
         created_at, updated_at FROM route_prices WHERE tenant_id = $1 ORDER BY route_code ASC",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// A duplicate `(tenant_id, route_code)` surfaces as a real `23505`, propagated via `?` for
/// `ApiError::From<sqlx::Error>` to map to `409` — same non-special-casing convention as
/// `agency_credentials::create`.
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    new: &NewRoutePrice,
) -> Result<RoutePrice, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, RoutePrice>(
        "INSERT INTO route_prices (tenant_id, route_code, region, origin, destinations, price, vehicle_type) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, tenant_id, route_code, region, origin, destinations, price, vehicle_type, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(&new.route_code)
    .bind(&new.region)
    .bind(&new.origin)
    .bind(&new.destinations)
    .bind(new.price)
    .bind(&new.vehicle_type)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// `None` when no row matches `(tenant_id, id)` — caller maps that to `404`.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    pool: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
    new: &NewRoutePrice,
) -> Result<Option<RoutePrice>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, RoutePrice>(
        "UPDATE route_prices SET route_code = $3, region = $4, origin = $5, destinations = $6, \
         price = $7, vehicle_type = $8, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 \
         RETURNING id, tenant_id, route_code, region, origin, destinations, price, vehicle_type, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(id)
    .bind(&new.route_code)
    .bind(&new.region)
    .bind(&new.origin)
    .bind(&new.destinations)
    .bind(new.price)
    .bind(&new.vehicle_type)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM route_prices WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
