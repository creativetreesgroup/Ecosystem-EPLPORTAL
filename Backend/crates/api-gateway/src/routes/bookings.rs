// Backend/crates/api-gateway/src/routes/bookings.rs
//! `GET /bookings/live`, `/bookings/history`, `/bookings/:id/detail`, `/bookings/spx-log` —
//! read-only booking + accept-event-audit views, and (Task 10) `POST /bookings/:id/accept` —
//! manual accept. Every route here needs only `session_auth` (any logged-in tenant member);
//! see this file's own `require_permission` usage (Task 10's handler) for the one exception's
//! rationale.
use axum::extract::{Extension, Path, Query, State};
use axum::routing::get;
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

/// Nested at `/bookings` by `build_router`. Task 10 appends `.route("/{id}/accept", post(...))`
/// to this SAME function (do not create a second router for it — one `/bookings` prefix, one
/// router, per this crate's established one-router-per-resource convention).
pub fn bookings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/live", get(live))
        .route("/history", get(history))
        .route("/{id}/detail", get(detail))
        .route("/spx-log", get(spx_log))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
