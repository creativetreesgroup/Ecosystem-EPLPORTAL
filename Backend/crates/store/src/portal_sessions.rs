// Backend/crates/store/src/portal_sessions.rs
//! Opaque session issuance/lookup/revocation. `token_hash` is always the
//! SHA-256 of the plaintext cookie token (`spx_client::crypto::session_token`)
//! — this crate never sees or stores a plaintext token.
use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::PortalSession;

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    portal_user_id: Uuid,
    token_hash: [u8; 32],
    ip: Option<&str>,
    user_agent: Option<&str>,
    ttl: Duration,
) -> Result<PortalSession, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let expires_at = Utc::now() + ttl;
    let row = sqlx::query_as::<_, PortalSession>(
        "INSERT INTO portal_sessions \
         (tenant_id, portal_user_id, token_hash, ip, user_agent, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, tenant_id, portal_user_id, token_hash, ip, user_agent, \
                   created_at, expires_at, last_seen_at",
    )
    .bind(tenant_id)
    .bind(portal_user_id)
    .bind(token_hash.as_slice())
    .bind(ip)
    .bind(user_agent)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Looks up by hash alone (globally unique per the schema) — tenant is read
/// FROM the result, not used to find it (the caller doesn't know the tenant
/// yet at this point in the auth-middleware flow). Runs OUTSIDE
/// `begin_tenant_tx`, via the `portal_sessions_find_valid_by_hash` SQL
/// function (migration `0018_portal_sessions_lookup_by_hash_fn.sql`) rather
/// than a plain `SELECT` against the base table: unlike `tenants`,
/// `portal_sessions` genuinely IS RLS-protected, so once the production
/// pool authenticates as `app_role` (Fase 6a Task 9) a plain `SELECT` here
/// with no `app.tenant_id` set would silently return zero rows for every
/// login attempt. The `SECURITY DEFINER` function bypasses that narrowly,
/// for exactly this lookup shape only — see the migration's own comment for
/// the full reasoning, and
/// `portal_sessions_find_valid_by_hash_fn_works_for_app_role_with_no_tenant_context`
/// in `lib.rs`'s test module for a proof that both the block and the
/// carve-out actually behave as intended.
pub async fn find_valid_by_hash(
    pool: &PgPool,
    token_hash: [u8; 32],
) -> Result<Option<PortalSession>, sqlx::Error> {
    sqlx::query_as::<_, PortalSession>("SELECT * FROM portal_sessions_find_valid_by_hash($1)")
        .bind(token_hash.as_slice())
        .fetch_optional(pool)
        .await
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, session_id: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query("DELETE FROM portal_sessions WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn touch_last_seen(
    pool: &PgPool,
    tenant_id: Uuid,
    session_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query("UPDATE portal_sessions SET last_seen_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
