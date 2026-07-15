// Backend/crates/api-gateway/src/error.rs
//! Unified API error → consistent `{"error": "..."}` JSON + status code.
//! Every handler in this crate returns `Result<T, ApiError>`.
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict(String),
    BadRequest(String),
    /// `429 Too Many Requests` — Fase 6b Task 5's OTP-gate cooldown/attempt-cap
    /// family (`OtpRequestError::TooSoon`, `OtpVerifyError::TooManyAttempts`).
    /// Distinct from `Conflict`: those two are rate-limit-shaped rejections
    /// ("come back later"), not a resource-state conflict, and this project
    /// already has a `429` precedent for a different rate-limit mechanism
    /// (`middleware::rate_limit`'s `tower_governor` login limiter) — this
    /// variant gives handler-level (non-`tower_governor`) rate-limit
    /// rejections the same, correct status code.
    TooManyRequests(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::TooManyRequests(m) => (StatusCode::TOO_MANY_REQUESTS, m),
            ApiError::Internal(m) => {
                tracing::error!(error = %m, "internal api-gateway error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// Postgres unique-violation (`23505`) maps to 409, not 500 — a later
/// task's insert into a uniquely-constrained table (e.g. `portal_users
/// (tenant_id, username)`) is a client-side conflict, not a server fault,
/// and must not be logged as one via the generic `Internal` path below.
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db_err) = &e {
            if db_err.code().as_deref() == Some("23505") {
                return ApiError::Conflict("already exists".to_string());
            }
        }
        ApiError::Internal(e.to_string())
    }
}
