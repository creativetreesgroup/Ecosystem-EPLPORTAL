// Backend/crates/store/src/site_settings.rs
//! Full CRUD (`get`/`put`/`delete`/`list`) for the generic tenant-scoped `site_settings`
//! key/value store (migration 0012, PK `(tenant_id, key)`). `get` shipped in Fase 6b (needed
//! before any writer existed); `put`/`delete`/`list` complete the set in Fase 6d, this table's
//! first two real writers (`waha_settings` extended by Task 6, `price_page`/branding by Task 8).
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Fetches a single `site_settings` row's `value` JSONB by `(tenant_id,
/// key)`. `None` when no such row exists — expected in this sub-phase, since
/// nothing writes `site_settings` yet (6d's job). The caller decides what a
/// missing row means for its own use case.
pub async fn get(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<Option<Value>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row: Option<(Value,)> =
        sqlx::query_as("SELECT value FROM site_settings WHERE tenant_id = $1 AND key = $2")
            .bind(tenant_id)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(row.map(|(v,)| v))
}

/// Upserts a `site_settings` row — `INSERT ... ON CONFLICT (tenant_id, key) DO UPDATE`, since the
/// table's PK IS `(tenant_id, key)` (migration 0012). Every writer of this table (WAHA settings,
/// bot settings, branding) wants "set this key to this value, whether or not it existed before" —
/// no caller needs to distinguish create-vs-update, matching this crate's established
/// `agency_credentials`-adjacent "PUT is idempotent, always 200" convention.
pub async fn put(
    pool: &PgPool,
    tenant_id: Uuid,
    key: &str,
    value: &Value,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query(
        "INSERT INTO site_settings (tenant_id, key, value, updated_at) VALUES ($1, $2, $3, now()) \
         ON CONFLICT (tenant_id, key) DO UPDATE SET value = $3, updated_at = now()",
    )
    .bind(tenant_id)
    .bind(key)
    .bind(value)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `true` if a row existed and was deleted, `false` if no such `(tenant_id, key)` row existed.
pub async fn delete(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM site_settings WHERE tenant_id = $1 AND key = $2")
        .bind(tenant_id)
        .bind(key)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}

/// Every `(key, value)` pair for `tenant_id` — no consumer needs this yet in THIS plan, but
/// `GET /bot/settings`-adjacent admin tooling (or a future settings-export feature) is the
/// obvious future caller; added now since it's a two-line query and completes the CRUD verb set
/// this module's own doc comment already promised.
pub async fn list(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<(String, Value)>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows: Vec<(String, Value)> =
        sqlx::query_as("SELECT key, value FROM site_settings WHERE tenant_id = $1 ORDER BY key ASC")
            .bind(tenant_id)
            .fetch_all(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(rows)
}
