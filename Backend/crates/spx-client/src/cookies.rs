//! SPX cookie jar + request headers. SPX auth is cookie-based (not bearer). The
//! header set mirrors the reference (spx.ts:52-76); client-hints are pinned to
//! the Chrome version actually used by the wreq emulation preset (Chrome 148 —
//! matching the reference's Chrome 148 exactly; the resolved `wreq-util`
//! `3.0.0-rc.14` line supports presets through `Chrome149`, so no compromise
//! versus the reference was needed here. Keep in lockstep with the
//! `wreq-util::Emulation::ChromeNNN` preset wired up in the client, Task 9).
use wreq::header::{HeaderMap, HeaderName, HeaderValue};

/// Chrome version whose UA + client-hints we emit. Keep in lockstep with the
/// wreq-util `Emulation::ChromeNNN` preset chosen in the client (Task 9).
pub const CHROME_MAJOR: u32 = 148;

fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/{CHROME_MAJOR}.0.0.0 Safari/537.36"
    )
}

fn sec_ch_ua() -> String {
    format!(
        "\"Chromium\";v=\"{CHROME_MAJOR}\",\"Google Chrome\";v=\"{CHROME_MAJOR}\",\"Not/A)Brand\";v=\"99\""
    )
}

/// 11-field SPX cookie set (spx.ts / session.ts EMPTY_COOKIES).
#[derive(Debug, Clone, Default)]
pub struct SpxCookies {
    pub fms_user_skey: String,
    pub fms_user_id: String,
    pub fms_user_agency_id: String,
    pub csrftoken: String,
    pub spx_uk: String,
    pub spx_cid: String,
    pub spx_uid: String,
    pub spx_agid: String,
    pub spx_st: String,
    pub ds: String,
    pub spx_admin_device_id: String, // cookie name: "spx-admin-device-id"
}

impl SpxCookies {
    fn pairs(&self) -> [(&'static str, &str); 11] {
        [
            ("fms_user_skey", &self.fms_user_skey),
            ("fms_user_id", &self.fms_user_id),
            ("fms_user_agency_id", &self.fms_user_agency_id),
            ("csrftoken", &self.csrftoken),
            ("spx_uk", &self.spx_uk),
            ("spx_cid", &self.spx_cid),
            ("spx_uid", &self.spx_uid),
            ("spx_agid", &self.spx_agid),
            ("spx_st", &self.spx_st),
            ("ds", &self.ds),
            ("spx-admin-device-id", &self.spx_admin_device_id),
        ]
    }
}

/// `Cookie:` header value — only non-empty pairs, `k=v` joined by `; `
/// (mirrors buildCookieString in session.ts).
pub fn build_cookie_string(c: &SpxCookies) -> String {
    c.pairs()
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Full request header map (spx.ts buildHeaders). `base_url` is the SPX origin
/// (e.g. "https://logistics.myagencyservice.id"). Adds `X-CSRFToken` and
/// `device-id` only when present (required for line-haul bidding endpoints).
pub fn build_headers(c: &SpxCookies, base_url: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    let set = |h: &mut HeaderMap, name: &'static str, val: String| {
        if let Ok(v) = HeaderValue::from_str(&val) {
            h.insert(HeaderName::from_static(name), v);
        }
    };
    set(&mut h, "accept", "application/json, text/plain, */*".to_string());
    set(&mut h, "accept-language", "en-US,en;q=0.9".to_string());
    set(&mut h, "cache-control", "no-cache".to_string());
    set(&mut h, "content-type", "application/json".to_string());
    set(&mut h, "cookie", build_cookie_string(c));
    set(&mut h, "user-agent", user_agent());
    set(&mut h, "referer", format!("{base_url}/"));
    set(&mut h, "origin", base_url.to_string());
    set(&mut h, "from-host", "logistics.myagencyservice.id".to_string());
    set(&mut h, "connection", "keep-alive".to_string());
    set(&mut h, "sec-ch-ua", sec_ch_ua());
    set(&mut h, "sec-ch-ua-mobile", "?0".to_string());
    set(&mut h, "sec-ch-ua-platform", "\"macOS\"".to_string());
    set(&mut h, "sec-fetch-dest", "empty".to_string());
    set(&mut h, "sec-fetch-mode", "cors".to_string());
    set(&mut h, "sec-fetch-site", "same-origin".to_string());
    if !c.csrftoken.is_empty() {
        set(&mut h, "x-csrftoken", c.csrftoken.clone());
    }
    if !c.spx_admin_device_id.is_empty() {
        set(&mut h, "device-id", c.spx_admin_device_id.clone());
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SpxCookies {
        SpxCookies {
            fms_user_skey: "SKEY".into(),
            fms_user_agency_id: "42".into(),
            csrftoken: "CSRF".into(),
            spx_admin_device_id: "DEV-1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn cookie_string_skips_empty_and_joins() {
        let s = build_cookie_string(&sample());
        assert!(s.contains("fms_user_skey=SKEY"));
        assert!(s.contains("csrftoken=CSRF"));
        assert!(s.contains("spx-admin-device-id=DEV-1"));
        assert!(!s.contains("spx_cid="), "empty cookies must be omitted");
        assert!(s.contains("; "), "pairs joined by '; '");
    }

    #[test]
    fn headers_include_csrf_and_device_id_when_present() {
        let h = build_headers(&sample(), "https://logistics.myagencyservice.id");
        assert_eq!(h.get("x-csrftoken").unwrap(), "CSRF");
        assert_eq!(h.get("device-id").unwrap(), "DEV-1");
        assert_eq!(h.get("origin").unwrap(), "https://logistics.myagencyservice.id");
        assert!(h
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Chrome/148"));
        assert!(h.get("cookie").unwrap().to_str().unwrap().contains("fms_user_skey=SKEY"));
    }

    #[test]
    fn csrf_and_device_omitted_when_empty() {
        let h = build_headers(&SpxCookies::default(), "https://x");
        assert!(h.get("x-csrftoken").is_none());
        assert!(h.get("device-id").is_none());
    }
}
