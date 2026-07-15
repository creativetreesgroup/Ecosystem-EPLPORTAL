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
