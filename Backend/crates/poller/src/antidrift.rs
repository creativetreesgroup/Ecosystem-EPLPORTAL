// Backend/crates/poller/src/antidrift.rs
//! Anti-drift, gated by the `FetchOutcome` TYPE. `resurrect_pending`/
//! `expire_stale_bookings` are reachable ONLY through `run_anti_drift`, which
//! takes a `&FetchOutcome` (never a raw `HashSet`) and runs them ONLY when
//! `fetch_complete` (correction #9). A rotating-window or page-failed sweep is
//! `fetch_complete=false`, so it does nothing — a partial view can never expire
//! a live ticket.
use uuid::Uuid;

use crate::fetch::FetchOutcome;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store error: {0}")]
    Db(String),
}

/// Run anti-drift for one completed sweep. NO-OP unless `outcome.fetch_complete`.
pub async fn run_anti_drift(
    pool: &store::PgPool,
    tenant_id: Uuid,
    outcome: &FetchOutcome,
) -> Result<(), StoreError> {
    // The gate: a partial sweep (rotating window, or a full sweep with page
    // failures) is NEVER the basis for expire/resurrect.
    if !outcome.fetch_complete {
        return Ok(());
    }
    let active = &outcome.spx_id_set;
    let seen: Vec<String> = active.iter().cloned().collect();

    // Resurrect first (flip mistakenly-failed rows we positively see back to
    // pending), THEN expire (mark pending rows we NO LONGER see as failed).
    store::resurrect_pending(pool, tenant_id, &seen)
        .await
        .map_err(|e| StoreError::Db(e.to_string()))?;
    store::expire_stale_bookings(pool, tenant_id, active)
        .await
        .map_err(|e| StoreError::Db(e.to_string()))?;
    Ok(())
}
