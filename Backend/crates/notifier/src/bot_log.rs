// Backend/crates/notifier/src/bot_log.rs
//! Redis-backed bot-activity audit log (`spx:bot:logs:<tenant_id>`, `LPUSH`+`LTRIM`, capped at 200
//! entries) — mirrors the reference's own `recordBotLog`/`BOT_LOGS_KEY` mechanism (Fase 6d Task 7).
//! Deliberately NOT wired automatically into `notify_accepted`/`notify_agency_loss`/
//! `waha::send_to_waha_many` — those already-shipped fns' signatures stay untouched; `record` is
//! called EXPLICITLY by each caller immediately after its own existing notify call, keeping this
//! crate's core send path free of a new Redis dependency while still sharing one logging fn.
//!
//! The key is tenant-scoped (`spx:bot:logs:<tenant_id>`, not a single global `spx:bot:logs`) —
//! TOWER is multi-tenant with RLS enforced on every Postgres table, and a single shared key would
//! let any tenant's `ManageBotSettings` user read every OTHER tenant's booking IDs and rival-agency
//! emails via `GET /bot/logs` (review finding, closed by this tenant-scoping).
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

const MAX_LOGS: isize = 200;

fn bot_logs_key(tenant_id: uuid::Uuid) -> String {
    format!("spx:bot:logs:{tenant_id}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotLogEntry {
    pub ts: i64,
    /// `"success"` | `"error"`.
    pub log_type: String,
    /// `"accept"` | `"agency_loss"` | `"otp"` — `None` for a kind this task doesn't produce yet.
    pub kind: Option<String>,
    pub booking_id: Option<String>,
    pub latency_ms: Option<i64>,
    pub rule: Option<String>,
    pub error: Option<String>,
}

/// Best-effort — a serialization or Redis failure here must never propagate to the caller's own
/// (more important) notify call; every error is silently dropped, matching this crate's
/// established fire-and-forget tolerance for anything Redis/WAHA-adjacent. Tenant-scoped: writes
/// to `spx:bot:logs:<tenant_id>`, not a global key.
pub async fn record(redis: &mut ConnectionManager, tenant_id: uuid::Uuid, entry: &BotLogEntry) {
    let key = bot_logs_key(tenant_id);
    let Ok(serialized) = serde_json::to_string(entry) else {
        return;
    };
    let _: Result<i64, redis::RedisError> = redis.lpush(&key, &serialized).await;
    let _: Result<(), redis::RedisError> = redis.ltrim(&key, 0, MAX_LOGS - 1).await;
}

/// Newest-first (LPUSH prepends, so index 0 is already the newest — no `ORDER BY` needed, unlike
/// every Postgres-backed list fn in this workspace). `limit` is clamped to `[1, MAX_LOGS]`.
/// Tenant-scoped: reads from `spx:bot:logs:<tenant_id>`, not a global key.
pub async fn list(redis: &mut ConnectionManager, tenant_id: uuid::Uuid, limit: isize) -> Vec<BotLogEntry> {
    let key = bot_logs_key(tenant_id);
    let clamped = limit.clamp(1, MAX_LOGS);
    let raw: Result<Vec<String>, redis::RedisError> =
        redis.lrange(&key, 0, clamped - 1).await;
    raw.unwrap_or_default()
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect()
}

/// Tenant-scoped: deletes `spx:bot:logs:<tenant_id>`, not a global key.
pub async fn clear(redis: &mut ConnectionManager, tenant_id: uuid::Uuid) {
    let key = bot_logs_key(tenant_id);
    let _: Result<i64, redis::RedisError> = redis.del(&key).await;
}
