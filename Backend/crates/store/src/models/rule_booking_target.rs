use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuleBookingTarget {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub rule_id: Uuid,
    pub booking_id_raw: String,
    pub booking_id_norm: String,
    pub created_at: DateTime<Utc>,
}
