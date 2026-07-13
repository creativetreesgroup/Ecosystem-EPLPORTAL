use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PushSubscription {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portal_user_id: Uuid,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
