use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SiteSetting {
    pub tenant_id: Uuid,
    pub key: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}
