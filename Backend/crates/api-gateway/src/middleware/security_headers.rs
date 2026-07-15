// Backend/crates/api-gateway/src/middleware/security_headers.rs
//! Fixed security headers on every api-gateway response, incl. a real CSP
//! (the reference has none — master spec explicitly requires it; see design
//! doc correction #1).
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        "Strict-Transport-Security",
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    h.insert("X-XSS-Protection", HeaderValue::from_static("0"));
    h.insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    h.insert(
        "Permissions-Policy",
        HeaderValue::from_static("geolocation=(), camera=(), microphone=()"),
    );
    h.insert(
        "Content-Security-Policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; base-uri 'none'"),
    );
    res
}
