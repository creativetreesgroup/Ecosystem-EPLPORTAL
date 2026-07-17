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

use std::future::Future;
use std::pin::Pin;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use axum_extra::extract::CookieJar;
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
            // Determine emptiness with the `get_mut` guard dropped before any
            // removal attempt: DashMap's per-shard lock isn't reentrant, so
            // calling `remove`/`remove_if` on `ch` while still holding this
            // guard would deadlock.
            let now_empty = match self.clients.get_mut(ch) {
                Some(mut set) => {
                    set.remove(&id);
                    set.is_empty()
                }
                None => false,
            };
            if now_empty {
                // Re-check emptiness under `remove_if` (which re-locks the
                // shard) rather than an unconditional `remove`, so a
                // concurrent `register()` that raced in between — adding a
                // new socket to what was momentarily an empty map — isn't
                // wiped out from under it.
                self.clients.remove_if(ch, |_, set| set.is_empty());
            }
        }
    }

    /// Introspection accessor: does the registry still hold an entry for
    /// `ch`? Exists mainly so integration tests can prove `unregister`
    /// reclaims fully-empty channel entries instead of leaking them forever
    /// (see review finding on Task 12); not otherwise used by the hub itself.
    pub fn has_channel(&self, ch: &str) -> bool {
        self.clients.contains_key(ch)
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

    /// Test-only: register a sender directly under `channel`, bypassing the
    /// real WS upgrade handshake, so integration tests (Task 13's cross-process
    /// Redis bridge test) can observe `deliver`/`deliver_broadcast` without
    /// spinning up a real socket. Gated on `feature = "test-helpers"` (enabled
    /// for `tests/*.rs` via the self dev-dependency in Cargo.toml) as well as
    /// plain `cfg(test)` for any future in-crate unit test.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn test_register(&self, channel: &str, tx: UnboundedSender<Message>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.register(channel, id, tx);
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

// --- Session-validated upgrade path (Fase 6a Task 10) ----------------------
//
// Additive alongside `ws_handler`/`ws_router` above (Fase 5, Task 12/13):
// those keep their existing no-auth signature/behavior UNCHANGED (their own
// tests — `local_broadcast.rs`, `redis_bridge.rs`, `registry_cleanup.rs` —
// call `ws_router(hub)` directly and must keep compiling and passing
// unmodified). A caller that wants real session validation opts into the
// NEW functions below instead.

/// Takes the plaintext `?session=` query value and resolves whether it names
/// a currently valid (existing, unexpired) session. A boxed async closure
/// rather than a trait: this hook has exactly one production caller
/// (`reactor-core`'s `main()`, wiring `store::portal_sessions::find_valid_by_hash`)
/// and one test caller, so a full trait hierarchy would be over-engineering
/// for a single call site — see the task brief's own sketch, which this
/// mirrors. The boxed future lets the closure `.await` a real `sqlx` query
/// without `ws_handler_with_auth`/`ws_router_with_auth` needing to be generic
/// over the future type.
pub type SessionValidator =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Validated upgrade path. Rejects with `401 Unauthorized` BEFORE
/// `ws.on_upgrade` ever runs for a missing/empty/invalid/expired session —
/// the WS handshake never completes in that case, rather than accepting
/// the connection and immediately closing it, per the task brief's
/// requirement that the client see a clean non-101 HTTP response.
///
/// **Fase 7a addition:** the session token can now come from EITHER the
/// `?session=` query param (unchanged, kept for test/tooling convenience —
/// `session_validated_ws.rs`'s existing tests still use it directly) OR the
/// `cookie_name`-named `HttpOnly` cookie a real browser sends automatically
/// (closing the Fase-6a tracked gap: a browser has no way to read an
/// `HttpOnly` cookie's value to construct a `?session=` query string itself).
/// The query param wins if BOTH are present (keeps existing test behavior
/// byte-identical); the cookie is used ONLY when the query param is empty.
pub async fn ws_handler_with_auth(
    ws: WebSocketUpgrade,
    State((hub, validator, cookie_name)): State<(Arc<Hub>, SessionValidator, Arc<str>)>,
    Query(mut q): Query<WsQuery>,
    jar: CookieJar,
) -> Response {
    if q.session.is_empty() {
        if let Some(cookie) = jar.get(&cookie_name) {
            q.session = cookie.value().to_string();
        }
    }
    if q.session.is_empty() || !(validator)(q.session.clone()).await {
        return (StatusCode::UNAUTHORIZED, "invalid session").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, hub, q))
}

/// Same `/ws` route shape as `ws_router` (existing, unchanged, no-auth) but
/// requiring `validator` to confirm a session (from `?session=` OR the
/// `cookie_name` cookie) is valid before the handshake completes.
/// Authentication only — no `is_main_account`/RBAC check here, per the task
/// brief: any valid, logged-in session may open a WS connection.
pub fn ws_router_with_auth(
    hub: Arc<Hub>,
    validator: SessionValidator,
    cookie_name: Arc<str>,
) -> Router {
    Router::new()
        .route("/ws", get(ws_handler_with_auth))
        .with_state((hub, validator, cookie_name))
}
