// Backend/crates/store/src/agency_credentials.rs
//! Agency-credential lookups for the account-bootstrap loop (Fase 6a Task 9:
//! `reactor-core`'s `build_state()` spawns one poller task per row this
//! returns). Tenant-scoped — runs inside `begin_tenant_tx`, per this crate's
//! established discipline (see `pool::begin_tenant_tx`'s doc comment).
//!
//! `agency_credentials` (`migrations/0004_agency_credentials.sql`) carries no
//! "enabled" boolean column, so there is no `list_enabled` filter to apply
//! here (verified via `grep -n "agency_credentials" migrations/*.sql`) —
//! every row IS an account to bootstrap, full stop. Disabling one account
//! without deleting its row is 6b's CRUD scope, not this task's.
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::AgencyCredential;

/// Every `agency_credentials` row for `tenant_id`. `ciphertext`/`nonce` are
/// still encrypted here — decrypting the password (via
/// `spx_client::crypto::envelope::decrypt_agency_password`) is the caller's
/// job, same layering `agency_credentials_pg.rs`'s Fase 3 round-trip test
/// already established (this crate has no dependency on `spx-client`'s
/// crypto module and shouldn't grow one just to decrypt here).
pub async fn list_all(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Vec<AgencyCredential>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, AgencyCredential>(
        "SELECT id, tenant_id, label, username, ciphertext, nonce, key_version, \
         created_at, updated_at FROM agency_credentials WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Single row by `(tenant_id, label)` — Fase 6b's `GET /auth/spx-credentials`
/// read path (`label` is this table's natural per-account key, not a
/// synthetic list index).
pub async fn find_by_label(
    pool: &PgPool,
    tenant_id: Uuid,
    label: &str,
) -> Result<Option<AgencyCredential>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AgencyCredential>(
        "SELECT id, tenant_id, label, username, ciphertext, nonce, key_version, \
         created_at, updated_at FROM agency_credentials WHERE tenant_id = $1 AND label = $2",
    )
    .bind(tenant_id)
    .bind(label)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Inserts a new `agency_credentials` row. `ciphertext`/`nonce` are already
/// envelope-encrypted by the caller (`spx_client::crypto::envelope` — this
/// crate has no dependency on that module, see this file's top doc comment)
/// — `create` only ever persists opaque bytes, never plaintext. A duplicate
/// `(tenant_id, label)` surfaces as `sqlx::Error::Database` with code
/// `23505`; deliberately NOT special-cased here — `api-gateway`'s
/// `ApiError: From<sqlx::Error>` (Fase 6a Task 1) already maps that code to
/// `409 Conflict`, so this fn just lets it propagate via `?`.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    label: &str,
    username: &str,
    ciphertext: &[u8],
    nonce: &[u8],
    key_version: i32,
) -> Result<AgencyCredential, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AgencyCredential>(
        "INSERT INTO agency_credentials (tenant_id, label, username, ciphertext, nonce, key_version) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, tenant_id, label, username, ciphertext, nonce, key_version, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(label)
    .bind(username)
    .bind(ciphertext)
    .bind(nonce)
    .bind(key_version)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Replaces the username/ciphertext/nonce/key_version for an existing
/// `(tenant_id, label)` row (re-encrypt-and-save, e.g. `PUT
/// /auth/spx-credentials`). `None` when no row matched — the caller maps
/// that to `404`, mirroring `agency_credentials`'s sibling CRUD modules'
/// `Option`-return convention for "not found" rather than a bespoke error
/// variant.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    pool: &PgPool,
    tenant_id: Uuid,
    label: &str,
    username: &str,
    ciphertext: &[u8],
    nonce: &[u8],
    key_version: i32,
) -> Result<Option<AgencyCredential>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, AgencyCredential>(
        "UPDATE agency_credentials SET username = $3, ciphertext = $4, nonce = $5, \
         key_version = $6, updated_at = now() \
         WHERE tenant_id = $1 AND label = $2 \
         RETURNING id, tenant_id, label, username, ciphertext, nonce, key_version, created_at, updated_at",
    )
    .bind(tenant_id)
    .bind(label)
    .bind(username)
    .bind(ciphertext)
    .bind(nonce)
    .bind(key_version)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Deletes an `agency_credentials` row by `(tenant_id, label)`. `true` if a
/// row was actually deleted, `false` if no such row existed (the caller maps
/// that to `404`).
pub async fn delete(pool: &PgPool, tenant_id: Uuid, label: &str) -> Result<bool, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let result = sqlx::query("DELETE FROM agency_credentials WHERE tenant_id = $1 AND label = $2")
        .bind(tenant_id)
        .bind(label)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
