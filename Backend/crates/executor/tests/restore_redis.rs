//! DoD #4: seed a durable ZSET with in-window and out-of-window entries,
//! restore, and assert only the in-window entries land in Layer 1 (the stale
//! ones are trimmed, not restored) — checked both in-proc (`AccountDedupState`)
//! AND directly against Redis itself (ZCARD/ZSCORE on `spx:accepted:<acct>`)
//! so the assertion can't pass "by coincidence" if the trim silently no-oped.
//! Real Redis @ 127.0.0.1:16379, unique account id per test (no FLUSHALL
//! needed for isolation).
use executor::{AccountDedupState, ExecutorHandle};
use redis::AsyncCommands;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

const WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

async fn direct_conn() -> redis::aio::MultiplexedConnection {
    let client = redis::Client::open(redis_url()).expect("open direct client");
    client
        .get_multiplexed_async_connection()
        .await
        .expect("direct connect")
}

#[tokio::test]
async fn restore_keeps_in_window_and_trims_stale() {
    let h = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect");
    let account = format!("t{}", Uuid::new_v4().simple());
    let key = format!("spx:accepted:{account}");

    let now = now_epoch_secs();
    let eight_days = 8 * 24 * 60 * 60;

    // Four members spanning the 7-day cutoff with a wide margin so the test
    // is not sensitive to the few seconds of drift between "now" as computed
    // here and "now" as computed inside `restore_accepted_ids`:
    //   - well inside the window (today)
    //   - just inside the window (cutoff + 1 hour)
    //   - just outside the window (cutoff - 1 hour)
    //   - well outside the window (8 days old)
    let cutoff = now - WINDOW_SECS;
    h.record_durable_accept_at(&account, "recent-spx", now)
        .await
        .expect("record recent");
    h.record_durable_accept_at(&account, "boundary-in-spx", cutoff + 3600)
        .await
        .expect("record boundary-in");
    h.record_durable_accept_at(&account, "boundary-out-spx", cutoff - 3600)
        .await
        .expect("record boundary-out");
    h.record_durable_accept_at(&account, "stale-spx", now - eight_days)
        .await
        .expect("record stale");

    // Sanity: all four are actually in Redis before restore runs.
    let mut con = direct_conn().await;
    let pre_card: i64 = con.zcard(&key).await.expect("pre zcard");
    assert_eq!(pre_card, 4, "all four members must be seeded before trim");

    let state = AccountDedupState::new();
    let restored = h
        .restore_accepted_ids(&account, &state)
        .await
        .expect("restore");

    // In-proc (Layer 1) assertions.
    assert_eq!(
        restored, 2,
        "only the two in-window entries may be restored"
    );
    assert!(state.is_known("recent-spx"));
    assert!(state.is_known("boundary-in-spx"));
    assert!(
        !state.is_known("boundary-out-spx"),
        "an entry just past the 7-day cutoff must be trimmed, not restored"
    );
    assert!(
        !state.is_known("stale-spx"),
        "an entry 8 days old must be trimmed, not restored"
    );
    assert_eq!(state.accepted_len(), 2);

    // Redis-side assertions: prove the trim actually happened in Redis
    // itself, not merely that the in-proc state looks right by coincidence.
    let post_card: i64 = con.zcard(&key).await.expect("post zcard");
    assert_eq!(
        post_card, 2,
        "ZREMRANGEBYSCORE must have physically removed the stale members from Redis"
    );
    let stale_score: Option<f64> = con.zscore(&key, "stale-spx").await.expect("zscore stale");
    assert!(
        stale_score.is_none(),
        "stale-spx must no longer exist in the Redis ZSET after trim"
    );
    let boundary_out_score: Option<f64> = con
        .zscore(&key, "boundary-out-spx")
        .await
        .expect("zscore boundary-out");
    assert!(
        boundary_out_score.is_none(),
        "boundary-out-spx must no longer exist in the Redis ZSET after trim"
    );
    let recent_score: Option<f64> = con.zscore(&key, "recent-spx").await.expect("zscore recent");
    assert!(
        recent_score.is_some(),
        "recent-spx must still exist in the Redis ZSET after trim"
    );
}

#[tokio::test]
async fn restore_is_idempotent_and_survives_repeat_calls() {
    // A second restore call (e.g. a process crash-restart-restart) must not
    // double-count or error; ZADD on the same member/score is a no-op update,
    // and DashMap insert on an existing key is also idempotent.
    let h = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect");
    let account = format!("t{}", Uuid::new_v4().simple());
    let now = now_epoch_secs();

    h.record_durable_accept_at(&account, "only-spx", now)
        .await
        .expect("record");

    let state = AccountDedupState::new();
    let first = h
        .restore_accepted_ids(&account, &state)
        .await
        .expect("first restore");
    let second = h
        .restore_accepted_ids(&account, &state)
        .await
        .expect("second restore");

    assert_eq!(first, 1);
    assert_eq!(second, 1);
    assert_eq!(state.accepted_len(), 1);
    assert!(state.is_known("only-spx"));
}
