use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoutePrice {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub route_code: String,
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
