// Backend/crates/api-gateway/tests/otp_module.rs
//! Real-Redis (127.0.0.1:16379) tests for `api_gateway::otp` (Task 4) — pure
//! Redis state-management logic, no HTTP/router/`AppState` involved (that's
//! Task 5's job). Same real-Redis integration-test convention this crate
//! and `executor` already use for Redis-backed logic (see
//! `crates/executor/tests/gate_redis.rs`, `crates/api-gateway/tests/
//! auth_routes.rs`'s `test_redis_manager` helper) rather than an inline
//! `#[cfg(test)] mod tests` — unlike `auth::permission::Permission`'s inline
//! tests (pure logic, no I/O), every case here needs a live Redis
//! connection, which this codebase's precedent keeps out of `src/` unit
//! tests and in `tests/*.rs` integration tests instead.
//!
//! A fresh `ConnectionManager` and unique `Uuid::new_v4()` tenant_id/user_id
//! pair per test avoids any cross-test key collision (the OTP module's Redis
//! keys are namespaced by both IDs), so tests can run concurrently — this
//! file does not need `--test-threads=1` on its own, though the workspace
//! test command runs the whole crate that way for other files' sake.
use api_gateway::otp::{self, OtpRequestError, OtpVerifyError};
use uuid::Uuid;

/// Mirrors `otp::MAX_ATTEMPTS` (private to the module) — kept as a separate
/// test-local constant rather than exposing the module's internal constant,
/// since asserting against a hardcoded `5` here is itself part of what this
/// test verifies (a silent change to the module's real constant should fail
/// this test, not silently track it).
const MAX_ATTEMPTS: u32 = 5;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

async fn conn() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis connection manager")
}

async fn ttl(redis: &mut redis::aio::ConnectionManager, key: &str) -> i64 {
    redis::cmd("TTL")
        .arg(key)
        .query_async(redis)
        .await
        .expect("TTL query")
}

#[tokio::test]
async fn request_succeeds_and_returns_six_digit_code() {
    let mut redis = conn().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let code = otp::request(&mut redis, tenant_id, user_id)
        .await
        .expect("first request should succeed");

    assert_eq!(code.len(), 6, "OTP code must be exactly 6 characters: {code}");
    assert!(
        code.chars().all(|c| c.is_ascii_digit()),
        "OTP code must be all-numeric: {code}"
    );
}

#[tokio::test]
async fn immediate_resend_is_rejected_as_too_soon() {
    let mut redis = conn().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    otp::request(&mut redis, tenant_id, user_id)
        .await
        .expect("first request should succeed");

    let second = otp::request(&mut redis, tenant_id, user_id).await;
    match second {
        Err(OtpRequestError::TooSoon) => {}
        other => panic!("expected TooSoon on immediate resend, got {other:?}"),
    }
}

#[tokio::test]
async fn verify_correct_code_succeeds_and_deletes_the_code() {
    let mut redis = conn().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let code = otp::request(&mut redis, tenant_id, user_id)
        .await
        .expect("request should succeed");

    otp::verify(&mut redis, tenant_id, user_id, &code)
        .await
        .expect("verify with the correct code should succeed");

    // Single-use: the same (now-deleted) code fails as NoActiveCode, not
    // WrongCode — the stored code is gone, not merely mismatched.
    let second = otp::verify(&mut redis, tenant_id, user_id, &code).await;
    match second {
        Err(OtpVerifyError::NoActiveCode) => {}
        other => panic!("expected NoActiveCode after single-use consumption, got {other:?}"),
    }
}

#[tokio::test]
async fn five_wrong_attempts_then_the_sixth_is_too_many_attempts() {
    let mut redis = conn().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let code = otp::request(&mut redis, tenant_id, user_id)
        .await
        .expect("request should succeed");
    // A wrong code guaranteed not to equal the real one (6 nines is never a
    // value `generate_code`'s `% 1_000_000` reduction can collide with...
    // except when it IS "999999"; guard against that one-in-a-million case
    // explicitly rather than leaving a flaky test).
    let wrong = if code == "999999" { "000000" } else { "999999" };

    for attempt in 1..=MAX_ATTEMPTS {
        let result = otp::verify(&mut redis, tenant_id, user_id, wrong).await;
        match result {
            Err(OtpVerifyError::WrongCode) => {}
            other => panic!("attempt {attempt}: expected WrongCode, got {other:?}"),
        }
    }

    // The 6th attempt (MAX_ATTEMPTS + 1st) is rejected on attempt-count
    // alone, regardless of which code is submitted.
    let sixth = otp::verify(&mut redis, tenant_id, user_id, wrong).await;
    match sixth {
        Err(OtpVerifyError::TooManyAttempts) => {}
        other => panic!("expected TooManyAttempts on the 6th attempt, got {other:?}"),
    }
}

#[tokio::test]
async fn successful_verify_writes_pwverify_key_with_expected_ttl() {
    let mut redis = conn().await;
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    let code = otp::request(&mut redis, tenant_id, user_id)
        .await
        .expect("request should succeed");
    otp::verify(&mut redis, tenant_id, user_id, &code)
        .await
        .expect("verify should succeed");

    let pwverify_key = format!("spx:pwverify:{tenant_id}:{user_id}");
    let ttl_secs = ttl(&mut redis, &pwverify_key).await;
    assert!(
        ttl_secs > 0 && ttl_secs <= 120,
        "pwverify TTL should be in (0, 120], got {ttl_secs}"
    );
}
