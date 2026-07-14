// Backend/crates/ws-hub/src/bridge.rs
//! Redis pub/sub → local broadcast. A dedicated PubSub connection subscribes to
//! `spx:ws:*`; each message's channel suffix selects the local `Hub` channel to
//! deliver to. This is what makes ws-hub work across processes (poller in
//! reactor-core publishes; every ws-hub instance delivers to its own sockets) —
//! correction #8, the one WS piece that is accurate 1:1 to the master spec.
//!
//! Verified against the pinned `redis` 1.3.0 source
//! (`~/.cargo/registry/src/.../redis-1.3.0`): `Client::get_async_pubsub` is
//! gated on feature `aio`, which both `tokio-comp` and `connection-manager`
//! (already enabled here) pull in; `aio::PubSub::psubscribe` exists and
//! returns `RedisResult<()>`; `on_message()` returns `impl Stream<Item = Msg>`;
//! `Msg::get_channel_name()` returns the ACTUAL channel a pattern-subscribed
//! message was published to (not the pattern), which is exactly the string we
//! need to strip `spx:ws:` off of.
use std::sync::Arc;

use futures::StreamExt;
use tokio::task::JoinHandle;

use crate::hub::Hub;

const PREFIX: &str = "spx:ws:";
const PATTERN: &str = "spx:ws:*";
const BROADCAST_SUFFIX: &str = "__broadcast__";

/// Spawn the bridge task. Returns an error only if the initial subscribe fails.
pub async fn spawn_bridge(hub: Arc<Hub>, redis_url: &str) -> Result<JoinHandle<()>, redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.psubscribe(PATTERN).await?;

    let handle = tokio::spawn(async move {
        let mut stream = pubsub.on_message();
        while let Some(msg) = stream.next().await {
            let channel = msg.get_channel_name().to_string();
            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let suffix = channel.strip_prefix(PREFIX).unwrap_or(&channel);
            if suffix == BROADCAST_SUFFIX {
                hub.deliver_broadcast(&payload);
            } else {
                hub.deliver(suffix, &payload);
            }
        }
    });
    Ok(handle)
}
