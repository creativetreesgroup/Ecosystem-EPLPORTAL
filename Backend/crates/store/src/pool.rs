use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new().max_connections(10).connect(database_url).await
}

pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

/// Begin a transaction with `app.tenant_id` set for its duration via
/// `set_config(..., true)` (the `true` = local-to-transaction, matching `SET
/// LOCAL` semantics but parameter-bindable, unlike `SET LOCAL` itself). Every
/// tenant-scoped query MUST go through this — Row-Level Security policies key
/// off `current_setting('app.tenant_id', true)`, so a bare pool connection
/// sees no rows in any tenant-scoped table (RLS defaults to "no match" when
/// the setting is unset).
pub async fn begin_tenant_tx(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
