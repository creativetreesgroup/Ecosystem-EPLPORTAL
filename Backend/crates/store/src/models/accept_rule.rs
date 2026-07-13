use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AcceptRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: String,
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub origin: String,
    pub destinations: Vec<String>,
    pub booking_type: String,
    pub shift_types: Vec<i32>,
    pub trip_types: Vec<i32>,
    pub match_mode: String,
    pub min_deadline_min: Option<i32>,
    pub max_accept_count: i32,
    pub accepted_count: i32,
    pub route_signature: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
