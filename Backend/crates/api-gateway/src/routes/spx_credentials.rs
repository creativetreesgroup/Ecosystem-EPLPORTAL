// Backend/crates/api-gateway/src/routes/spx_credentials.rs
//! `GET/PUT/DELETE /auth/spx-credentials` — envelope-encrypted SPX login
//! storage. The decrypted password, the ciphertext, and the nonce NEVER
//! appear in any response body: `CredentialSummary` below is the ONLY shape
//! ever returned by this module, and it carries just `{label, username}`.
//!
//! RBAC split (binding, from the Fase 6b plan): `GET` requires only a valid
//! session (`session_auth`, applied to the whole router below) — ANY logged-
//! in tenant member may list labels/usernames, matching this project's
//! single-tenant data-visibility model. `PUT`/`DELETE` additionally require
//! `require_permission(Permission::ManageSpxCredentials)` (main-account
//! only), checked inside the handler so the 403 is `ApiError::Forbidden`
//! rather than a bespoke middleware-level rejection — same pattern as every
//! other `require_permission` call site in this crate.
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::{encrypt_agency_password, KEY_VERSION};

/// The ONLY shape this module ever returns. Deliberately excludes
/// `ciphertext`/`nonce`/the decrypted password — see this file's top comment.
#[derive(Debug, Serialize)]
pub struct CredentialSummary {
    pub label: String,
    pub username: String,
}

/// No `Debug` derive: this struct carries the plaintext `password` from the
/// request body, and a `Debug`/`{:?}` impl is exactly the kind of thing a
/// future `tracing::debug!(?body)` could reach for without realizing it logs
/// a raw credential (review finding — Fase 6b Task 2).
#[derive(Deserialize)]
pub struct UpsertCredential {
    pub username: String,
    pub password: String,
}

async fn list(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<Vec<CredentialSummary>>, ApiError> {
    let rows = store::agency_credentials::list_all(&state.poller.pool, user.tenant_id).await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| CredentialSummary {
                label: r.label,
                username: r.username,
            })
            .collect(),
    ))
}

/// Idempotent create-or-update by `(tenant_id, label)`. Always returns `200`
/// — `PUT`'s own semantics ("replace the resource at this URL, whether or not
/// it already existed") don't require distinguishing create-vs-update via
/// status code, and collapsing to one status keeps the handler simpler than
/// threading a "was this a create" bool out to the response layer for no
/// caller-visible benefit.
///
/// `existing.is_some()` then a SEPARATE `create`/`update` call below is a
/// benign TOCTOU: another request could insert the same `(tenant_id, label)`
/// row between the check and this handler's own write. On this admin-only,
/// low-traffic, rare-double-submit endpoint that just surfaces as
/// `ApiError::Conflict` (`create`'s `23505` mapped by `impl From<sqlx::Error>
/// for ApiError`) instead of a clean `200` — not a security issue, so a
/// single atomic `INSERT ... ON CONFLICT DO UPDATE` is intentionally not
/// built here (see the task brief for the explicit call not to over-engineer
/// this).
async fn upsert(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
    Json(body): Json<UpsertCredential>,
) -> Result<Json<CredentialSummary>, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    if body.username.trim().is_empty() || body.password.is_empty() {
        return Err(ApiError::BadRequest(
            "username and password are required".to_string(),
        ));
    }

    let ct = encrypt_agency_password(&state.master_key, user.tenant_id, &body.password)
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?;

    let existing =
        store::agency_credentials::find_by_label(&state.poller.pool, user.tenant_id, &label)
            .await?;
    let row = if existing.is_some() {
        store::agency_credentials::update(
            &state.poller.pool,
            user.tenant_id,
            &label,
            &body.username,
            &ct.bytes,
            &ct.nonce,
            KEY_VERSION,
        )
        .await?
        .ok_or(ApiError::NotFound)?
    } else {
        store::agency_credentials::create(
            &state.poller.pool,
            user.tenant_id,
            &label,
            &body.username,
            &ct.bytes,
            &ct.nonce,
            KEY_VERSION,
        )
        .await?
    };

    Ok(Json(CredentialSummary {
        label: row.label,
        username: row.username,
    }))
}

async fn remove(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    let deleted =
        store::agency_credentials::delete(&state.poller.pool, user.tenant_id, &label).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

/// Nested at `/auth/spx-credentials` by `build_router`. The WHOLE router is
/// `session_auth`-protected via `route_layer` (same pattern `auth_router`'s
/// `protected` sub-router already established) — `PUT`/`DELETE` layer
/// `require_permission` on top, INSIDE the handler, not as a second
/// `route_layer`, since `require_permission` needs the `CurrentUser` the
/// `session_auth` middleware just inserted.
pub fn spx_credentials_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/{label}", put(upsert).delete(remove))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
