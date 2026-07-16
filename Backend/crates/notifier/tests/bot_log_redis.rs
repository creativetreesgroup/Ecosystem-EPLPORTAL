// Backend/crates/notifier/tests/bot_log_redis.rs
//! `notifier::bot_log` — record/list/clear round trip + the 200-entry cap, against real Redis.
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

async fn connection() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis")
}

#[tokio::test]
async fn record_list_clear_round_trip_newest_first() {
    let mut redis = connection().await;
    let tenant_id = uuid::Uuid::new_v4();
    notifier::bot_log::clear(&mut redis, tenant_id).await; // start from a known-empty state

    for kind in ["accept", "otp", "agency_loss"] {
        notifier::bot_log::record(
            &mut redis,
            tenant_id,
            &notifier::bot_log::BotLogEntry {
                ts: 1000,
                log_type: "success".to_string(),
                kind: Some(kind.to_string()),
                booking_id: None,
                latency_ms: None,
                rule: None,
                error: None,
            },
        )
        .await;
    }

    let listed = notifier::bot_log::list(&mut redis, tenant_id, 10).await;
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].kind.as_deref(), Some("agency_loss"), "LPUSH means newest (last-recorded) is first");
    assert_eq!(listed[2].kind.as_deref(), Some("accept"));

    notifier::bot_log::clear(&mut redis, tenant_id).await;
    let after_clear = notifier::bot_log::list(&mut redis, tenant_id, 10).await;
    assert_eq!(after_clear.len(), 0);
}

#[tokio::test]
async fn caps_at_200_entries() {
    let mut redis = connection().await;
    let tenant_id = uuid::Uuid::new_v4();
    notifier::bot_log::clear(&mut redis, tenant_id).await;

    for i in 0..210 {
        notifier::bot_log::record(
            &mut redis,
            tenant_id,
            &notifier::bot_log::BotLogEntry {
                ts: i,
                log_type: "success".to_string(),
                kind: None,
                booking_id: None,
                latency_ms: None,
                rule: None,
                error: None,
            },
        )
        .await;
    }

    let listed = notifier::bot_log::list(&mut redis, tenant_id, 250).await;
    assert_eq!(listed.len(), 200, "LTRIM must cap the list at 200 regardless of how many were pushed");
    assert_eq!(listed[0].ts, 209, "the newest 200 must survive, not the oldest");

    notifier::bot_log::clear(&mut redis, tenant_id).await;
}

#[tokio::test]
async fn different_tenants_do_not_see_each_others_logs() {
    let mut redis = connection().await;
    let tenant_a = uuid::Uuid::new_v4();
    let tenant_b = uuid::Uuid::new_v4();
    notifier::bot_log::clear(&mut redis, tenant_a).await;
    notifier::bot_log::clear(&mut redis, tenant_b).await;

    notifier::bot_log::record(&mut redis, tenant_a, &notifier::bot_log::BotLogEntry {
        ts: 1, log_type: "success".to_string(), kind: Some("accept".to_string()),
        booking_id: Some("tenant-a-booking".to_string()), latency_ms: None, rule: None, error: None,
    }).await;
    notifier::bot_log::record(&mut redis, tenant_b, &notifier::bot_log::BotLogEntry {
        ts: 2, log_type: "success".to_string(), kind: Some("accept".to_string()),
        booking_id: Some("tenant-b-booking".to_string()), latency_ms: None, rule: None, error: None,
    }).await;

    let logs_a = notifier::bot_log::list(&mut redis, tenant_a, 10).await;
    let logs_b = notifier::bot_log::list(&mut redis, tenant_b, 10).await;
    assert_eq!(logs_a.len(), 1);
    assert_eq!(logs_a[0].booking_id.as_deref(), Some("tenant-a-booking"));
    assert_eq!(logs_b.len(), 1);
    assert_eq!(logs_b[0].booking_id.as_deref(), Some("tenant-b-booking"));

    notifier::bot_log::clear(&mut redis, tenant_a).await;
    notifier::bot_log::clear(&mut redis, tenant_b).await;
}
