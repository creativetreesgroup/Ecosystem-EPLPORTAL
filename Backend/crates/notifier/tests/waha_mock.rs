//! Fire-and-forget contract tests: a WAHA/n8n failure (5xx OR connection
//! refused) must NOT make `notify_*` error or panic — every `notify_*`
//! returns `()` regardless of delivery outcome, so a caller that
//! `tokio::spawn`s it and drops the handle is never coupled to notification
//! delivery. Also proves a healthy WAHA actually receives the sendText call
//! with the correct body (not just "some request was made").
use notifier::{notify_accepted, notify_agency_loss, notify_new_tickets, BotSettings, NotifyBooking};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings(url: String) -> BotSettings {
    BotSettings {
        enabled: true,
        waha_url: url,
        waha_api_key: "K".into(),
        waha_session: "default".into(),
        wa_group: "12036@g.us".into(),
        portal_label: "EPL".into(),
        ..Default::default()
    }
}

fn booking() -> NotifyBooking {
    NotifyBooking {
        booking_id: "1".into(),
        booking_name: "SPXID1".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn waha_500_does_not_propagate() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    // If notify_accepted returned a Result, a caller doing `?` on it could
    // couple notification failure to the accept flow. It returns () — the
    // compiler enforces there is nothing to propagate. Reaching the end of
    // this test (no panic) IS the assertion.
    notify_accepted(&settings(server.uri()), &booking()).await;
}

#[tokio::test]
async fn waha_connection_refused_does_not_propagate() {
    // No server listening on this port — a hard connection-level error
    // (distinct from an HTTP 5xx), the other class of failure a
    // fire-and-forget sender must swallow.
    let s = settings("http://127.0.0.1:1".to_string());
    notify_accepted(&s, &booking()).await;
    notify_agency_loss(&s, "SPXID1", "rival@x.com", 42, Some("R")).await;
    notify_new_tickets(&s, &[booking()], "https://p/accept").await;
}

#[tokio::test]
async fn healthy_waha_receives_sendtext_with_correct_body() {
    let server = MockServer::start().await;
    let b = NotifyBooking {
        booking_id: "9".into(),
        booking_name: "SPXID9".into(),
        ..Default::default()
    };
    let expected_text = notifier::build_wa_message(&b, "EPL");
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .and(body_json(serde_json::json!({
            "session": "default",
            "chatId": "12036@g.us",
            "text": expected_text,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;
    notify_accepted(&settings(server.uri()), &b).await;
    // wiremock's .expect(1) (with the body_json matcher) verifies on drop
    // that sendText was called exactly once with the exact ported template
    // text — proving both delivery AND correct message content.
}

#[tokio::test]
async fn n8n_failure_does_not_propagate_even_when_waha_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/n8n-hook"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let mut s = settings(server.uri());
    s.webhook_url = format!("{}/n8n-hook", server.uri());
    notify_accepted(&s, &booking()).await;
}

#[tokio::test]
async fn disabled_settings_send_nothing() {
    let server = MockServer::start().await;
    // .expect(0) verifies on drop that sendText was NEVER called.
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let mut s = settings(server.uri());
    s.enabled = false;
    notify_accepted(&s, &booking()).await;
}

#[tokio::test]
async fn new_tickets_flood_guard_skips_over_30_without_calling_waha() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let many: Vec<NotifyBooking> = (0..31).map(|_| booking()).collect();
    notify_new_tickets(&settings(server.uri()), &many, "https://p/accept").await;
}
