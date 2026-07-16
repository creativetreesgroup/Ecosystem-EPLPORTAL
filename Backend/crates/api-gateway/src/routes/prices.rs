// Backend/crates/api-gateway/src/routes/prices.rs
//! `GET /prices` — public, no `session_auth`, rate-limited 120/min/IP instead. The first route in
//! this crate with no session concept at all (unlike `POST /auth/portal-login`, which merely
//! doesn't yet HAVE a session — this route never authenticates anyone, for anyone, ever).
//! `POST/PUT/DELETE /prices` are `session_auth` + `Permission::ManagePrices`-gated, following
//! this crate's established mutation-gating convention.
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct RoutePriceItem {
    pub id: Uuid,
    pub route_code: String,
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
}

impl From<store::models::RoutePrice> for RoutePriceItem {
    fn from(r: store::models::RoutePrice) -> Self {
        Self {
            id: r.id,
            route_code: r.route_code,
            region: r.region,
            origin: r.origin,
            destinations: r.destinations,
            price: r.price,
            vehicle_type: r.vehicle_type,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PriceInput {
    pub route_code: String,
    #[serde(default)]
    pub region: String,
    pub origin: String,
    pub destinations: Value,
    pub price: i64,
    pub vehicle_type: String,
}

/// Validates `destinations` is a JSON array of 1-5 non-empty strings — mirrors the DB's OWN
/// `route_prices_destinations_1to5` CHECK constraint (migration 0013) at the HTTP layer, so a
/// malformed request gets a clear `400` instead of an opaque `500` from a raw constraint
/// violation (`ApiError::From<sqlx::Error>` maps any non-`23505` DB error, including a CHECK
/// violation, to `Internal`/`500` — this validation exists specifically to avoid that for the
/// common, easily-anticipated case).
fn validate_destinations(v: &Value) -> Result<(), ApiError> {
    let arr = v
        .as_array()
        .ok_or_else(|| ApiError::BadRequest("destinations must be a JSON array".to_string()))?;
    if arr.is_empty() || arr.len() > 5 {
        return Err(ApiError::BadRequest(
            "destinations must have between 1 and 5 entries".to_string(),
        ));
    }
    if !arr.iter().all(|d| d.as_str().is_some_and(|s| !s.trim().is_empty())) {
        return Err(ApiError::BadRequest(
            "every destination must be a non-empty string".to_string(),
        ));
    }
    Ok(())
}

fn to_new_route_price(input: &PriceInput) -> store::NewRoutePrice {
    store::NewRoutePrice {
        route_code: input.route_code.trim().to_string(),
        region: input.region.trim().to_string(),
        origin: input.origin.trim().to_string(),
        destinations: input.destinations.clone(),
        price: input.price,
        vehicle_type: input.vehicle_type.trim().to_string(),
    }
}

async fn list_prices(State(state): State<AppState>) -> Result<Json<Vec<RoutePriceItem>>, ApiError> {
    let rows = store::list_route_prices(&state.poller.pool, state.tenant_id).await?;
    Ok(Json(rows.into_iter().map(RoutePriceItem::from).collect()))
}

async fn create_price(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<PriceInput>,
) -> Result<Json<RoutePriceItem>, ApiError> {
    require_permission(&user, Permission::ManagePrices)?;
    validate_destinations(&body.destinations)?;
    let row = store::create_route_price(&state.poller.pool, user.tenant_id, &to_new_route_price(&body))
        .await?;
    Ok(Json(RoutePriceItem::from(row)))
}

async fn update_price(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    Json(body): Json<PriceInput>,
) -> Result<Json<RoutePriceItem>, ApiError> {
    require_permission(&user, Permission::ManagePrices)?;
    validate_destinations(&body.destinations)?;
    let row = store::update_route_price(&state.poller.pool, user.tenant_id, id, &to_new_route_price(&body))
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(RoutePriceItem::from(row)))
}

async fn delete_price(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManagePrices)?;
    let deleted = store::delete_route_price(&state.poller.pool, user.tenant_id, id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /` (public, rate-limited) merged with `POST /`, `PUT/DELETE /{id}` (session_auth +
/// `ManagePrices`) — same `public.merge(protected)` shape `routes/auth.rs::auth_router` already
/// established for `/portal-login` vs. `/me`+`/logout`. Different HTTP methods at the SAME path
/// (`GET "/"` in `public`, `POST "/"` in `protected`) compose cleanly under `Router::merge` —
/// axum only rejects merging the SAME method at the same path twice, not different methods.
pub fn prices_router(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/", get(list_prices))
        .route_layer(crate::middleware::public_rate_limit_layer());

    let protected = Router::new()
        .route("/", post(create_price))
        .route("/{id}", put(update_price).delete(delete_price))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth));

    public.merge(protected)
}
