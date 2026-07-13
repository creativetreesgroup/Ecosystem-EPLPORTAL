// Backend/crates/spx-client/src/login.rs
//! Tier 2 (API) + tier 3 (form) SPX login, ported from spx-auth.ts. Unlike the
//! data endpoints (which only SEND cookies), login must CAPTURE Set-Cookie
//! response headers and follow one redirect. Success == `fms_user_skey`
//! present. No browser — safe on the hot-path process. Tier 1 (browser) is a
//! separate process (`auth-sidecar`); this crate never touches chromiumoxide.
//!
//! wreq (pinned `=6.0.0-rc.29`) reconciliation notes (verified against the
//! vendored crate source, not guessed):
//! - `Response::cookies(&self) -> impl Iterator<Item = cookie::Cookie<'_>>`
//!   (feature `cookies`, already enabled by this crate) parses every
//!   `Set-Cookie` header for us — no manual `k=v; ...` splitting needed.
//!   `Cookie::name()`/`Cookie::value()` (from the `cookie` crate, 0.18) give
//!   `&str`. This borrows `&res`, so it MUST run before `res.json()` /
//!   `res.text()`, which consume `self`.
//! - `ClientBuilder`'s default `redirect_policy` is `redirect::Policy::none()`
//!   (see `wreq::client::ClientBuilder::default` /
//!   `Config { redirect_policy: redirect::Policy::none(), .. }`), and
//!   `SpxClient::new` never overrides it — so redirects are already NOT
//!   followed automatically; `form_login`'s manual `Location` follow below is
//!   real, not defensive-but-dead code.
//! - `RequestBuilder::form(&T)` (feature `form`, added to this crate's `wreq`
//!   features — additive, same pinned version, no dependency-graph change)
//!   URL-encodes a `&[(&str, &str)]` body and sets `Content-Type:
//!   application/x-www-form-urlencoded` via `entry().or_insert()` — a NO-OP
//!   if the header is already present. Since `build_headers` always sets
//!   `content-type: application/json` (the data-endpoint default),
//!   `form_login`'s POST must force the correct content-type back with an
//!   explicit `.header(CONTENT_TYPE, ...)` AFTER `.form()` (`.header()`
//!   always replaces).
use serde_json::Value;

use crate::client::SpxClient;
use crate::cookies::{build_headers, SpxCookies};

// The 11 known SPX cookie names → SpxCookies fields. `spx-admin-device-id`
// maps to `spx_admin_device_id`.
fn apply_set_cookie(jar: &mut SpxCookies, name: &str, value: &str) {
    match name {
        "fms_user_skey" => jar.fms_user_skey = value.to_string(),
        "fms_user_id" => jar.fms_user_id = value.to_string(),
        "fms_user_agency_id" => jar.fms_user_agency_id = value.to_string(),
        "csrftoken" => jar.csrftoken = value.to_string(),
        "spx_uk" => jar.spx_uk = value.to_string(),
        "spx_cid" => jar.spx_cid = value.to_string(),
        "spx_uid" => jar.spx_uid = value.to_string(),
        "spx_agid" => jar.spx_agid = value.to_string(),
        "spx_st" => jar.spx_st = value.to_string(),
        "ds" => jar.ds = value.to_string(),
        "spx-admin-device-id" => jar.spx_admin_device_id = value.to_string(),
        _ => {}
    }
}

/// Fold every `Set-Cookie` header on `res` into `jar`. Must be called before
/// consuming `res` (e.g. via `.json()`/`.text()`), since `Response::cookies`
/// only borrows `&self`.
fn merge_set_cookies(jar: &mut SpxCookies, res: &wreq::Response) {
    for cookie in res.cookies() {
        apply_set_cookie(jar, cookie.name(), cookie.value());
    }
}

/// Best-effort merge of a `retcode==0`/`success==true` JSON body's session
/// fields into `jar`, for SPX deployments that return the session in the body
/// instead of (or in addition to) Set-Cookie — mirrors the reference's
/// belt-and-suspenders extraction. Never overwrites a field already captured
/// from Set-Cookie.
fn merge_login_body(jar: &mut SpxCookies, body: &Value) {
    let retcode_ok = body.get("retcode").and_then(Value::as_i64) == Some(0);
    let success_ok = body.get("success").and_then(Value::as_bool) == Some(true);
    if !(retcode_ok || success_ok) {
        return;
    }
    let data = body.get("data").unwrap_or(body);
    if jar.fms_user_skey.is_empty() {
        if let Some(v) = string_field(data, "session_key") {
            jar.fms_user_skey = v;
        }
    }
    if jar.fms_user_id.is_empty() {
        if let Some(v) = string_field(data, "user_id") {
            jar.fms_user_id = v;
        }
    }
    if jar.fms_user_agency_id.is_empty() {
        if let Some(v) = string_field(data, "agency_id") {
            jar.fms_user_agency_id = v;
        }
    }
}

/// A JSON field that may be encoded as either a string or a number.
fn string_field(v: &Value, key: &str) -> Option<String> {
    let field = v.get(key)?;
    if let Some(s) = field.as_str() {
        return Some(s.to_string());
    }
    field.as_i64().map(|n| n.to_string())
}

impl SpxClient {
    /// Tier 2 — API login. Tries the reference's 5 endpoint/body variants; a
    /// captured `fms_user_skey` (from Set-Cookie or a retcode==0 body) wins.
    pub async fn api_login(&self, username: &str, password: &str) -> Option<SpxCookies> {
        let attempts: [(&str, Value); 5] = [
            (
                "/api/basicserver/agency/account/login",
                serde_json::json!({ "username": username, "password": password, "use_case": "agency portal" }),
            ),
            (
                "/api/basicserver/agency/account/login",
                serde_json::json!({ "username": username, "password": password }),
            ),
            (
                "/api/basicserver/account/login",
                serde_json::json!({ "username": username, "password": password }),
            ),
            (
                "/api/basicserver/agency/auth/login",
                serde_json::json!({ "username": username, "password": password }),
            ),
            (
                "/api/user/login",
                serde_json::json!({ "username": username, "password": password }),
            ),
        ];
        for (path, body) in attempts {
            if let Some(jar) = self.login_post_capture(path, &body).await {
                if !jar.fms_user_skey.is_empty() {
                    return Some(jar);
                }
            }
        }
        None
    }

    /// Tier 3 — form login: GET /login (CSRF), POST urlencoded form, follow the
    /// redirect to capture spx_* cookies.
    pub async fn form_login(&self, username: &str, password: &str) -> Option<SpxCookies> {
        let mut jar = SpxCookies::default();

        // 1. GET /login → capture Set-Cookie (csrftoken, if the portal sets one
        // up front).
        let get_res = self
            .http
            .get(self.url("/login"))
            .headers(build_headers(&jar, &self.base_url))
            .send()
            .await
            .ok()?;
        merge_set_cookies(&mut jar, &get_res);

        // 2. POST the login form, sending along whatever cookies step 1
        // captured (notably csrftoken). Redirects are NOT auto-followed (the
        // client's default `redirect::Policy::none()` — see module doc), so a
        // 3xx response comes back to us directly with its Set-Cookie/Location
        // headers intact.
        let form = [
            ("username", username),
            ("password", password),
            ("csrfmiddlewaretoken", jar.csrftoken.as_str()),
            ("next", "/"),
        ];
        let post_res = self
            .http
            .post(self.url("/login"))
            .headers(build_headers(&jar, &self.base_url))
            // `build_headers` unconditionally sets `content-type:
            // application/json` (the data-endpoint default); `.form()`'s own
            // content-type insertion is an `entry().or_insert()` (a no-op
            // when one is already present — see the `wreq` doc note at the
            // top of this file), so it would silently lose to the json
            // default above. Force the correct urlencoded content-type
            // explicitly AFTER `.form()` — `.header()` always replaces,
            // unlike `.form()`'s insert-if-absent.
            .form(&form)
            .header(wreq::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .send()
            .await
            .ok()?;
        merge_set_cookies(&mut jar, &post_res);
        let redirect_location = post_res
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // 3. Follow the ONE redirect manually (if present) — SPX's post-login
        // redirect target is what actually sets the spx_* session cookies.
        if let Some(location) = redirect_location {
            let redirect_url = if location.starts_with("http") {
                location
            } else {
                self.url(&location)
            };
            if let Ok(redirect_res) = self
                .http
                .get(redirect_url)
                .headers(build_headers(&jar, &self.base_url))
                .send()
                .await
            {
                merge_set_cookies(&mut jar, &redirect_res);
            }
        }

        if jar.fms_user_skey.is_empty() {
            None
        } else {
            Some(jar)
        }
    }

    /// Visit pages/count API that set spx_cid; fill it if empty. Port of
    /// fetchSpxCid. Best-effort: any transport failure on any candidate is
    /// silently skipped (spx_cid is an enrichment cookie, not a login gate —
    /// its absence must never fail an otherwise-successful login).
    pub async fn fetch_spx_cid(&self, cookies: &mut SpxCookies) {
        if !cookies.spx_cid.is_empty() {
            return;
        }
        for page in ["/line-haul/booking", "/line-haul", "/booking", "/"] {
            if let Ok(res) = self
                .http
                .get(self.url(page))
                .headers(build_headers(cookies, &self.base_url))
                .send()
                .await
            {
                merge_set_cookies(cookies, &res);
            }
            if !cookies.spx_cid.is_empty() {
                return;
            }
        }
        // Last resort: the count API (also sets spx_cid on some deployments).
        if let Ok(res) = self
            .http
            .post(self.url(crate::client::PATH_COUNT_V2))
            .headers(build_headers(cookies, &self.base_url))
            .json(&serde_json::json!({ "request_tab_all": true }))
            .send()
            .await
        {
            merge_set_cookies(cookies, &res);
        }
    }

    /// Shared helper: POST JSON to a login path and capture Set-Cookie into a
    /// fresh jar (also merges a retcode==0/success body's session fields, like
    /// the reference). Each attempt starts from an EMPTY jar — unlike
    /// `form_login`'s multi-step flow, the reference's API-login attempts are
    /// independent, unauthenticated POSTs.
    async fn login_post_capture(&self, path: &str, body: &Value) -> Option<SpxCookies> {
        let empty = SpxCookies::default();
        let res = self
            .http
            .post(self.url(path))
            .headers(build_headers(&empty, &self.base_url))
            .json(body)
            .send()
            .await
            .ok()?;
        let mut jar = SpxCookies::default();
        merge_set_cookies(&mut jar, &res);
        if let Ok(value) = res.json::<Value>().await {
            merge_login_body(&mut jar, &value);
        }
        Some(jar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_set_cookie_maps_all_11_known_names() {
        let mut jar = SpxCookies::default();
        apply_set_cookie(&mut jar, "fms_user_skey", "A");
        apply_set_cookie(&mut jar, "fms_user_id", "B");
        apply_set_cookie(&mut jar, "fms_user_agency_id", "C");
        apply_set_cookie(&mut jar, "csrftoken", "D");
        apply_set_cookie(&mut jar, "spx_uk", "E");
        apply_set_cookie(&mut jar, "spx_cid", "F");
        apply_set_cookie(&mut jar, "spx_uid", "G");
        apply_set_cookie(&mut jar, "spx_agid", "H");
        apply_set_cookie(&mut jar, "spx_st", "I");
        apply_set_cookie(&mut jar, "ds", "J");
        apply_set_cookie(&mut jar, "spx-admin-device-id", "K");
        assert_eq!(jar.fms_user_skey, "A");
        assert_eq!(jar.fms_user_id, "B");
        assert_eq!(jar.fms_user_agency_id, "C");
        assert_eq!(jar.csrftoken, "D");
        assert_eq!(jar.spx_uk, "E");
        assert_eq!(jar.spx_cid, "F");
        assert_eq!(jar.spx_uid, "G");
        assert_eq!(jar.spx_agid, "H");
        assert_eq!(jar.spx_st, "I");
        assert_eq!(jar.ds, "J");
        assert_eq!(jar.spx_admin_device_id, "K");
    }

    #[test]
    fn apply_set_cookie_ignores_unknown_names() {
        let mut jar = SpxCookies::default();
        apply_set_cookie(&mut jar, "some_other_cookie", "ignored");
        assert_eq!(jar.fms_user_skey, "");
        assert_eq!(jar.spx_cid, "");
    }

    #[test]
    fn merge_login_body_fills_only_empty_fields_from_retcode_zero() {
        let mut jar = SpxCookies {
            fms_user_skey: "ALREADY-FROM-COOKIE".into(),
            ..Default::default()
        };
        let body = serde_json::json!({
            "retcode": 0,
            "data": { "session_key": "IGNORED", "user_id": 7, "agency_id": "42" }
        });
        merge_login_body(&mut jar, &body);
        // fms_user_skey was already set (from Set-Cookie) — body must not
        // clobber it.
        assert_eq!(jar.fms_user_skey, "ALREADY-FROM-COOKIE");
        assert_eq!(jar.fms_user_id, "7");
        assert_eq!(jar.fms_user_agency_id, "42");
    }

    #[test]
    fn merge_login_body_noop_when_not_success() {
        let mut jar = SpxCookies::default();
        let body = serde_json::json!({ "retcode": -1, "data": { "session_key": "X" } });
        merge_login_body(&mut jar, &body);
        assert_eq!(jar.fms_user_skey, "");
    }
}
