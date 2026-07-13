use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PortalSession {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portal_user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}
