use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub raw_data: Value,
    pub status: String,
    /// Read-only — computed by Postgres, never set on INSERT/UPDATE.
    pub is_coc: bool,
    /// Read-only — computed by Postgres, never set on INSERT/UPDATE.
    pub needs_enrichment: bool,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub accept_latency_ms: Option<i32>,
    pub rule_matched: Option<Uuid>,
    /// Read-only — generated column (migration 0021), derived from `raw_data`.
    pub spx_request_id: Option<String>,
    /// Read-only — generated column (migration 0021), derived from `raw_data`.
    pub spx_onsite_id: Option<String>,
    /// Read-only — generated column (migration 0021), derived from `raw_data`;
    /// never NULL — the migration's `COALESCE(..., spx_id)` guarantees a value.
    pub spx_tx_id: String,
    /// Read-only — generated column (migration 0021), derived from `raw_data`.
    pub spx_vehicle_type: Option<String>,
    /// Read-only — generated column (migration 0021), derived from `raw_data`.
    pub spx_deadline_at: Option<DateTime<Utc>>,
    /// Read-only — generated column (migration 0021), derived from `raw_data`.
    pub spx_pickup_time: Option<DateTime<Utc>>,
    /// Read-only — generated column (migration 0021), derived from `raw_data`.
    pub spx_trip_type: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
