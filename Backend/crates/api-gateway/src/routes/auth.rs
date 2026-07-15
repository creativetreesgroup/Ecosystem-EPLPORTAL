// Backend/crates/api-gateway/src/routes/auth.rs
//! `POST /auth/portal-login`, `GET /auth/me`, `POST /auth/logout`.
//!
//! Security notes (see task brief for the full rationale):
//! - Unknown-username and known-username/wrong-password are indistinguishable
//!   to the caller: same 401 status, same JSON body shape, AND (below) the
//!   same rough wall-clock cost, since a real `verify_password` call runs on
//!   both paths (never short-circuited before hashing).
//! - The plaintext session token is set ONLY via the `Set-Cookie` header
//!   (`CookieJar`/`IntoResponseParts`), never placed in a JSON response body.
//! - `logout` deletes the EXACT session row named by `CurrentUser::session_id`
//!   — never all sessions for the user — so other concurrent devices/tabs
//!   stay logged in.
use std::sync::OnceLock;

use axum::extract::{Extension, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use chrono::Duration;
use serde::{Deserialize, Serialize};

use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::password::{hash_password, verify_password};
use spx_client::crypto::secret::ExposeSecret;
use spx_client::crypto::session_token::generate_session_token;

/// Server-side session lifetime. Enforced independently by
/// `store::portal_sessions::find_valid_by_hash` (the `expires_at` check runs
/// on every authenticated request via `session_auth`), so this constant is
/// the single source of truth for how long a login is good for.
const SESSION_TTL: Duration = Duration::hours(12);

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Shape returned by both `POST /auth/portal-login` and `GET /auth/me` —
/// deliberately does NOT include the session token (that goes only in the
/// `Set-Cookie` header) or anything else that isn't already safe to log.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub username: String,
    pub display_name: String,
    pub is_main_account: bool,
}

/// A fixed, validly-formatted argon2id PHC hash that no real user password
/// will ever match, used only to keep `verify_password`'s cost identical on
/// the "username not found" path (see `portal_login`'s enumeration-timing
/// comment below). Computed once per process via `OnceLock` rather than a
/// hardcoded string literal, so it stays valid even if argon2 parameters
/// change — `hash_password` itself is the only thing that needs to agree
/// with `verify_password` on format.
fn dummy_password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        hash_password("timing-safety-dummy-password-never-assigned-to-a-real-user")
            .expect("hash the fixed dummy password used for enumeration timing defense")
    })
}

async fn portal_login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> Result<(CookieJar, Json<LoginResponse>), ApiError> {
    let user =
        store::portal_users::find_by_username(&state.poller.pool, state.tenant_id, &body.username)
            .await?;

    // Always call `verify_password` exactly once, on both branches — an
    // unknown username still pays the full argon2id cost against a dummy
    // hash, instead of returning near-instantly. Skipping this (i.e.
    // short-circuiting via `.ok_or(ApiError::Unauthorized)?` before ever
    // hashing) would let a timing attacker distinguish "no such user" from
    // "wrong password" even though the status code and JSON body are
    // already identical on both paths.
    let password_ok = match &user {
        Some(u) => verify_password(&body.password, &u.password_hash),
        None => {
            verify_password(&body.password, dummy_password_hash());
            false
        }
    };

    let user = match user {
        Some(u) if u.enabled && password_ok => u,
        _ => return Err(ApiError::Unauthorized),
    };

    let (token, hash) =
        generate_session_token().map_err(|e| ApiError::Internal(format!("{e:?}")))?;
    store::portal_sessions::create(
        &state.poller.pool,
        state.tenant_id,
        user.id,
        hash,
        None,
        None,
        SESSION_TTL,
    )
    .await?;

    let cookie = Cookie::build((state.session_cookie_name.to_string(), {
        token.expose_secret().to_string()
    }))
    .http_only(true)
    .secure(state.cookie_secure)
    .same_site(SameSite::Strict)
    .path("/")
    .build();

    Ok((
        jar.add(cookie),
        Json(LoginResponse {
            username: user.username,
            display_name: user.display_name,
            is_main_account: user.is_main_account,
        }),
    ))
}

async fn me(Extension(user): Extension<CurrentUser>) -> Json<LoginResponse> {
    Json(LoginResponse {
        username: user.username,
        display_name: user.display_name,
        is_main_account: user.is_main_account,
    })
}

async fn logout(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    jar: CookieJar,
) -> Result<CookieJar, ApiError> {
    // Delete the EXACT session row `session_id` names — a user may have
    // several concurrent sessions across devices; logout must not touch any
    // session but the one presented (never "delete all of this user's
    // sessions").
    store::portal_sessions::delete(&state.poller.pool, user.tenant_id, user.session_id).await?;

    // The removal cookie must carry the SAME `path` as the cookie originally
    // set in `portal_login` (`"/"`); `cookie::CookieJar::remove`'s own doc
    // comment is explicit that a mismatched (here: absent) path produces a
    // removal `Set-Cookie` scoped to the request's own path
    // (`/auth/logout`'s directory, not `/`), which a real browser would NOT
    // apply to the original `Path=/` cookie — leaving it alive client-side
    // even though the server-side row above is already gone.
    let removal = Cookie::build(state.session_cookie_name.to_string())
        .path("/")
        .build();
    Ok(jar.remove(removal))
}

/// `/portal-login` has no `session_auth` layer (that's exactly the route
/// that establishes a session); `/me` and `/logout` are nested under a
/// sub-router with `session_auth` applied via `route_layer` so only those
/// two require an existing session.
///
/// `/portal-login` instead gets its own `route_layer`
/// (`middleware::login_rate_limit_layer`, Task 8): a per-IP ~20/min budget
/// scoped to JUST this route via the same `route_layer`-on-a-sub-router
/// pattern `protected` already uses for `session_auth` below — never applied
/// to `/me`/`/logout`, and never mounted globally in `build_router`. A login
/// POST is a credential-stuffing target; `/me`/`/logout` are
/// already-authenticated traffic that doesn't need this stricter budget.
pub fn auth_router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/me", get(me))
        .route("/logout", post(logout))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session_auth,
        ));

    let login = Router::new()
        .route("/portal-login", post(portal_login))
        .route_layer(crate::middleware::login_rate_limit_layer());

    login.merge(protected)
}
