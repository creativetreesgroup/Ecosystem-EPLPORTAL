// Backend/crates/store/src/site_settings.rs
//! Minimal READ-ONLY accessor for `site_settings` — Fase 6b Task 5's
//! `api-gateway::routes::otp::load_bot_settings` needs to fetch the
//! `waha_settings` row (Fase 3's `spx_client::waha_settings::WahaSettings`
//! JSONB shape) before 6d's own `site_settings` CRUD route ships. Tenant-
//! scoped via `begin_tenant_tx`, same discipline as every other module in
//! this crate (see `pool::begin_tenant_tx`'s doc comment).
//!
//! Deliberately just this one `get` fn, not a full CRUD module — `create`/
//! `update`/`upsert`/`list`/`delete` for `site_settings` are 6d's job (the
//! master-spec `GET/PUT /bot/settings` route and friends). Mirrors this
//! plan's own precedent: Fase 6a Task 9 read `agency_credentials` for the
//! account-bootstrap loop long before that table got its own CRUD route.
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
