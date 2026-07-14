// Backend/crates/ws-hub/tests/redis_bridge.rs
//! DoD #9: prove the Redis bridge genuinely bridges across CONNECTIONS (not just
//! a local broadcast). Connection A (a publisher) PUBLISHes to spx:ws:acct:x;
//! the ws-hub bridge (its OWN separate PubSub connection) receives it and
//! delivers to a locally-registered socket. Two distinct Redis connections prove
//! cross-process behavior. Real Redis @ 16379.
use redis::AsyncCommands;
use ws_hub::{spawn_bridge, Hub};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn publish_on_one_connection_reaches_a_socket_via_the_bridge() {
    let hub = Hub::new();
    // Register a fake local socket on acct:x by reaching into Hub through a test
    // helper: deliver() sends to registered mpsc senders, so register one here.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    hub.test_register("acct:x", tx); // test-only helper (see hub.rs)

    // Start the bridge (its OWN PubSub connection).
    let _bridge = spawn_bridge(hub.clone(), &redis_url()).await.expect("bridge");
    // Bridge needs a beat to finish psubscribe before we publish.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Connection A: a SEPARATE publisher connection.
    let client = redis::Client::open(redis_url()).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let payload = r#"{"type":"ticket_accepted","data":{"bookingId":"B1"}}"#;
    let _: i64 = con.publish("spx:ws:acct:x", payload).await.unwrap();

    // The bridge must have delivered it to our registered socket.
    let got = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("bridge delivered within 3s")
        .expect("a message");
    let text = match got {
        axum::extract::ws::Message::Text(t) => t.to_string(),
        other => panic!("expected Text, got {other:?}"),
    };
    assert!(text.contains("ticket_accepted") && text.contains("B1"));
}

#[tokio::test]
async fn broadcast_suffix_reaches_every_channel() {
    let hub = Hub::new();
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
    hub.test_register("acct:a", tx_a);
    hub.test_register("acct:b", tx_b);

    let _bridge = spawn_bridge(hub.clone(), &redis_url()).await.expect("bridge");
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let client = redis::Client::open(redis_url()).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let payload = r#"{"type":"stats_update","data":{}}"#;
    let _: i64 = con.publish("spx:ws:__broadcast__", payload).await.unwrap();

    for rx in [&mut rx_a, &mut rx_b] {
        let got = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("bridge delivered within 3s")
            .expect("a message");
        let text = match got {
            axum::extract::ws::Message::Text(t) => t.to_string(),
            other => panic!("expected Text, got {other:?}"),
        };
        assert!(text.contains("stats_update"));
    }
}
