//! DoD #8 (push half): a valid subscription + VAPID key yields a POST to the
//! subscription endpoint with aes128gcm content-encoding + a vapid Authorization
//! header. Uses a generated test VAPID keypair + a p256 subscription key so the
//! crypto actually runs; wiremock is the push endpoint.
//!
//! `send_push_to_account`'s Redis-backed integration tests follow this
//! project's Fase-2 convention: real Redis (`redis://127.0.0.1:16379`,
//! already running via `docker compose` in this environment) for the
//! subscription store, wiremock only for the push-service HTTP endpoint.
use notifier::{
    build_push_request, send_push_to_account, PushError, PushPayload, PushSubscription,
    VapidConfig,
};
use redis::AsyncCommands;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REDIS_URL: &str = "redis://127.0.0.1:16379";

// Real base64url test vectors, generated once via
// `p256::SecretKey::random(&mut OsRng)` (VAPID app-server keypair) and a
// second independent p256 keypair standing in for a browser's
// `pushManager.subscribe()` `p256dh` key, plus 16 random bytes for `auth`.
// The assertions below are about REQUEST SHAPE (method, host,
// content-encoding, vapid Authorization prefix) — no live push is delivered
// against these; a live-endpoint send was verified manually against a real
// browser subscription (see commit message).
const VAPID_PRIVATE: &str = "QxKPh3e_W8Aq51rZ5OCdIpR6QzvL-xNZhYFDO0yIl4Q";
const VAPID_PUBLIC: &str =
    "BItm9hqevlykkyVBEq_WFtwln5IskrcDIcVRhUJP2frWF5fJ20FkBATDs192ninO08cqrH5Y75sXbTYoylXv-5c";
const SUB_P256DH: &str =
    "BK2DV0BVTlWL7k1mCXvnwbwINCaehHj8UzcYhi63DCkPRCctpmiXCMt26lcQTOkg4IPCoM3L0S2q4R9ZS8f9PsA";
const SUB_AUTH: &str = "7ZMZGPIeqwZYNPQHrMM9Ug";

fn vapid() -> VapidConfig {
    VapidConfig {
        subject: "mailto:ops@example.com".into(),
        public_key: VAPID_PUBLIC.into(),
        private_key: VAPID_PRIVATE.into(),
    }
}

#[test]
fn build_push_request_has_encryption_and_vapid_headers() {
    let sub = PushSubscription {
        endpoint: "https://push.example.com/abc".into(),
        p256dh: SUB_P256DH.into(),
        auth: SUB_AUTH.into(),
    };
    let payload = PushPayload {
        title: "T".into(),
        body: "B".into(),
        url: None,
        tag: None,
    };
    let req = build_push_request(&vapid(), &sub, &payload).expect("build push request");
    assert_eq!(req.method(), http::Method::POST);
    assert_eq!(req.uri().host(), Some("push.example.com"));
    assert_eq!(req.headers().get("content-encoding").unwrap(), "aes128gcm");
    assert!(req
        .headers()
        .get(http::header::AUTHORIZATION)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("vapid "));
}

// Review finding: `auth` decodes fine as base64url but isn't 16 bytes (this
// is `SUB_AUTH` above truncated by one char) — `build_push_request` must
// return `Err(PushError::Sub(_))`, NOT panic, since `Auth::clone_from_slice`
// (a `GenericArray<u8, U16>`) panics on any length other than 16.
const SUB_AUTH_WRONG_LEN: &str = "7ZMZGPIeqwZYNPQHrMM9";

#[test]
fn build_push_request_rejects_malformed_auth_field_instead_of_panicking() {
    let sub = PushSubscription {
        endpoint: "https://push.example.com/abc".into(),
        p256dh: SUB_P256DH.into(),
        auth: SUB_AUTH_WRONG_LEN.into(),
    };
    let payload = PushPayload {
        title: "T".into(),
        body: "B".into(),
        url: None,
        tag: None,
    };
    let err = build_push_request(&vapid(), &sub, &payload)
        .expect_err("malformed auth must be rejected, not panic");
    assert!(matches!(err, PushError::Sub(_)));
}

async fn redis_con() -> redis::aio::MultiplexedConnection {
    redis::Client::open(REDIS_URL)
        .expect("redis client")
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection (docker compose tower-redis on :16379)")
}

async fn seed_subscription(account_id: &str, endpoint: &str) {
    let key = format!("spx:push_subs:{}", account_id.to_lowercase());
    let sub_json =
        serde_json::json!({ "endpoint": endpoint, "keys": { "p256dh": SUB_P256DH, "auth": SUB_AUTH } })
            .to_string();
    let mut con = redis_con().await;
    let _: usize = con.del(&key).await.unwrap_or(0); // idempotent across reruns
    let _: usize = con.sadd(&key, sub_json).await.expect("seed subscription");
}

async fn subscription_count(account_id: &str) -> usize {
    let key = format!("spx:push_subs:{}", account_id.to_lowercase());
    let mut con = redis_con().await;
    let members: Vec<String> = con.smembers(&key).await.unwrap_or_default();
    members.len()
}

fn payload() -> PushPayload {
    PushPayload {
        title: "New ticket".into(),
        body: "SPX1".into(),
        url: None,
        tag: None,
    }
}

#[tokio::test]
async fn send_push_to_account_delivers_and_leaves_live_subscription() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push/ok"))
        .and(header("content-encoding", "aes128gcm"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let account_id = "push-test-ok";
    seed_subscription(account_id, &format!("{}/push/ok", server.uri())).await;

    let sent = send_push_to_account(REDIS_URL, &vapid(), account_id, &payload()).await;
    assert_eq!(sent, 1);
    assert_eq!(subscription_count(account_id).await, 1); // untouched on success
}

#[tokio::test]
async fn send_push_to_account_prunes_410_gone_subscription() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push/gone"))
        .respond_with(ResponseTemplate::new(410))
        .expect(1)
        .mount(&server)
        .await;

    let account_id = "push-test-gone";
    seed_subscription(account_id, &format!("{}/push/gone", server.uri())).await;

    let sent = send_push_to_account(REDIS_URL, &vapid(), account_id, &payload()).await;
    assert_eq!(sent, 0);
    assert_eq!(subscription_count(account_id).await, 0); // pruned from the Redis SET
}

#[tokio::test]
async fn send_push_to_account_with_no_subscriptions_is_a_noop() {
    let account_id = "push-test-empty";
    let key = format!("spx:push_subs:{}", account_id.to_lowercase());
    let mut con = redis_con().await;
    let _: usize = con.del(&key).await.unwrap_or(0);
    drop(con);

    let sent = send_push_to_account(REDIS_URL, &vapid(), account_id, &payload()).await;
    assert_eq!(sent, 0);
}
