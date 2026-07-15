// Backend/crates/store/src/automation_settings.rs
//! `automation_settings` — one row per tenant, the home of the `autoAccept` GLOBAL kill switch
//! (Aturan Keras #2). Fase 6c only touches `auto_accept_enabled`; every other column
//! (`smart_*`, `counter_reset_*`) is out of this sub-phase's scope (6d/later) and this module
//! deliberately does not expose a way to change them yet — `set_auto_accept_enabled` is a
//! narrow, single-column write, not a general-purpose upsert.
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::AutomationSettings;

pub async fn get(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<AutomationSettings>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AutomationSettings>(
        "SELECT tenant_id, auto_accept_enabled, poll_interval_ms, smart_paused, \
         smart_paused_until, smart_dry_run, smart_schedule, smart_blacklist, \
         counter_reset_hour, counter_reset_last_at, updated_at \
         FROM automation_settings WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// `INSERT ... ON CONFLICT (tenant_id) DO UPDATE` — a tenant's `automation_settings` row may or
/// may not exist yet (nothing has created one before Fase 6c; the schema ships no default row
/// per tenant). Every other column keeps its existing value (or the schema default, on first
/// insert) — only `auto_accept_enabled` is ever written by this fn.
pub async fn set_auto_accept_enabled(
    pool: &PgPool,
    tenant_id: Uuid,
    enabled: bool,
) -> Result<AutomationSettings, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AutomationSettings>(
        "INSERT INTO automation_settings (tenant_id, auto_accept_enabled) VALUES ($1, $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET auto_accept_enabled = $2, updated_at = now() \
         RETURNING tenant_id, auto_accept_enabled, poll_interval_ms, smart_paused, \
           smart_paused_until, smart_dry_run, smart_schedule, smart_blacklist, \
           counter_reset_hour, counter_reset_last_at, updated_at",
    )
    .bind(tenant_id)
    .bind(enabled)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}
