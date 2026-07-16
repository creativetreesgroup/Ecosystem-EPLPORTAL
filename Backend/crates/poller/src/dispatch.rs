// Backend/crates/poller/src/dispatch.rs
//! The accept decision pipeline: match → claim (Layer 1+2) → accept HTTP →
//! classify → agency-dup verify → quota consume → durable record → notify. Ties
//! Fase 3 (spx-client) + Fase 4 (executor) + Fase 5 together. Task 10b wires
//! `notifier::notify_accepted`/`notify_agency_loss` in as `tokio::spawn`'d
//! fire-and-forget calls (real outbound HTTP to WAHA/n8n — must never block
//! the hot path) in `finalize_win` and the `LostToAgency` branch below.
//! Task 13's ws `ticket_accepted` publish (below, in `finalize_win`) is a
//! cheap in-process `RedisPublisher::publish` call and is awaited inline
//! rather than spawned, per the Task 13 brief's own wiring snippet.
use std::time::Instant;

use core_domain::matching::find_best_matching_rule_compiled;
use core_domain::BookingType;
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
                    // Task 10b: fire-and-forget "agency-loss" WAHA/n8n alert.
                    if let Some(settings) = shared.notifier.clone() {
                        let spx_id = booking.id.clone();
                        let rival = rival_email.clone();
                        let rule_name = meta.name.clone();
                        let latency_ms_i64 = latency_ms as i64;
                        tokio::spawn(async move {
                            notifier::notify_agency_loss(&settings, &spx_id, &rival, latency_ms_i64, Some(&rule_name)).await;
                        });
                    }
                    if let Some(pub_) = &shared.redis {
                        pub_.record_bot_log(st.tenant_id, &notifier::bot_log::BotLogEntry {
                            ts: chrono::Utc::now().timestamp_millis(),
                            log_type: "error".to_string(),
                            kind: Some("agency_loss".to_string()),
                            booking_id: Some(booking.id.clone()),
                            latency_ms: Some(latency_ms as i64),
                            rule: Some(meta.name.clone()),
                            error: Some(format!("lost to {rival_email}")),
                        })
                        .await;
                    }
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
            // Review correction: release the Layer-2 claim (and, for a capped
            // rule, the inflight quota slot) here too, same as Transient. A
            // 401/403 means SPX rejected the request before it authenticated —
            // the accept almost certainly never fired server-side, so there is
            // no double-accept to protect against. Holding the claim/quota
            // slot for its full 600s TTL would block this ticket from being
            // retried even after Task 7's auto-login recovers in seconds, and
            // for a capped rule would spuriously inflate the inflight count
            // against OTHER, unrelated tickets matching the same rule
            // (`try_claim_auto`'s `accepted_count + SCARD(inflight) >= cap`
            // check), causing spurious `QuotaFull` unrelated to any real
            // accept. Leave the booking row 'pending' (no DB write) — Task 7's
            // relogin is what determines whether this booking gets retried.
            shared
                .executor
                .release_claim_auto(&st.account_id, &booking.id, Some(meta.uuid))
                .await;
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
    // Task 10b: fire-and-forget "accepted" WAHA/n8n notification. Field
    // mapping verified against SpxBooking's ACTUAL struct (spx-client's
    // booking.rs), not guessed — see notify_booking_from_spx_booking's doc
    // comment for the field-by-field rationale (several NotifyBooking fields
    // have no genuine SpxBooking counterpart and are left at their
    // `Default` rather than invented).
    if let Some(settings) = shared.notifier.clone() {
        let nb = notify_booking_from_spx_booking(booking);
        tokio::spawn(async move { notifier::notify_accepted(&settings, &nb).await; });
    }
    if let Some(pub_) = &shared.redis {
        pub_.publish_ticket_accepted(
            &st.account_id,
            serde_json::json!({
                "bookingId": booking.booking_id,
                "latencyMs": latency_ms,
                "autoAccept": true,
                "rule": meta.name,
            }),
        )
        .await;
        pub_.record_bot_log(st.tenant_id, &notifier::bot_log::BotLogEntry {
            ts: chrono::Utc::now().timestamp_millis(),
            log_type: "success".to_string(),
            kind: Some("accept".to_string()),
            booking_id: Some(booking.id.clone()),
            latency_ms: Some(latency_ms as i64),
            rule: Some(meta.name.clone()),
            error: None,
        })
        .await;
    }
}

/// Build `notifier::NotifyBooking` from a real `SpxBooking` (Task 10b). Field
/// mapping was verified against SpxBooking's ACTUAL struct definition
/// (`spx-client/src/booking.rs`) and NotifyBooking's ACTUAL struct definition
/// (`notifier/src/lib.rs`) — not assumed from the plan brief's guessed 1:1
/// list, which named several fields (`cost_type`, `adhoc_tag`,
/// `standby_time`, `period_start`, `period_end`, `bidding_ddl`) that do not
/// actually exist on `SpxBooking` under any name or clear equivalent meaning:
///
/// - `booking_id`/`request_id`/`spx_tx_id`/`vehicle_type`/`route_stops`/
///   `report_station`: identical field names AND meaning on both structs —
///   direct copy.
/// - `onsite_id`: same meaning, but `SpxBooking.onsite_id` is `Option<String>`
///   while `NotifyBooking.onsite_id` is a bare `String` — `unwrap_or_default`.
/// - `is_coc`: NOT sourced from `SpxBooking.cod` (that field is COD/"cash on
///   delivery", a distinct concept — proven independent of COC classification
///   by core_domain's own
///   `coc_only_treats_spxid_as_coc_even_when_cod_flag_is_false` /
///   `coc_only_rejects_reguler_even_when_cod_flag_is_true` tests in
///   `matching.rs`). This codebase's actual definition of "COC" is
///   `booking_type == BookingType::Spxid` (same signal `coc_only` accept
///   rules match on, and the same SPXID-prefix detection
///   `notifier::message::build_ticket_block` itself falls back to internally).
/// - `booking_name`, `cost_type`, `adhoc_tag`, `standby_time`, `period_start`,
///   `period_end`, `bidding_ddl`: `SpxBooking` has no field with this name or
///   an unambiguous same-meaning/same-unit equivalent (`deadline_at` is the
///   closest candidate for `bidding_ddl`, but it is epoch-MILLISECONDS while
///   sibling `period_start`/`period_end` are consumed as epoch-SECONDS by
///   `message::fmt_dmy` — guessing a scale would risk silently planting a
///   1000x-wrong timestamp for whenever these currently-unused-by-any-template
///   fields do get wired up). Left at `NotifyBooking::default()`'s value per
///   the brief's own instruction to default rather than invent.
fn notify_booking_from_spx_booking(b: &SpxBooking) -> notifier::NotifyBooking {
    notifier::NotifyBooking {
        booking_id: b.booking_id.clone(),
        request_id: b.request_id.clone(),
        onsite_id: b.onsite_id.clone().unwrap_or_default(),
        spx_tx_id: b.spx_tx_id.clone(),
        vehicle_type: b.vehicle_type.clone(),
        route_stops: b.route_stops.clone(),
        report_station: b.report_station.clone(),
        is_coc: b.booking_type == BookingType::Spxid,
        ..Default::default()
    }
}

/// Fetch + cache the account's own email (once) for agency-dup classification.
/// A fetch failure collapses to `""` (see `fetch_self_email`'s `Option` ->
/// `unwrap_or_default`); an empty result is never cached as settled — caching
/// it would permanently short-circuit agency-dup detection to `Inconclusive`
/// (-> treated as a win) for the rest of this account's poller task lifetime
/// after a single transient SPX API blip. Only a genuine non-empty email is
/// cached; otherwise every call retries the fetch until one succeeds.
async fn ensure_self_email(shared: &PollerShared, st: &mut PollerState) -> String {
    if let Some(e) = &st.self_email {
        if !e.is_empty() {
            return e.clone();
        }
    }
    let email = executor::fetch_self_email(&shared.client, &st.cookies)
        .await
        .unwrap_or_default();
    if !email.is_empty() {
        st.self_email = Some(email.clone());
    }
    email
}
