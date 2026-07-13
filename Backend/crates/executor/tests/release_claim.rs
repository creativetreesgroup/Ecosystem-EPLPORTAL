// Backend/crates/executor/tests/release_claim.rs
//! release_claim_auto lets the SAME spxId be claimed again (a transient retry).
use executor::{ClaimOutcome, ExecutorHandle};
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn release_allows_reclaim() {
    let h = ExecutorHandle::connect(&redis_url()).await.unwrap();
    let a = format!("t{}", Uuid::new_v4().simple());
    assert_eq!(
        h.try_claim_auto(&a, "77", None, 0, 0).await,
        ClaimOutcome::Proceed
    );
    assert_eq!(
        h.try_claim_auto(&a, "77", None, 0, 0).await,
        ClaimOutcome::AlreadyClaimed
    );
    h.release_claim_auto(&a, "77", None).await;
    assert_eq!(
        h.try_claim_auto(&a, "77", None, 0, 0).await,
        ClaimOutcome::Proceed,
        "after release the ticket must be reclaimable"
    );
}

/// Same proof, but with a capped rule so the release also SREMs the inflight
/// set (`release_claim_auto`'s `rule_id.is_some()` branch), not just the plain
/// claim key.
#[tokio::test]
async fn release_with_rule_id_allows_reclaim_under_cap() {
    let h = ExecutorHandle::connect(&redis_url()).await.unwrap();
    let a = format!("t{}", Uuid::new_v4().simple());
    let rule = Uuid::new_v4();
    // cap=1, accepted_count=0: first claim succeeds and consumes the inflight
    // slot; a second spxId for the SAME rule would hit QuotaFull until the
    // first is released.
    assert_eq!(
        h.try_claim_auto(&a, "88", Some(rule), 1, 0).await,
        ClaimOutcome::Proceed
    );
    assert_eq!(
        h.try_claim_auto(&a, "89", Some(rule), 1, 0).await,
        ClaimOutcome::QuotaFull,
        "cap=1 with one in-flight claim must reject a second spxId"
    );
    h.release_claim_auto(&a, "88", Some(rule)).await;
    assert_eq!(
        h.try_claim_auto(&a, "88", Some(rule), 1, 0).await,
        ClaimOutcome::Proceed,
        "after release, the original spxId must be reclaimable"
    );
}
