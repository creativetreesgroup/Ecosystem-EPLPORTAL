// Backend/crates/ws-hub/tests/local_broadcast.rs
//! A real WS client connects with ?account=ACC; a Hub::deliver on channel
//! `acct:acc` reaches that socket. Proves the registry + upgrade + send path
//! (no Redis yet — that is Task 13's cross-process test).
use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message as CM;
use ws_hub::{ws_router, Hub};

#[tokio::test]
async fn account_channel_delivers_to_connected_socket() {
    let hub = Hub::new();
    let app = ws_router(hub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let url = format!("ws://{addr}/ws?account=ACC");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    // First frame is the `connected` greeting.
    let first = ws.next().await.unwrap().unwrap();
    assert!(matches!(first, CM::Text(ref t) if t.contains("connected")));

    // Give the server a beat to finish registering, then deliver on acct:acc.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    hub.deliver("acct:acc", r#"{"type":"tickets_removed","data":{"ids":["x"]}}"#);

    let got = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("delivered within 2s")
        .unwrap()
        .unwrap();
    assert!(matches!(got, CM::Text(ref t) if t.contains("tickets_removed")));
}
