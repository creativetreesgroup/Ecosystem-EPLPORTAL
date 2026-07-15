// Backend/crates/api-gateway/src/auth/middleware.rs
//! Session-cookie auth middleware. Runs before every route it's applied to
//! (mounted per-router-group in Task 5's login/me/logout wiring and by every
//! later sub-phase's protected routes) — NOT applied to `/healthz` or the
//! Task 6e quick-accept routes (those are explicitly session-free, per the
//! design doc).
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::CookieJar;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::session_token::hash_session_token;

/// Inserted as a request extension by `session_auth`. Handlers retrieve it
/// via `axum::extract::Extension<CurrentUser>` (no custom `FromRequestParts`
/// impl — the stock `Extension` extractor already gives handlers a typed,
/// infallible-once-the-middleware-has-run pull of this value, and adding a
/// bespoke extractor on top would just be a thinner wrapper around the same
/// `req.extensions()` lookup).
#[derive(Debug, Clone)]
pub struct CurrentUser {
    /// Carried specifically so Task 5's `logout` handler can delete the
    /// EXACT session row (a user may have several concurrent sessions across
    /// devices — logout must not touch any session but the one presented).
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub portal_user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_main_account: bool,
}

pub async fn session_auth(
    State(state): State<AppState>,
    jar: CookieJar,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = jar
        .get(&state.session_cookie_name)
        .map(|c| c.value().to_string())
        .ok_or(ApiError::Unauthorized)?;
    let hash = hash_session_token(&token);

    let session = store::portal_sessions::find_valid_by_hash(&state.poller.pool, hash)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::Unauthorized)?;

    // Look up the user WITHIN the session's own tenant (not `state.tenant_id`
    // — defense in depth in case a future multi-tenant change reintroduces
    // per-request tenant variance; today they're always equal since only one
    // tenant exists, but the session row is the source of truth here).
    let user = store::portal_users::find_by_id(
        &state.poller.pool,
        session.tenant_id,
        session.portal_user_id,
    )
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::Unauthorized)?;
    if !user.enabled {
        return Err(ApiError::Unauthorized);
    }

    let _ =
        store::portal_sessions::touch_last_seen(&state.poller.pool, session.tenant_id, session.id)
            .await;

    req.extensions_mut().insert(CurrentUser {
        session_id: session.id,
        tenant_id: session.tenant_id,
        portal_user_id: user.id,
        username: user.username,
        display_name: user.display_name,
        is_main_account: user.is_main_account,
    });

    Ok(next.run(req).await)
}
