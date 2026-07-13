use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PortalUser {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub display_name: String,
    pub is_main_account: bool,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
