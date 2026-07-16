// Backend/crates/store/src/route_locations.rs
//! `route_locations` CRUD. Add/delete only — the table has no `updated_at`
//! column (migration 0014), matching the reference's own behavior: a
//! location's `name` is either right or gets deleted and re-added, never
//! edited in place.
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::RouteLocation;

pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<RouteLocation>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, RouteLocation>(
        "SELECT id, tenant_id, name, created_at FROM route_locations \
         WHERE tenant_id = $1 ORDER BY name ASC",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// A duplicate `(tenant_id, name)` surfaces as `23505` via `?`, mapped to `409`.
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    name: &str,
) -> Result<RouteLocation, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, RouteLocation>(
        "INSERT INTO route_locations (tenant_id, name) VALUES ($1, $2) \
         RETURNING id, tenant_id, name, created_at",
    )
    .bind(tenant_id)
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM route_locations WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
