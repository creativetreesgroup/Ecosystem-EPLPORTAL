// Backend/crates/ws-hub/tests/registry_cleanup.rs
//! A real WS client connects with no `session`/`account` query param, so it
//! registers under the anonymous channel `anon:<id>`. After the client
//! disconnects, `Hub::unregister` must reclaim the now-empty channel entry
//! rather than leaking it forever (review finding on Task 12). Proves the fix
//! in `Hub::unregister` by polling `Hub::has_channel` until it goes false.
use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message as CM;
use ws_hub::{ws_router, Hub};

#[tokio::test]
async fn anon_channel_is_reclaimed_after_disconnect() {
    let hub = Hub::new();
    let app = ws_router(hub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // No `session`/`account` query param -> registers under `anon:1` (this is
    // the first and only anonymous connection on a fresh Hub, whose next_id
    // counter starts at 1).
    let url = format!("ws://{addr}/ws");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    // First frame is the `connected` greeting; reading it confirms the
    // server has finished registering before we tear the socket down.
    let first = ws.next().await.unwrap().unwrap();
    assert!(matches!(first, CM::Text(ref t) if t.contains("connected")));

    assert!(hub.has_channel("anon:1"), "channel should exist while socket is connected");

    // Explicitly close the client side to trigger a real disconnect.
    ws.close(None).await.unwrap();
    drop(ws);

    // Poll for the entry to be reclaimed rather than a single fixed sleep:
    // the server needs a moment to notice the disconnect and run its cleanup.
    let reclaimed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !hub.has_channel("anon:1") {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(false);

    assert!(reclaimed, "anon:1 channel entry should be reclaimed after disconnect, not leaked");
}
