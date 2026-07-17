// Backend/crates/api-gateway/src/routes/branding.rs
//! `GET /branding` (public, rate-limited) + `PUT /branding` (session_auth + `ManageBranding`).
//! Mounted from a SEPARATELY-layered sub-router in `lib.rs::build_router` — see that fn's own
//! doc comment for why the 15MB body-limit carve-out requires this structural split.
use axum::extract::{Extension, State};
use axum::routing::{get, put};
use axum::{Json, Router};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::branding::{validate_and_normalize, Branding, BrandingInput, SITE_SETTINGS_KEY};
use crate::error::ApiError;
use crate::state::AppState;

async fn get_branding(State(state): State<AppState>) -> Result<Json<Branding>, ApiError> {
    let value = store::site_settings::get(&state.poller.pool, state.tenant_id, SITE_SETTINGS_KEY).await?;
    let branding = match value {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => Branding::default(),
    };
    Ok(Json(branding))
}

async fn put_branding(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<BrandingInput>,
) -> Result<Json<Branding>, ApiError> {
    require_permission(&user, Permission::ManageBranding)?;
    let branding = validate_and_normalize(body).map_err(ApiError::BadRequest)?;
    let value = serde_json::to_value(&branding).map_err(|e| ApiError::Internal(e.to_string()))?;
    store::site_settings::put(&state.poller.pool, user.tenant_id, SITE_SETTINGS_KEY, &value).await?;
    Ok(Json(branding))
}

/// `GET /` (public, `public_rate_limit_layer` — Task 4) merged with `PUT /` (session_auth +
/// `ManageBranding`) — same `public.merge(protected)` shape already established by
/// `routes/prices.rs::prices_router` and `routes/auth.rs::auth_router`.
pub fn branding_router(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/", get(get_branding))
        .route_layer(crate::middleware::public_rate_limit_layer());
    let protected = Router::new()
        .route("/", put(put_branding))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth));
    public.merge(protected)
}
