mod browser;

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use browser::{browser_login, BrowserLoginCfg};

fn app() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/login", post(login))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "auth-sidecar" }))
}

#[derive(Deserialize)]
struct LoginReq {
    #[allow(dead_code)]
    account_id: String,
    username: String,
    password: String,
}

#[derive(Serialize, Default)]
struct CookiesOut {
    fms_user_skey: String,
    fms_user_id: String,
    fms_user_agency_id: String,
    csrftoken: String,
    spx_uk: String,
    spx_cid: String,
    spx_uid: String,
    spx_agid: String,
    spx_st: String,
    ds: String,
    spx_admin_device_id: String,
}

/// `POST /login` — server side of Task 7's `SidecarClient::login` contract.
/// Always returns HTTP 200: `{ok:true, cookies:{...}}` on success or
/// `{ok:false, error}` on any browser-login failure. A 5xx is reserved for a
/// genuine framework/panic error — this is what lets the poller's
/// tier-fallthrough (tier 1 -> 2 -> 3) treat a clean `ok:false` as "try the
/// next tier" rather than "the whole login attempt failed".
async fn login(Json(req): Json<LoginReq>) -> Json<Value> {
    let cfg = BrowserLoginCfg::from_env();
    match browser_login(&cfg, &req.username, &req.password).await {
        Ok(map) => {
            let mut c = CookiesOut::default();
            let g = |k: &str| map.get(k).cloned().unwrap_or_default();
            c.fms_user_skey = g("fms_user_skey");
            c.fms_user_id = g("fms_user_id");
            c.fms_user_agency_id = g("fms_user_agency_id");
            c.csrftoken = g("csrftoken");
            c.spx_uk = g("spx_uk");
            c.spx_cid = g("spx_cid");
            c.spx_uid = g("spx_uid");
            c.spx_agid = g("spx_agid");
            c.spx_st = g("spx_st");
            c.ds = g("ds");
            c.spx_admin_device_id = g("spx-admin-device-id");
            Json(json!({ "ok": true, "cookies": c }))
        }
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("auth-sidecar starting");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8082")
        .await
        .expect("bind 0.0.0.0:8082");

    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl_c handler");
    tracing::info!("auth-sidecar shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "auth-sidecar");
    }

    #[tokio::test]
    async fn login_returns_ok_false_when_browser_unavailable() {
        // With no Chromium in the test env, browser_login returns Err → the handler
        // must respond 200 with {ok:false, error:...} (NEVER a 5xx — the poller's
        // tier-fallthrough relies on a clean ok:false). Proves the request parse +
        // response shape without a real browser.
        std::env::set_var("CHROME_BIN", "/nonexistent/chromium");
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"account_id":"a","username":"u","password":"p"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], false, "browser-unavailable must be a clean ok:false, not a 5xx");
        assert!(json["error"].is_string());
    }
}
