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
}
