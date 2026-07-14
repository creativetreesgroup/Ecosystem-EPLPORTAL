// Backend/bin/auth-sidecar/src/browser.rs
//! Tier-1 SPX browser login via chromiumoxide (0.9.1). Runs in THIS process
//! (separate from reactor-core) so a Chromium crash cannot touch the hot path.
//! Every failure returns Err (never panics) so the sidecar stays up.
use std::collections::HashMap;
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::error::CdpError;
use futures::StreamExt;

pub struct BrowserLoginCfg {
    pub spx_base_url: String,
    pub chrome_bin: String,
}

impl BrowserLoginCfg {
    pub fn from_env() -> Self {
        Self {
            spx_base_url: std::env::var("SPX_BASE_URL")
                .unwrap_or_else(|_| "https://logistics.myagencyservice.id".to_string()),
            chrome_bin: std::env::var("CHROME_BIN").unwrap_or_else(|_| "/usr/bin/chromium".to_string()),
        }
    }
}

/// Launch headless Chromium, log into SPX, return all cookies as name→value.
pub async fn browser_login(
    cfg: &BrowserLoginCfg,
    username: &str,
    password: &str,
) -> Result<HashMap<String, String>, String> {
    let config = BrowserConfig::builder()
        .chrome_executable(&cfg.chrome_bin)
        .no_sandbox()
        .arg("--disable-dev-shm-usage")
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .request_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("browser config: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("launch: {e}"))?;
    // MUST drive the handler or nothing progresses; keep the task alive for the
    // whole login and abort it at the end.
    let handler_task = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if h.is_err() {
                break;
            }
        }
    });

    let result = do_login(&browser, cfg, username, password).await;

    let _ = browser.close().await;
    handler_task.abort();
    result
}

async fn do_login(
    browser: &Browser,
    cfg: &BrowserLoginCfg,
    username: &str,
    password: &str,
) -> Result<HashMap<String, String>, String> {
    let login_url = format!("{}/login", cfg.spx_base_url.trim_end_matches('/'));
    let booking_url = format!("{}/line-haul/booking", cfg.spx_base_url.trim_end_matches('/'));

    let page = browser.new_page(&login_url).await.map_err(cdp)?;
    // Wait for the SSO password field to appear (form-ready signal).
    page.find_element("input[type=\"password\"]").await.map_err(cdp)?;

    // Fill email (SSO uses bare inputs; try email → text fallbacks in one selector).
    let email_sel = "input[type=\"email\"], input[name=\"email\"], input[name=\"username\"], input[type=\"text\"]";
    page.find_element(email_sel).await.map_err(cdp)?.click().await.map_err(cdp)?.type_str(username).await.map_err(cdp)?;
    // Password + submit via Enter (the React SSO form submits on Enter).
    page.find_element("input[type=\"password\"]").await.map_err(cdp)?
        .click().await.map_err(cdp)?
        .type_str(password).await.map_err(cdp)?
        .press_key("Enter").await.map_err(cdp)?;

    // Poll up to ~20s for the auth cookie.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(cookies) = page.get_cookies().await {
            if cookies.iter().any(|c| c.name == "fms_user_skey" && !c.value.is_empty()) {
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("login timeout: fms_user_skey not set".into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // Visit the booking page so SPX sets spx_cid.
    let _ = page.goto(&booking_url).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let cookies = page.get_cookies().await.map_err(cdp)?;
    let mut out = HashMap::new();
    for c in cookies {
        out.insert(c.name.clone(), c.value.clone());
    }
    if !out.contains_key("fms_user_skey") {
        return Err("no fms_user_skey after login".into());
    }
    Ok(out)
}

fn cdp(e: CdpError) -> String {
    format!("cdp: {e}")
}
