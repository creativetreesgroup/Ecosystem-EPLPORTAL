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
pub async fn list_all(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<AgencyCredential>, sqlx::Error> {
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
