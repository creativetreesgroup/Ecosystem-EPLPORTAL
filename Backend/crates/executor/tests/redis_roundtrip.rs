//! Basic real-Redis connectivity: open a pool against the tower-redis container
//! (127.0.0.1:16379), PING, and SET/GET round-trip through a unique key. Proves
//! the RedisPool lazy-connect + ConnectionManager path works end to end.
use executor::RedisPool;
use redis::AsyncCommands;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn pool_connects_and_round_trips() {
    let pool = RedisPool::open(&redis_url()).expect("open");
    let mut con = pool.conn().await.expect("conn");

    let pong: String = redis::cmd("PING")
        .query_async(&mut con)
        .await
        .expect("ping");
    assert_eq!(pong, "PONG");

    let key = format!("executor:test:{}", Uuid::new_v4());
    let _: () = con.set(&key, "hello").await.expect("set");
    let got: String = con.get(&key).await.expect("get");
    assert_eq!(got, "hello");
    let _: () = con.del(&key).await.expect("del");
}
