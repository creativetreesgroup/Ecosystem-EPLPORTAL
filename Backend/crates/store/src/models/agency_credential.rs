use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgencyCredential {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub label: String,
    pub username: String,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub key_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
