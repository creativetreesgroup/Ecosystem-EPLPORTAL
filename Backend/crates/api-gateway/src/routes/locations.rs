// Backend/crates/api-gateway/src/routes/locations.rs
//! `GET/POST/DELETE /locations` — session-auth-only for `GET` (any tenant member sees the
//! location list, matching this project's established data-visibility model), `Permission::
//! ManageLocations`-gated for `POST`/`DELETE`. No `PUT` — locations are add/delete-only (no
//! `updated_at` column, Task 2's store layer has no `update` fn either).
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct LocationItem {
    pub id: Uuid,
    pub name: String,
}

impl From<store::models::RouteLocation> for LocationItem {
    fn from(l: store::models::RouteLocation) -> Self {
        Self { id: l.id, name: l.name }
    }
}

#[derive(Debug, Deserialize)]
pub struct LocationInput {
    pub name: String,
}

async fn list_locations(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<LocationItem>>, ApiError> {
    let rows = store::list_route_locations(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(rows.into_iter().map(LocationItem::from).collect()))
}

async fn create_location(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<LocationInput>,
) -> Result<Json<LocationItem>, ApiError> {
    require_permission(&user, Permission::ManageLocations)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("name is required".to_string()));
    }
    let row = store::create_route_location(&state.poller.pool, user.tenant_id, name).await?;
    Ok(Json(LocationItem::from(row)))
}

async fn delete_location(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManageLocations)?;
    let deleted = store::delete_route_location(&state.poller.pool, user.tenant_id, id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

pub fn locations_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(list_locations).post(create_location))
        .route("/{id}", axum::routing::delete(delete_location))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
