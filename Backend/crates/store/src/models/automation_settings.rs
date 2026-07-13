use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AutomationSettings {
    pub tenant_id: Uuid,
    /// Aturan Keras #2 — GLOBAL kill switch. Schema default is `false`;
    /// nothing in this crate ever flips it implicitly.
    pub auto_accept_enabled: bool,
    pub poll_interval_ms: i32,
    pub smart_paused: bool,
    pub smart_paused_until: Option<DateTime<Utc>>,
    pub smart_dry_run: bool,
    pub smart_schedule: Value,
    pub smart_blacklist: Vec<String>,
    pub counter_reset_hour: Option<i32>,
    pub counter_reset_last_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}
