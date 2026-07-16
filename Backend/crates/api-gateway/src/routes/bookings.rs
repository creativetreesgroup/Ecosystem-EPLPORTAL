// Backend/crates/api-gateway/src/routes/bookings.rs
//! `GET /bookings/live`, `/bookings/history`, `/bookings/:id/detail`, `/bookings/spx-log` —
//! read-only booking + accept-event-audit views, and (Task 10) `POST /bookings/:id/accept` —
//! manual accept. Every route here needs only `session_auth` (any logged-in tenant member);
//! see this file's own `require_permission` usage (Task 10's handler) for the one exception's
//! rationale.
use axum::extract::{Extension, Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}
fn default_limit() -> i64 {
    50
}
/// Clamp caller-supplied pagination to a sane range — `store`'s own list fns trust their
/// caller (see `bookings.rs`'s doc comment on `list_live`), so this route is that caller.
fn clamp_limit(limit: i64) -> i64 {
    limit.clamp(1, 200)
}
fn clamp_offset(offset: i64) -> i64 {
    offset.max(0)
}

#[derive(Debug, Serialize)]
pub struct BookingListItem {
    pub id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl From<store::models::Booking> for BookingListItem {
    fn from(b: store::models::Booking) -> Self {
        Self {
            id: b.id,
            account_id: b.account_id,
            spx_id: b.spx_id,
            status: b.status,
            service_type: b.service_type,
            weight: b.weight,
            cod_amount: b.cod_amount,
            auto_accepted: b.auto_accepted,
            rule_matched: b.rule_matched,
            created_at: b.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BookingDetail {
    pub id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub raw_data: Value,
    pub is_coc: bool,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub accept_latency_ms: Option<i32>,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<store::models::Booking> for BookingDetail {
    fn from(b: store::models::Booking) -> Self {
        Self {
            id: b.id,
            account_id: b.account_id,
            spx_id: b.spx_id,
            status: b.status,
            raw_data: b.raw_data,
            is_coc: b.is_coc,
            service_type: b.service_type,
            weight: b.weight,
            cod_amount: b.cod_amount,
            auto_accepted: b.auto_accepted,
            accept_latency_ms: b.accept_latency_ms,
            rule_matched: b.rule_matched,
            created_at: b.created_at,
            updated_at: b.updated_at,
        }
    }
}

async fn live(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<BookingListItem>>, ApiError> {
    let rows = store::bookings::list_live(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
    )
    .await?;
    Ok(Json(rows.into_iter().map(BookingListItem::from).collect()))
}

async fn history(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<BookingListItem>>, ApiError> {
    let rows = store::bookings::list_history(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
    )
    .await?;
    Ok(Json(rows.into_iter().map(BookingListItem::from).collect()))
}

async fn detail(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<BookingDetail>, ApiError> {
    let row = store::bookings::get_detail(&state.poller.pool, user.tenant_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(BookingDetail::from(row)))
}

#[derive(Debug, Serialize)]
pub struct AcceptEventItem {
    pub id: Uuid,
    pub booking_id: Option<Uuid>,
    pub rule_id: Option<Uuid>,
    pub outcome: String,
    pub local_dispatch_us: Option<i64>,
    pub accept_e2e_ms: Option<i64>,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

impl From<store::models::AcceptEvent> for AcceptEventItem {
    fn from(e: store::models::AcceptEvent) -> Self {
        Self {
            id: e.id,
            booking_id: e.booking_id,
            rule_id: e.rule_id,
            outcome: e.outcome,
            local_dispatch_us: e.local_dispatch_us,
            accept_e2e_ms: e.accept_e2e_ms,
            detail: e.detail,
            created_at: e.created_at,
        }
    }
}

async fn spx_log(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<AcceptEventItem>>, ApiError> {
    let rows = store::list_accept_events(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
    )
    .await?;
    Ok(Json(rows.into_iter().map(AcceptEventItem::from).collect()))
}

#[derive(Debug, Serialize)]
pub struct ManualAcceptResponse {
    pub ok: bool,
    pub reason: String,
    pub message: String,
}

/// Maps `spx_client::AcceptReason` to the SAME `outcome` vocabulary `accept_events.outcome`'s
/// CHECK constraint allows (`'accepted' | 'rejected' | 'skipped' | 'taken_by_agency' | 'failed'
/// | 'agency_dup_unverified'`, migration 0008) — `Skipped`/`Rejected` never occur on THIS path
/// (this route only reaches `accept_booking` after `try_claim_manual` already returned `Ok`),
/// so only the remaining 4 variants are mapped.
///
/// `AgencyDup` is deliberately treated as an unconditional failure here, UNLIKE the auto-accept
/// path (`poller::dispatch::dispatch_booking`), which runs `executor::verify_agency_dup` and can
/// still resolve `AgencyDup` to a win (`Ours`/`Inconclusive`). A manual accept skips that
/// verification — the `agency_dup_unverified` outcome name says so honestly, and this is a
/// disclosed scope simplification (review finding, Task 10), not an oversight.
fn outcome_for(reason: spx_client::AcceptReason) -> &'static str {
    match reason {
        spx_client::AcceptReason::Ok => "accepted",
        spx_client::AcceptReason::AgencyDup => "agency_dup_unverified",
        spx_client::AcceptReason::Taken => "taken_by_agency",
        spx_client::AcceptReason::Transient
        | spx_client::AcceptReason::Auth
        | spx_client::AcceptReason::Error => "failed",
    }
}

/// `POST /bookings/:id/accept` — manual accept. NO `require_permission` gate — only
/// `session_auth` (any logged-in tenant member may manually accept); see this file's module doc
/// for the disclosed rationale (matches Task 8/9's read routes' precedent).
async fn accept(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<ManualAcceptResponse>, ApiError> {
    let booking = store::bookings::get_detail(&state.poller.pool, user.tenant_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if booking.status != "pending" {
        return Err(ApiError::Conflict(format!(
            "booking is not pending (status: {})",
            booking.status
        )));
    }

    let (dedup, manual_tx) = {
        let handle = state
            .poller
            .accounts
            .get(&booking.account_id)
            .ok_or_else(|| {
                ApiError::Conflict(
                    "the account this booking belongs to is not currently connected".to_string(),
                )
            })?;
        (handle.dedup.clone(), handle.manual_accept.clone())
    };

    match state
        .poller
        .executor
        .try_claim_manual(&booking.account_id, &booking.spx_id, &dedup)
        .await
    {
        executor::ManualClaimOutcome::AlreadyAccepted => {
            return Err(ApiError::Conflict(
                "booking is already claimed or accepted".to_string(),
            ));
        }
        executor::ManualClaimOutcome::Ok => {}
    }

    let spx_booking = spx_client::normalize_booking(&booking.raw_data);
    let booking_id_i64 = spx_booking.booking_id.parse::<i64>().unwrap_or(0);
    let request_ids: Vec<i64> = spx_booking
        .request_id
        .parse::<i64>()
        .ok()
        .into_iter()
        .collect();

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    manual_tx
        .send(poller::ManualAcceptRequest {
            booking_id: booking_id_i64,
            request_ids,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            ApiError::Internal("account task is not accepting manual requests".to_string())
        })?;

    let result = tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx)
        .await
        .map_err(|_| ApiError::Internal("manual accept dispatch timed out".to_string()))?
        .map_err(|_| ApiError::Internal("account task dropped the manual accept reply".to_string()))?;

    let outcome = outcome_for(result.reason);

    if matches!(result.reason, spx_client::AcceptReason::Ok) {
        dedup.commit_accept(&booking.spx_id);
        let _ = state
            .poller
            .executor
            .record_durable_accept(&booking.account_id, &booking.spx_id)
            .await;
        let _ = store::update_booking_status(
            &state.poller.pool,
            user.tenant_id,
            &booking.spx_id,
            store::BookingStatusUpdate {
                status: "accepted",
                latency_ms: None,
                auto_accepted: false,
                rule_matched: None,
                accept_reason: None,
            },
        )
        .await;
    } else {
        // Best-effort: release the Layer-2 claim so a retry isn't blocked for the full 600s
        // TTL. `rule_id: None` — manual accepts never populate the inflight quota set, so this
        // is a harmless no-op SREM against a set that was never written to.
        state
            .poller
            .executor
            .release_claim_auto(&booking.account_id, &booking.spx_id, None)
            .await;
        dedup.abort_accept(&booking.spx_id);
        let _ = store::update_booking_status(
            &state.poller.pool,
            user.tenant_id,
            &booking.spx_id,
            store::BookingStatusUpdate {
                status: "failed",
                latency_ms: None,
                auto_accepted: false,
                rule_matched: None,
                accept_reason: Some("manual_accept_failed"),
            },
        )
        .await;
    }

    let _ = store::insert_accept_event(
        &state.poller.pool,
        user.tenant_id,
        &store::NewAcceptEvent {
            booking_id: Some(booking.id),
            rule_id: None,
            outcome: outcome.to_string(),
            local_dispatch_us: None,
            accept_e2e_ms: None,
            detail: serde_json::json!({
                "manual": true,
                "retcode": result.retcode,
                "message": result.message,
            }),
        },
    )
    .await;

    Ok(Json(ManualAcceptResponse {
        ok: matches!(result.reason, spx_client::AcceptReason::Ok),
        reason: outcome.to_string(),
        message: result.message,
    }))
}

/// Nested at `/bookings` by `build_router`. Task 10 appends `.route("/{id}/accept", post(...))`
/// to this SAME function (do not create a second router for it — one `/bookings` prefix, one
/// router, per this crate's established one-router-per-resource convention).
pub fn bookings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/live", get(live))
        .route("/history", get(history))
        .route("/{id}/detail", get(detail))
        .route("/spx-log", get(spx_log))
        .route("/{id}/accept", post(accept))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
