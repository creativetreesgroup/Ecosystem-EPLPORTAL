// Backend/crates/poller/src/publish.rs
//! Poller-side WS event publisher. Publishes pre-serialized JSON to
//! `spx:ws:<channel>` so ws-hub's bridge (any process) delivers it to sockets.
//! Poller does NOT depend on ws-hub — the wire format is a shared CONTRACT (the
//! `{"type":..,"data":..}` shape, matching ws-hub's `WsEvent`'s
//! `#[serde(tag="type",content="data")]` serialization), not a shared type.
//! Fase 5 emits exactly two event types from the poller: `ticket_accepted`
//! (wired below, into `dispatch::finalize_win`) and `new_tickets` (NOT wired
//! in this task — see `dispatch.rs`'s module doc / the Task 13 report for why
//! there is no clean, already-computed "genuinely new booking" signal in the
//! current fetch/upsert pipeline to hang it off of).
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

#[derive(Clone)]
pub struct RedisPublisher {
    con: ConnectionManager,
}

impl RedisPublisher {
    pub async fn connect(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let con = ConnectionManager::new(client).await?;
        Ok(Self { con })
    }

    /// Publish a pre-serialized WS payload to `spx:ws:<channel>`.
    pub async fn publish(&self, channel: &str, payload: &str) {
        let mut con = self.con.clone();
        let full = format!("spx:ws:{channel}");
        let _: Result<i64, _> = con.publish(&full, payload).await;
    }

    /// Convenience: publish a `ticket_accepted` event to `acct:<id>`.
    pub async fn publish_ticket_accepted(&self, account_id: &str, data: serde_json::Value) {
        let payload = serde_json::json!({ "type": "ticket_accepted", "data": data }).to_string();
        self.publish(&format!("acct:{}", account_id.to_lowercase()), &payload).await;
    }

    /// Records one bot-activity log entry (Fase 6d Task 7) — reuses this struct's own
    /// `ConnectionManager`, same `.clone()`-then-use pattern every other method here already
    /// follows. `poller` already depends on `notifier` (`PollerShared.notifier`), so this adds
    /// no new Cargo.toml entry. `tenant_id` tenant-scopes the underlying Redis key (review
    /// finding — a single global key let any tenant read every other tenant's bot logs).
    pub async fn record_bot_log(&self, tenant_id: uuid::Uuid, entry: &notifier::bot_log::BotLogEntry) {
        let mut con = self.con.clone();
        notifier::bot_log::record(&mut con, tenant_id, entry).await;
    }
}
