// Backend/crates/ws-hub/src/hub.rs
//! Local socket registry + the axum WS upgrade handler. Each socket registers
//! under its session id AND (if present) `acct:<account_id>` (lowercased) so
//! every device of an account gets the same live updates (correction #8). Two
//! tasks per socket: a recv loop (Pong/Close) and a send loop (mpsc forward +
//! 30s ping). The Redis bridge (Task 13) calls `deliver`.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::events::WsEvent;

const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Local registry: channel → (socket id → sender). Channel is a session id or
/// `acct:<account_id>`.
pub struct Hub {
    clients: DashMap<String, HashMap<u64, UnboundedSender<Message>>>,
    next_id: AtomicU64,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { clients: DashMap::new(), next_id: AtomicU64::new(1) })
    }

    fn register(&self, channel: &str, id: u64, tx: UnboundedSender<Message>) {
        self.clients.entry(channel.to_string()).or_default().insert(id, tx);
    }

    fn unregister(&self, channels: &[String], id: u64) {
        for ch in channels {
            if let Some(mut set) = self.clients.get_mut(ch) {
                set.remove(&id);
            }
        }
    }

    /// Deliver a payload to every socket on `channel`.
    pub fn deliver(&self, channel: &str, payload: &str) {
        if let Some(set) = self.clients.get(channel) {
            for tx in set.values() {
                let _ = tx.send(Message::Text(payload.to_string().into()));
            }
        }
    }

    /// Deliver to ALL sockets (broadcast channel).
    pub fn deliver_broadcast(&self, payload: &str) {
        for set in self.clients.iter() {
            for tx in set.value().values() {
                let _ = tx.send(Message::Text(payload.to_string().into()));
            }
        }
    }

    pub fn deliver_event(&self, channel: &str, ev: &WsEvent) {
        self.deliver(channel, &ev.to_json());
    }
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub account: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(hub): State<Arc<Hub>>,
    Query(q): Query<WsQuery>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, hub, q))
}

async fn handle_socket(socket: WebSocket, hub: Arc<Hub>, q: WsQuery) {
    let id = hub.next_id.fetch_add(1, Ordering::Relaxed);
    // Channels this socket belongs to: its session, and (if any) its account.
    let mut channels: Vec<String> = Vec::new();
    if !q.session.is_empty() {
        channels.push(q.session.clone());
    }
    if !q.account.is_empty() {
        channels.push(format!("acct:{}", q.account.to_lowercase()));
    }
    if channels.is_empty() {
        channels.push(format!("anon:{id}"));
    }

    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    for ch in &channels {
        hub.register(ch, id, tx.clone());
    }

    // Greet with a `connected` event.
    let _ = tx.send(Message::Text(
        WsEvent::Connected { session: q.session.clone() }.to_json().into(),
    ));

    // Send task: forward mpsc messages + a 30s ping.
    let send_task = tokio::spawn(async move {
        // `interval()`'s first tick fires immediately, which would send a
        // spurious Ping right after the `connected` greeting; `interval_at`
        // with a `PING_INTERVAL`-away start defers the first real ping to
        // 30s out, as intended.
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => { if sink.send(m).await.is_err() { break; } }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                }
            }
        }
    });

    // Recv loop: drain until close/error (Pong handled implicitly by axum).
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Close(_) = msg {
            break;
        }
    }

    // Cleanup.
    send_task.abort();
    hub.unregister(&channels, id);
}

pub fn ws_router(hub: Arc<Hub>) -> Router {
    Router::new().route("/ws", get(ws_handler)).with_state(hub)
}
