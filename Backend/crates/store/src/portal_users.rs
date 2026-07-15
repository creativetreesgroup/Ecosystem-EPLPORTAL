// Backend/crates/store/src/portal_users.rs
//! Portal-user lookups. Tenant-scoped — every query here runs inside
//! `begin_tenant_tx` (the tenant is already known by the time login/session
//! code calls into this module; `tenants::find_by_slug` resolves it first).
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::PortalUser;

pub async fn find_by_username(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
) -> Result<Option<PortalUser>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, PortalUser>(
        "SELECT id, tenant_id, username, password_hash, display_name, is_main_account, \
         enabled, created_at, updated_at FROM portal_users \
         WHERE tenant_id = $1 AND username = $2",
    )
    .bind(tenant_id)
    .bind(username)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// By-id lookup, same tenant-scoped `begin_tenant_tx` pattern as
/// `find_by_username`. Added for the session-auth middleware (Fase 6a Task
/// 3): a validated `portal_sessions` row only carries `portal_user_id`, not
/// a username, so the middleware needs this by-id shape to build
/// `CurrentUser`.
pub async fn find_by_id(
    pool: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<PortalUser>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, PortalUser>(
        "SELECT id, tenant_id, username, password_hash, display_name, is_main_account, \
         enabled, created_at, updated_at FROM portal_users \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Inserts a new `portal_users` row (Fase 6b's sub-user CRUD,
/// `POST /auth/portal-users`). `password_hash` is already hashed by the
/// caller (this crate does no hashing of its own, same layering
/// `find_by_username`'s auth-middleware caller already relies on) — `create`
/// only ever persists the hash. A duplicate `(tenant_id, username)` surfaces
/// as `sqlx::Error::Database` with code `23505`, deliberately NOT
/// special-cased here — `api-gateway`'s `ApiError: From<sqlx::Error>` (Fase
/// 6a Task 1) already maps that code to `409 Conflict`, same pattern as
/// `agency_credentials::create`.
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
    password_hash: &str,
    display_name: &str,
    is_main_account: bool,
) -> Result<PortalUser, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, PortalUser>(
        "INSERT INTO portal_users (tenant_id, username, password_hash, display_name, is_main_account) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, tenant_id, username, password_hash, display_name, is_main_account, enabled, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(username)
    .bind(password_hash)
    .bind(display_name)
    .bind(is_main_account)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Every `portal_users` row for `tenant_id`, ordered by `created_at` (oldest
/// first — stable listing order for `GET /auth/portal-users`).
pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<PortalUser>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, PortalUser>(
        "SELECT id, tenant_id, username, password_hash, display_name, is_main_account, \
         enabled, created_at, updated_at FROM portal_users WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Deletes a `portal_users` row by `(tenant_id, id)`. `true` if a row was
/// actually deleted, `false` if no such row existed (the caller maps that to
/// `404`). Whether a sub-user may delete THEMSELVES, or delete the last
/// remaining main account, is an RBAC/handler-level concern (Fase 6b's
/// `require_permission`), not this fn's — `store` stays a thin CRUD layer.
pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM portal_users WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
