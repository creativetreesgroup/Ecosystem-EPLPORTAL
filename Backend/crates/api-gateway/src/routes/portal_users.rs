// Backend/crates/api-gateway/src/routes/portal_users.rs
//! `GET/POST/DELETE /auth/portal-users` — sub-user management.
//!
//! RBAC split (binding, from the Fase 6b plan): `GET` requires only a valid
//! session (`session_auth`, applied to the whole router below) — ANY logged-
//! in tenant member may list, matching `GET /auth/spx-credentials`'s
//! established RBAC posture (a read within one's own tenant). `POST`/`DELETE`
//! additionally require `require_permission(Permission::ManageSubUsers)`
//! (main-account only), checked inside the handler — same pattern as
//! `spx_credentials.rs`.
//!
//! `PortalUserSummary` is the ONLY shape this module ever returns — it
//! deliberately excludes `password_hash` (and `tenant_id`/`created_at`/
//! `updated_at`, which no caller of this route needs).
use axum::extract::{Extension, Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::password::hash_password;

#[derive(Debug, Serialize)]
pub struct PortalUserSummary {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_main_account: bool,
    pub enabled: bool,
}

impl From<store::models::PortalUser> for PortalUserSummary {
    fn from(u: store::models::PortalUser) -> Self {
        Self {
            id: u.id,
            username: u.username,
            display_name: u.display_name,
            is_main_account: u.is_main_account,
            enabled: u.enabled,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePortalUser {
    pub username: String,
    pub password: String,
    pub display_name: String,
    #[serde(default)]
    pub is_main_account: bool,
}

async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<PortalUserSummary>>, ApiError> {
    let rows = store::portal_users::list_all(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// `require_permission(ManageSubUsers)` gated — main-account only. Minimum
/// password length (>= 8 chars) is enforced here, not in `store`, matching
/// this crate's established layering (`spx_credentials::upsert`'s own
/// non-empty-username/password check lives at the handler level, not in
/// `store::agency_credentials`).
async fn create(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<CreatePortalUser>,
) -> Result<Json<PortalUserSummary>, ApiError> {
    require_permission(&user, Permission::ManageSubUsers)?;
    if body.username.trim().is_empty() || body.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "username required, password must be >= 8 chars".to_string(),
        ));
    }
    let hash = hash_password(&body.password).map_err(|e| ApiError::Internal(format!("{e:?}")))?;
    let row = store::portal_users::create(
        &state.poller.pool,
        user.tenant_id,
        &body.username,
        &hash,
        &body.display_name,
        body.is_main_account,
    )
    .await?;
    Ok(Json(row.into()))
}

/// `require_permission(ManageSubUsers)` gated — main-account only. Self-
/// lockout guard: `id == user.portal_user_id` is rejected with `400` BEFORE
/// the `store::portal_users::delete` call — a real, if edge-case, risk (a
/// main account deleting its own row would leave the tenant with no way to
/// manage sub-users, since this route's own writes are main-account-gated).
async fn remove(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    require_permission(&user, Permission::ManageSubUsers)?;
    if id == user.portal_user_id {
        return Err(ApiError::BadRequest(
            "cannot delete your own account".to_string(),
        ));
    }
    let deleted = store::portal_users::delete(&state.poller.pool, user.tenant_id, id).await?;
    if deleted {
        Ok(axum::http::StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Nested at `/auth/portal-users` by `build_router`. The WHOLE router is
/// `session_auth`-protected via `route_layer` (same pattern
/// `spx_credentials_router` already established) — `POST`/`DELETE` layer
/// `require_permission` on top, INSIDE the handler, not as a second
/// `route_layer`, since `require_permission` needs the `CurrentUser` the
/// `session_auth` middleware just inserted.
pub fn portal_users_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", axum::routing::delete(remove))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
