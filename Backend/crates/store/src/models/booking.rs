use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub tenant_id: Uuid,
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
