// Backend/crates/api-gateway/src/middleware/cors.rs
//! Exact-match CORS allowlist — NO wildcard origin. The reference's own code
//! comments flag a near-miss ngrok-wildcard CSRF finding; this project does
//! not repeat it. `origins` come from `AppState.cors_origins` (populated at
//! boot from the `CORS_ALLOWED_ORIGINS` env var — see `reactor-core`'s
//! `build_state`).
//!
//! Verified against the ACTUALLY-resolved `tower_http = 0.7.0` (this crate's
//! first direct use of `tower_http`; previously only a transitive dependency
//! of `reqwest`), by reading
//! `~/.cargo/registry/src/.../tower-http-0.7.0/src/cors/{mod,allow_origin}.rs`
//! directly rather than assuming the task brief's best-effort snippet:
//! - `CorsLayer::allow_origin<T: Into<AllowOrigin>>` — `Vec<HeaderValue>` has
//!   a `From` impl (`AllowOrigin::list` under the hood), so passing a
//!   `Vec<HeaderValue>` directly (no manual `AllowOrigin::list(..)` wrapping
//!   needed) compiles and behaves as an exact-match allowlist.
//! - `AllowOrigin::list` returns the ORIGIN'S OWN header value back verbatim
//!   only when `origin`s in the request is a member of the configured list
//!   (`origin.filter(|o| l.contains(o))`) — an unlisted origin makes that
//!   `filter` yield `None`, so `Access-Control-Allow-Origin` is simply
//!   OMITTED from the response (not a 4xx/5xx error status). Confirmed
//!   empirically in `tests/cors_and_body_limit.rs`.
//! - `ensure_usable_cors_rules` (called once, when `CorsLayer` is turned into
//!   a `Service` by `Router::layer` at `build_router` time) `assert!`s that
//!   `allow_credentials(true)` is never combined with a WILDCARD
//!   `allow_origin`/`allow_headers`/`allow_methods`/`expose_headers` — this
//!   crate never sets any of those to `Any`, so `allow_credentials(true)` +
//!   an exact-match origin list is accepted, as expected.
use axum::http::{HeaderValue, Method};
use tower_http::cors::CorsLayer;

/// Builds the global CORS layer from the configured exact-match allowlist.
///
/// A configured origin string that fails to parse as a valid `HeaderValue`
/// (a typo'd `CORS_ALLOWED_ORIGINS` entry) is dropped from the allowlist with
/// a `tracing::warn!` rather than panicking the process — a bad entry should
/// be visible in the logs, not take down `reactor-core` at boot.
///
/// A literal `"*"` entry is ALSO dropped (with a warning) rather than passed
/// through to `tower_http`: `AllowOrigin::list` parses `"*"` as a perfectly
/// valid `HeaderValue` and would only reject it by `panic!`king (since it
/// equals `tower_http::cors::Any`'s wildcard constant) the first time the
/// layer is applied — i.e. it would crash `reactor-core` at boot instead of
/// just misconfiguring CORS. Filtering it here keeps the "misconfiguration
/// warns, never panics" contract uniform for every bad entry, and doubles as
/// an explicit, defense-in-depth enforcement of the "NO wildcard origin"
/// constraint (rather than relying solely on nobody ever calling `.allow_origin(Any)`
/// elsewhere in this file).
pub fn cors_layer(origins: &[String]) -> CorsLayer {
    let allowed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| {
            if o.trim() == "*" {
                tracing::warn!(
                    origin = %o,
                    "CORS_ALLOWED_ORIGINS: wildcard origin is not permitted (exact-match \
                     allowlist only) — dropping"
                );
                return None;
            }
            match HeaderValue::from_str(o) {
                Ok(v) => Some(v),
                Err(err) => {
                    tracing::warn!(
                        origin = %o,
                        error = %err,
                        "CORS_ALLOWED_ORIGINS: dropping origin that failed to parse as a valid \
                         HeaderValue"
                    );
                    None
                }
            }
        })
        .collect();

    CorsLayer::new()
        .allow_origin(allowed)
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([axum::http::header::CONTENT_TYPE])
}
