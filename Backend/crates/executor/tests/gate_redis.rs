//! Real-Redis (127.0.0.1:16379) tests for the claim gate: the three Lua return
//! values, auto=fail-closed vs manual=fail-open under an unreachable Redis, and
//! that manual + auto share the claim keyspace. Unique account ids per test so
//! no FLUSHALL / serialization is needed for key isolation.
use executor::{AccountDedupState, ClaimOutcome, ExecutorHandle, ManualClaimOutcome};
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn acct() -> String {
    format!("t{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn gate_returns_proceed_already_and_quota_full() {
    let h = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect");
    let a = acct();
    let rule = Uuid::new_v4();

    // New claim → Proceed (uncapped).
    assert_eq!(
        h.try_claim_auto(&a, "100", None, 0, 0).await,
        ClaimOutcome::Proceed
    );
    // Same spxId again → AlreadyClaimed (SET NX fails).
    assert_eq!(
        h.try_claim_auto(&a, "100", None, 0, 0).await,
        ClaimOutcome::AlreadyClaimed
    );
    // Capped rule (cap=1, accepted=0): first NEW spxId claims and enters the
    // inflight set → Proceed.
    assert_eq!(
        h.try_claim_auto(&a, "200", Some(rule), 1, 0).await,
        ClaimOutcome::Proceed
    );
    // A second NEW spxId under the same full rule → QuotaFull.
    assert_eq!(
        h.try_claim_auto(&a, "201", Some(rule), 1, 0).await,
        ClaimOutcome::QuotaFull
    );
}

#[tokio::test]
async fn auto_fails_closed_manual_fails_open_when_redis_unreachable() {
    // Nothing listens on 16999 — the pool opens offline; commands error fast.
    let h = ExecutorHandle::connect("redis://127.0.0.1:16999")
        .await
        .expect("open offline");
    let a = acct();
    let dedup = AccountDedupState::new();

    // Auto → RedisUnavailable (fail-closed: must NOT dispatch).
    let auto = h.try_claim_auto(&a, "1", None, 0, 0).await;
    assert_eq!(auto, ClaimOutcome::RedisUnavailable);
    assert!(!auto.should_dispatch());

    // Manual → Ok (fail-open: proceed).
    assert_eq!(
        h.try_claim_manual(&a, "1", &dedup).await,
        ManualClaimOutcome::Ok
    );
}

#[tokio::test]
async fn manual_and_auto_share_the_claim_key() {
    let h = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect");
    let a = acct();
    let dedup = AccountDedupState::new();

    // Manual claims spxId X first.
    assert_eq!(
        h.try_claim_manual(&a, "555", &dedup).await,
        ManualClaimOutcome::Ok
    );
    // Auto for the SAME account+spxId must now fail — proving the keyspace is
    // genuinely shared (DoD #8).
    assert_eq!(
        h.try_claim_auto(&a, "555", None, 0, 0).await,
        ClaimOutcome::AlreadyClaimed
    );
}

#[tokio::test]
async fn manual_rejects_when_layer1_already_known() {
    let h = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect");
    let a = acct();
    let dedup = AccountDedupState::new();
    dedup.insert_restored("999"); // pretend it was already accepted
    assert_eq!(
        h.try_claim_manual(&a, "999", &dedup).await,
        ManualClaimOutcome::AlreadyAccepted
    );
}
