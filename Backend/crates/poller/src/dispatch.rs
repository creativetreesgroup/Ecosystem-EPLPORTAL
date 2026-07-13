// Backend/crates/poller/src/dispatch.rs
//! The accept decision pipeline: match → claim (Layer 1+2) → accept HTTP →
//! classify → agency-dup verify → quota consume → durable record → notify. Ties
//! Fase 3 (spx-client) + Fase 4 (executor) + Fase 5 together. Notifier/ws
//! publish are spawned fire-and-forget (Tasks 10/13 fill the hooks).
use std::time::Instant;

use core_domain::matching::find_best_matching_rule_compiled;
use executor::{AgencyDupOutcome, ClaimOutcome};
use spx_client::{to_core_booking, AcceptReason, SpxBooking};
use uuid::Uuid;

use crate::state::{PollerShared, PollerState};

/// DB rule identity aligned by index with `PollerState.rules` (CompiledRule has a
/// String id; the executor/store quota APIs need a Uuid + cap/accepted).
#[derive(Debug, Clone)]
pub struct RuleMeta {
    pub uuid: Uuid,
    pub cap: i64,
    pub accepted_count: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    Accepted,
    Duplicate,
    QuotaFull,
    Taken,
    LostToAgency { rival: String },
    Transient,
    Auth,
    Skipped,
}

/// A sub-classification stamped into `raw_data->>'accept_reason'` for a
/// terminal-but-not-won outcome — `rule_matched` (a UUID FK) cannot hold a
/// free-text reason, so this is threaded through `store::update_booking_status`
/// as a SEPARATE parameter (never bound to `rule_matched`). See Task 5's
/// `drift_reason` precedent (`store::bookings`'s module doc) for why.
const REASON_TAKEN_BY_OTHER: &str = "taken_by_other";

pub async fn dispatch_booking(
    shared: &PollerShared,
    st: &mut PollerState,
    booking: &SpxBooking,
) -> DispatchResult {
    // 1. Match against compiled rules (first-wins index).
    let core = to_core_booking(booking);
    let idx = match find_best_matching_rule_compiled(&st.rules, &core, &st.match_state) {
        Some(i) => i,
        None => return DispatchResult::Skipped,
    };
    let meta = st.rule_meta[idx].clone();

    // 2. Layer 1 in-proc claim.
    if !st.dedup.try_begin_accept(&booking.id) {
        return DispatchResult::Skipped;
    }

    // 3. Layer 2 durable atomic gate (fail-closed).
    match shared
        .executor
        .try_claim_auto(
            &st.account_id,
            &booking.id,
            Some(meta.uuid),
            meta.cap,
            meta.accepted_count,
        )
        .await
    {
        ClaimOutcome::Proceed => {}
        ClaimOutcome::AlreadyClaimed => {
            st.dedup.abort_accept(&booking.id);
            return DispatchResult::Duplicate;
        }
        ClaimOutcome::QuotaFull => {
            st.dedup.abort_accept(&booking.id);
            return DispatchResult::QuotaFull;
        }
        ClaimOutcome::RedisUnavailable => {
            st.dedup.abort_accept(&booking.id); // fail-closed: do NOT dispatch
            return DispatchResult::Skipped;
        }
    }

    // 4. The actual accept HTTP call. `accept_latency_ms` is Postgres `INT`
    // (Rust i32), not i64 — a booking's own accept latency in ms will never
    // remotely approach i32's ~2.1 billion range, so measuring directly as
    // i32 (rather than widening through i64 first) is both safe and keeps the
    // Rust type in lockstep with the DB column at every call site below.
    let booking_id_i64 = booking.booking_id.parse::<i64>().unwrap_or(0);
    let request_ids: Vec<i64> = booking.request_id.parse::<i64>().ok().into_iter().collect();
    let started = Instant::now();
    let result = shared
        .client
        .accept_booking(&st.cookies, booking_id_i64, st.agency_id, &request_ids)
        .await;
    let latency_ms: i32 = started.elapsed().as_millis().try_into().unwrap_or(i32::MAX);

    match result.reason {
        AcceptReason::Ok => {
            finalize_win(shared, st, booking, &meta, latency_ms).await;
            DispatchResult::Accepted
        }
        AcceptReason::AgencyDup => {
            let self_email = ensure_self_email(shared, st).await;
            match executor::verify_agency_dup(
                &shared.client,
                &st.cookies,
                &self_email,
                booking_id_i64,
            )
            .await
            {
                AgencyDupOutcome::Ours | AgencyDupOutcome::Inconclusive => {
                    finalize_win(shared, st, booking, &meta, latency_ms).await;
                    DispatchResult::Accepted
                }
                AgencyDupOutcome::LostToAgency { rival_email } => {
                    st.dedup.abort_accept(&booking.id);
                    let _ = store::update_booking_status(
                        &shared.pool,
                        st.tenant_id,
                        &booking.id,
                        store::BookingStatusUpdate {
                            status: "failed",
                            latency_ms: Some(latency_ms),
                            auto_accepted: false,
                            rule_matched: None,
                            accept_reason: Some(REASON_TAKEN_BY_OTHER),
                        },
                    )
                    .await;
                    // Task 10 fills the notifier agency-loss spawn here.
                    DispatchResult::LostToAgency { rival: rival_email }
                }
            }
        }
        AcceptReason::Taken => {
            st.dedup.abort_accept(&booking.id);
            // Terminal — keep the durable Layer-2 claim (do NOT release): a
            // ticket someone else won must never be retried.
            let _ = store::update_booking_status(
                &shared.pool,
                st.tenant_id,
                &booking.id,
                store::BookingStatusUpdate {
                    status: "failed",
                    latency_ms: Some(latency_ms),
                    auto_accepted: false,
                    rule_matched: None,
                    accept_reason: Some(REASON_TAKEN_BY_OTHER),
                },
            )
            .await;
            DispatchResult::Taken
        }
        AcceptReason::Transient => {
            st.dedup.abort_accept(&booking.id);
            // Release the Layer-2 claim so next cycle retries instead of
            // waiting out the 600s TTL; leave the booking row 'pending'.
            shared
                .executor
                .release_claim_auto(&st.account_id, &booking.id, Some(meta.uuid))
                .await;
            DispatchResult::Transient
        }
        AcceptReason::Auth => {
            st.dedup.abort_accept(&booking.id);
            // Correction #5: jump straight to the relogin threshold rather
            // than incrementing by one — an accept-time 401/403 is already a
            // confirmed auth failure, not a soft signal to accumulate.
            st.consecutive_401s = st.consecutive_401s.max(3);
            // Leave the booking row 'pending' and the Layer-2 claim intact
            // (port exactly: the design note does NOT release here, unlike
            // Transient — the claim's 600s TTL is a reasonable backstop while
            // relogin is in flight).
            DispatchResult::Auth
        }
        AcceptReason::Error => {
            st.dedup.abort_accept(&booking.id);
            shared
                .executor
                .release_claim_auto(&st.account_id, &booking.id, Some(meta.uuid))
                .await;
            DispatchResult::Skipped
        }
    }
}

/// Commit a confirmed win: Layer-1 commit + durable ZADD + quota consume + DB
/// status + (Tasks 10/13) notify/publish.
async fn finalize_win(
    shared: &PollerShared,
    st: &mut PollerState,
    booking: &SpxBooking,
    meta: &RuleMeta,
    latency_ms: i32,
) {
    st.dedup.commit_accept(&booking.id);
    let _ = shared
        .executor
        .record_durable_accept(&st.account_id, &booking.id)
        .await;
    let _ = shared
        .executor
        .apply_rule_consumption(
            &shared.pool,
            st.tenant_id,
            &st.account_id,
            meta.uuid,
            &booking.id,
        )
        .await;
    let _ = store::update_booking_status(
        &shared.pool,
        st.tenant_id,
        &booking.id,
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: Some(latency_ms),
            auto_accepted: true,
            rule_matched: Some(meta.uuid),
            accept_reason: None,
        },
    )
    .await;
    // Task 10: tokio::spawn(notifier::notify_accepted(...)); ignore Result.
    // Task 13: publish ws `ticket_accepted` to `acct:<account_id>`.
}

/// Fetch + cache the account's own email (once) for agency-dup classification.
async fn ensure_self_email(shared: &PollerShared, st: &mut PollerState) -> String {
    if let Some(e) = &st.self_email {
        return e.clone();
    }
    let email = executor::fetch_self_email(&shared.client, &st.cookies)
        .await
        .unwrap_or_default();
    st.self_email = Some(email.clone());
    email
}
