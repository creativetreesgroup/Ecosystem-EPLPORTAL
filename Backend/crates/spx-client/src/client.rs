// Backend/crates/spx-client/src/client.rs
//! SPX HTTP client. Cookie-based auth; Chrome-impersonating transport (wreq).
//! Endpoint paths are the reference's real defaults. The wreq request/response
//! API shape below (`Client::builder()...build()`,
//! `client.post(url).headers(hmap).json(&body).send().await?`, `res.status()`,
//! `res.json::<Value>().await?`) was verified against the pinned
//! `wreq =6.0.0-rc.29` source (docs.rs / `~/.cargo/registry`) — no reconciliation
//! was needed; the brief's assumed shape matches exactly.
use std::time::Duration;

use serde_json::{json, Value};

use crate::accept::{classify_accept_response, AcceptReason, AcceptResult};
use crate::booking::{normalize_booking, SpxBooking};
use crate::cookies::{build_headers, SpxCookies};

pub const PATH_BIDDING_LIST: &str = "/api/line_haul/agency/booking/bidding/list";
pub const PATH_COUNT_V2: &str = "/api/line_haul/agency/booking/bidding/count_v2";
pub const PATH_REQUEST_LIST: &str = "/api/line_haul/agency/booking/bidding/request/list";
pub const PATH_ACCEPT: &str = "/api/line_haul/agency/booking/bidding/accept";
pub const PATH_NOTIFICATION: &str =
    "/api/basicserver/agency/notification/pn/pending/read/count";
pub const PATH_BIDDING_LOG_LIST: &str = "/api/line_haul/agency/booking/bidding/log/list";
pub const PATH_USER_LIST: &str = "/api/basicserver/agency/account/user/list";
pub const PATH_PROFILE: &str = "/api/basicserver/agency/account/current_user/basic_info";
pub const PATH_BOOKING_OVERVIEW: &str =
    "/api/line_haul/agency/booking/bidding/booking_overview";
pub const PATH_BOOKING_LOG: &str = "/api/line_haul/agency/booking/request/booking_log";

#[derive(Debug, thiserror::Error)]
pub enum SpxError {
    #[error("http transport error")]
    Transport,
    #[error("http status {0}")]
    Status(u16),
    #[error("bad response body")]
    Body,
}

pub struct SpxClient {
    http: wreq::Client,
    base_url: String,
}

impl SpxClient {
    /// Build the client. The Chrome emulation preset (`Chrome148`) must match
    /// `cookies::CHROME_MAJOR`'s client-hints (also 148) — Task 8's decision;
    /// keep both in lockstep. An inconsistent TLS-fingerprint/header pairing is
    /// a more obvious impersonation signal than either alone being imperfect.
    pub fn new(base_url: impl Into<String>) -> Result<Self, SpxError> {
        let http = wreq::Client::builder()
            .emulation(wreq_util::Emulation::Chrome148)
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| SpxError::Transport)?;
        Ok(SpxClient { http, base_url: base_url.into() })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// POST a JSON body with SPX headers; return the parsed JSON on 2xx.
    async fn post_json(&self, cookies: &SpxCookies, path: &str, body: Value) -> Result<Value, SpxError> {
        let res = self
            .http
            .post(self.url(path))
            .headers(build_headers(cookies, &self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|_| SpxError::Transport)?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SpxError::Status(status));
        }
        res.json::<Value>().await.map_err(|_| SpxError::Body)
    }

    async fn get_json(&self, cookies: &SpxCookies, path_with_query: &str) -> Result<Value, SpxError> {
        let res = self
            .http
            .get(self.url(path_with_query))
            .headers(build_headers(cookies, &self.base_url))
            .send()
            .await
            .map_err(|_| SpxError::Transport)?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SpxError::Status(status));
        }
        res.json::<Value>().await.map_err(|_| SpxError::Body)
    }

    /// bidding/list — returns normalized bookings from `data.list`/`data.booking_list`.
    pub async fn fetch_bookings(&self, cookies: &SpxCookies, pageno: u32, count: u32) -> Result<Vec<SpxBooking>, SpxError> {
        let seven_days = (chrono::Utc::now().timestamp()) - 7 * 24 * 60 * 60;
        let body = json!({
            "pageno": pageno,
            "count": count,
            "request_tab_all": true,
            "request_ctime_start": seven_days,
        });
        let json = self.post_json(cookies, PATH_BIDDING_LIST, body).await?;
        Ok(extract_booking_list(&json).iter().map(normalize_booking).collect())
    }

    /// count_v2 — raw counts map (`data`).
    pub async fn fetch_booking_counts(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        let json = self.post_json(cookies, PATH_COUNT_V2, json!({ "request_tab_all": true })).await?;
        Ok(json.get("data").cloned().unwrap_or(Value::Null))
    }

    /// request/list — enrichment rows for one booking (`booking_id` MUST be numeric).
    pub async fn fetch_request_list(&self, cookies: &SpxCookies, booking_id: i64, count: u32) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_REQUEST_LIST, json!({ "booking_id": booking_id, "pageno": 1, "count": count })).await
    }

    /// accept — HTTP-status short-circuit (auth/transient) BEFORE body classification.
    pub async fn accept_booking(&self, cookies: &SpxCookies, booking_id: i64, agency_id: i64, request_ids: &[i64]) -> AcceptResult {
        if agency_id <= 0 {
            return AcceptResult { success: false, reason: AcceptReason::Auth, retcode: -1, message: "agency_id kosong".into() };
        }
        let mut body = json!({ "booking_id": booking_id, "agency_id": agency_id });
        if !request_ids.is_empty() {
            body["request_id_list"] = json!(request_ids);
        }
        let res = self
            .http
            .post(self.url(PATH_ACCEPT))
            .headers(build_headers(cookies, &self.base_url))
            .json(&body)
            .send()
            .await;
        let res = match res {
            Ok(r) => r,
            Err(_) => return AcceptResult { success: false, reason: AcceptReason::Transient, retcode: -1, message: "transport".into() },
        };
        let status = res.status().as_u16();
        if status == 401 || status == 403 {
            return AcceptResult { success: false, reason: AcceptReason::Auth, retcode: -1, message: format!("HTTP {status}") };
        }
        if status == 429 || status >= 500 {
            return AcceptResult { success: false, reason: AcceptReason::Transient, retcode: -1, message: format!("HTTP {status}") };
        }
        let body: Value = res.json().await.unwrap_or_else(|_| json!({}));
        let retcode = body.get("retcode").or_else(|| body.get("code")).and_then(Value::as_i64).unwrap_or(-1);
        let raw_msg = body.get("message").or_else(|| body.get("msg")).and_then(Value::as_str).unwrap_or("").to_string();
        let json_success = body.get("success").and_then(Value::as_bool).unwrap_or(false);
        classify_accept_response(retcode, json_success, &raw_msg)
    }

    /// notification pending count.
    pub async fn notification_count(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_NOTIFICATION, json!({ "use_case": "agency portal", "user_type": 4, "notification_type_list": [30] })).await
    }

    /// bidding log/list (GET) — the acceptor op-log.
    pub async fn fetch_bidding_log(&self, cookies: &SpxCookies, booking_id: i64) -> Result<Value, SpxError> {
        let path = format!("{PATH_BIDDING_LOG_LIST}?booking_id={booking_id}&pageno=1&count=30");
        self.get_json(cookies, &path).await
    }

    /// agency account user/list (requires request_source:1 + agency_id).
    pub async fn fetch_agency_users(&self, cookies: &SpxCookies, agency_id: i64) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_USER_LIST, json!({ "request_source": 1, "agency_id": agency_id, "pageno": 1, "count": 100 })).await
    }

    /// profile (GET) — primary of the reference's 6 fallbacks.
    pub async fn fetch_profile(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        self.get_json(cookies, PATH_PROFILE).await
    }

    /// booking_overview (POST) — fallback booking source.
    pub async fn fetch_booking_overview(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_BOOKING_OVERVIEW, json!({ "pageno": 1, "count": 100, "request_acceptance_status": 1, "request_tab_all": true })).await
    }

    /// booking_log (POST) — acceptor probe.
    pub async fn fetch_booking_log(&self, cookies: &SpxCookies, booking_id: i64) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_BOOKING_LOG, json!({ "booking_id": booking_id, "pageno": 1, "count": 20 })).await
    }
}

/// `data.list` else `data.booking_list` else `[]` (spx.ts fetchBookings).
fn extract_booking_list(json: &Value) -> Vec<Value> {
    let data = json.get("data").unwrap_or(json);
    if let Some(list) = data.get("list").and_then(Value::as_array) {
        return list.clone();
    }
    if let Some(list) = data.get("booking_list").and_then(Value::as_array) {
        return list.clone();
    }
    Vec::new()
}

#[cfg(test)]
mod extract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_prefers_list_then_booking_list() {
        let a = json!({ "data": { "list": [{ "booking_id": "1" }] } });
        assert_eq!(extract_booking_list(&a).len(), 1);
        let b = json!({ "data": { "booking_list": [{ "booking_id": "1" }, { "booking_id": "2" }] } });
        assert_eq!(extract_booking_list(&b).len(), 2);
        let c = json!({ "data": {} });
        assert_eq!(extract_booking_list(&c).len(), 0);
    }
}
