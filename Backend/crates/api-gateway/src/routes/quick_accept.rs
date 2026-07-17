// Backend/crates/api-gateway/src/routes/quick_accept.rs
//! `GET/POST /q/:token` — the HMAC quick-accept flow: the login-free "quick accept" link
//! embedded in a WhatsApp notification. Mounted OUTSIDE `session_auth` (the token itself IS the
//! authorization, matching the reference — there is no session cookie on this route at all).
//! `GET` returns a minimal HTML confirmation page (this crate's first non-JSON response — Fase
//! 7's Command Center replaces this page's styling entirely, so intentionally undecorated:
//! correct state, no CSS-parity attempt with the reference). `POST` returns JSON
//! `{ok, reason, message}` with REAL HTTP status codes on failure (400/404/410/409/500) — a
//! disclosed deviation from the reference's blanket 200 (this crate's established convention
//! everywhere else uses accurate status codes; the JSON body shape still lets a client render a
//! message either way).
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::state::AppState;
use spx_client::crypto::quick_token::verify_quick_token;

use super::bookings::{execute_manual_accept, ManualAcceptResponse};

/// Printable token shape — reject anything malformed BEFORE touching crypto/DB. Mirrors the
/// reference's `VALID_CODE` regex intent (`^[A-Za-z0-9_.\-]{4,512}$`) without a `regex`
/// dependency (this crate has none): `sign_quick_token`'s own output is
/// `<url-safe-no-pad-base64>.<url-safe-no-pad-base64>`, so the allowed charset below is exactly
/// that alphabet plus the single `.` separator.
fn is_valid_token_shape(s: &str) -> bool {
    let len = s.len();
    (4..=512).contains(&len)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Narrowly relaxed CSP for `confirmation_page` ONLY — the global default
/// (`middleware::security_headers`) is `default-src 'none'` with no `script-src`/`connect-src`,
/// which a real browser enforces by silently blocking BOTH this page's inline `<script>` from
/// running and its `fetch()` from firing, leaving the "Terima Tiket Sekarang" button inert.
/// `'unsafe-inline'` is the pragmatic, disclosed choice for this stopgap page (no nonce
/// infrastructure exists yet; Fase 7's Command Center replaces this page's styling/mechanism
/// entirely — see the file-level doc comment). `connect-src 'self'` is only what the page's own
/// same-origin `fetch()` to `/q/accept` needs. Everything else stays as locked down as the
/// global default: no `frame-ancestors`, no `object-src`, `base-uri 'none'`.
const CONFIRMATION_PAGE_CSP: &str =
    "default-src 'none'; script-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'";

/// Minimal HTML-escaping for interpolating untrusted, SPX-platform-sourced text (`spx_id`) into
/// `confirmation_page`'s markup. `spx_id` is never validated as HTML-safe anywhere in the
/// ingestion pipeline, so this is a display-only safety net — NOT used anywhere near
/// `post_body`'s JSON, which has its own separate, already-safe path via `{token:?}` +
/// `is_valid_token_shape`'s charset gate.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn error_page(status: StatusCode, text: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Terima Tiket</title></head>\
         <body style=\"font-family:sans-serif;max-width:420px;margin:60px auto;padding:0 20px;text-align:center\">\
         <p>{text}</p></body></html>"
    );
    (status, Html(body)).into_response()
}

/// Booking-state confirmation page. `post_url`/`post_body` are the exact endpoint/JSON body the
/// page's own `fetch()` posts back to (`/q/accept`, with `{"token": "..."}`).
fn confirmation_page(spx_id: &str, status: &str, post_url: &str, post_body: &str) -> Response {
    let (label, disabled) = match status {
        "accepted" => ("Tiket sudah diterima", true),
        "gone" => ("Tiket tidak tersedia lagi", true),
        _ => ("Terima tiket ini", false),
    };
    let btn_attr = if disabled { "disabled" } else { "" };
    let spx_id_escaped = html_escape(spx_id);
    let body = format!(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Terima Tiket</title></head>\
         <body style=\"font-family:sans-serif;max-width:420px;margin:40px auto;padding:0 20px\">\
         <p>Booking ID: <b>{spx_id_escaped}</b></p>\
         <p>{label}</p>\
         <button id=\"go\" {btn_attr} style=\"width:100%;padding:14px;font-size:16px\">Terima Tiket Sekarang</button>\
         <p id=\"msg\"></p>\
         <script>\
         var POST={post_url:?};var BODY={post_body};\
         document.getElementById('go').onclick=async function(){{\
           var b=this,m=document.getElementById('msg');b.disabled=true;\
           try{{var r=await fetch(POST,{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify(BODY)}});\
           var d=await r.json();m.textContent=d.message||(d.ok?'Berhasil diterima.':'Gagal.');\
           if(!d.ok)b.disabled=false;}}catch(e){{m.textContent='Koneksi gagal.';b.disabled=false;}}\
         }};\
         </script></body></html>"
    );
    let mut response = Html(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static(CONFIRMATION_PAGE_CSP),
    );
    response
}

async fn get_quick_token(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    if !is_valid_token_shape(&token) {
        return error_page(StatusCode::BAD_REQUEST, "Tautan tidak valid.");
    }
    // `state.tenant_id` (this deployment's single resolved tenant, NOT any session-derived
    // value — there is no session on this route) both scopes the HMAC verification AND the
    // booking lookup below, so a token signed for a different tenant can never resolve a
    // booking here even if the same `spx_id` string existed under that other tenant.
    let Some(claims) = verify_quick_token(&state.master_key, state.tenant_id, &token, now_ms())
    else {
        return error_page(StatusCode::GONE, "Tautan sudah kedaluwarsa atau tidak valid.");
    };
    let booking =
        match store::bookings::get_by_spx_id(&state.poller.pool, state.tenant_id, &claims.spx_id)
            .await
        {
            Ok(Some(b)) => b,
            Ok(None) => return error_page(StatusCode::NOT_FOUND, "Tiket tidak ditemukan."),
            Err(_) => {
                return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Terjadi kesalahan.")
            }
        };
    let page_status = match booking.status.as_str() {
        "accepted" => "accepted",
        "pending" => "available",
        _ => "gone",
    };
    confirmation_page(
        &claims.spx_id,
        page_status,
        "/q/accept",
        &format!("{{\"token\":{token:?}}}"),
    )
}

#[derive(Debug, Deserialize)]
struct QuickAcceptBody {
    token: String,
}

/// Maps every `execute_manual_accept` outcome to the SAME status-code convention the existing
/// session-gated `POST /bookings/:id/accept` uses (`routes/bookings.rs::accept`'s own doc
/// comment) — same failure, same status code, regardless of which route reached it:
/// - `"not_pending" | "account_offline" | "already_claimed"` → 409 (conflict-shaped).
/// - `"dispatch_failed" | "timeout" | "reply_dropped"` → 500 (internal-shaped).
/// - anything else (`"accepted"`, and the executor's own non-win outcomes `"taken_by_agency"`,
///   `"agency_dup_unverified"`, `"failed"`) → 200, matching the session-gated route: a
///   dispatched-but-lost attempt is still a successful HTTP round-trip, not a client/server
///   error, regardless of `ok`'s value.
fn status_for(response: &ManualAcceptResponse) -> StatusCode {
    match response.reason.as_str() {
        "not_pending" | "account_offline" | "already_claimed" => StatusCode::CONFLICT,
        "dispatch_failed" | "timeout" | "reply_dropped" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::OK,
    }
}

async fn post_quick_accept(
    State(state): State<AppState>,
    Json(body): Json<QuickAcceptBody>,
) -> (StatusCode, Json<ManualAcceptResponse>) {
    if !is_valid_token_shape(&body.token) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ManualAcceptResponse {
                ok: false,
                reason: "bad_request".to_string(),
                message: "Permintaan tidak valid".to_string(),
            }),
        );
    }
    let Some(claims) =
        verify_quick_token(&state.master_key, state.tenant_id, &body.token, now_ms())
    else {
        return (
            StatusCode::GONE,
            Json(ManualAcceptResponse {
                ok: false,
                reason: "expired_or_invalid".to_string(),
                message: "Tautan tidak valid atau kedaluwarsa".to_string(),
            }),
        );
    };

    let booking =
        match store::bookings::get_by_spx_id(&state.poller.pool, state.tenant_id, &claims.spx_id)
            .await
        {
            Ok(Some(b)) => b,
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ManualAcceptResponse {
                        ok: false,
                        reason: "not_found".to_string(),
                        message: "Tiket tidak ditemukan".to_string(),
                    }),
                );
            }
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ManualAcceptResponse {
                        ok: false,
                        reason: "internal".to_string(),
                        message: "Terjadi kesalahan".to_string(),
                    }),
                );
            }
        };

    let result = execute_manual_accept(&state, state.tenant_id, &booking).await;
    let status = status_for(&result);
    (status, Json(result))
}

/// Mounted at `/q` in `build_router`. No `session_auth`/`require_permission` layer — the token
/// itself is the authorization, matching the reference's login-free "quick accept" link. Rate
/// limiting for this public endpoint is a follow-up hardening item (tracked separately, not part
/// of this task), same as every other `*_router` fn in this crate keeps cross-cutting layers out
/// of the router-construction fn itself.
pub fn hmac_router(_state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{token}", get(get_quick_token))
        .route("/accept", post(post_quick_accept))
}
