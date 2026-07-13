// Backend/crates/executor/src/agency_dup.rs
//! agency_dup verification: when SPX reports "your agency already accepted",
//! probe the booking op-log to learn WHO really accepted — us (reclassify to ok)
//! or a rival agency (a real loss). Retry 0/500/1500ms because the op-log can lag
//! a beat after the race. NO "unverified" flag is stored anywhere — an
//! inconclusive probe is treated as "ours" by the caller (Fase 5).
use std::time::Duration;

use serde_json::Value;
use spx_client::{SpxClient, SpxCookies};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgencyDupOutcome {
    /// The acceptor is our own account — reclassify the accept back to "ok".
    Ours,
    /// A different agency won — Fase 5's notifier should alert on `rival_email`.
    LostToAgency { rival_email: String },
    /// 3 attempts, no `@`-bearing acceptor found. Treated as `Ours` by the caller
    /// (no state stored) — matches the reference's `return null`.
    Inconclusive,
}

/// Sleep-before-attempt delays (ms). Ported exactly from the reference; the sleep
/// is BEFORE attempts 2 and 3, not after.
const RETRY_DELAYS_MS: [u64; 3] = [0, 500, 1500];

/// Flexible numeric parse: JSON number or numeric string (reference `Number(x)`).
fn as_num(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    v.as_str().and_then(|s| s.trim().parse::<i64>().ok())
}

/// From a `log/list` response, the operator (email) of the ACCEPT op
/// (`booking_operation_type == 4`) that contains `@`, choosing the EARLIEST
/// `create_time` on ties. `None` if no such `@`-bearing acceptor exists.
pub(crate) fn earliest_accept_operator(log: &Value) -> Option<String> {
    // Reference: if retcode present and != 0, treat as no data.
    if let Some(rc) = log.get("retcode").and_then(as_num) {
        if rc != 0 {
            return None;
        }
    }
    let list = log.get("data").and_then(|d| d.get("list")).and_then(Value::as_array)?;
    let mut best: Option<(i64, String)> = None; // (create_time, operator)
    for op in list {
        if op.get("booking_operation_type").and_then(as_num) != Some(4) {
            continue;
        }
        let operator = op.get("operator").and_then(Value::as_str).unwrap_or("");
        if !operator.contains('@') {
            continue;
        }
        let ct = op.get("create_time").and_then(as_num).unwrap_or(0);
        match &best {
            Some((best_ct, _)) if *best_ct <= ct => {}
            _ => best = Some((ct, operator.to_string())),
        }
    }
    best.map(|(_, op)| op)
}

/// Extract the account's own email from a profile response, lowercased+trimmed.
pub fn extract_self_email(profile: &Value) -> Option<String> {
    let data = profile.get("data").unwrap_or(profile);
    for key in [
        "email",
        "user_email",
        "email_address",
        "account_email",
        "contact_email",
        "login_email",
    ] {
        if let Some(s) = data.get(key).and_then(Value::as_str) {
            let norm = s.trim().to_lowercase();
            if norm.contains('@') {
                return Some(norm);
            }
        }
    }
    None
}

/// Fetch the account's own email via `fetch_profile` (Fase 5 calls once, then
/// passes the result into `verify_agency_dup`). Returns `None` on any error or
/// if no email field is present.
pub async fn fetch_self_email(client: &SpxClient, cookies: &SpxCookies) -> Option<String> {
    let profile = client.fetch_profile(cookies).await.ok()?;
    extract_self_email(&profile)
}

/// Verify the real acceptor of `booking_id`. `self_email` MUST already be
/// lowercased+trimmed by the caller. Stops as soon as an `@`-bearing acceptor is
/// found; otherwise retries with the 0/500/1500ms schedule and finally returns
/// `Inconclusive`.
pub async fn verify_agency_dup(
    client: &SpxClient,
    cookies: &SpxCookies,
    self_email: &str,
    booking_id: i64,
) -> AgencyDupOutcome {
    for delay in RETRY_DELAYS_MS {
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        let log = match client.fetch_bidding_log(cookies, booking_id).await {
            Ok(v) => v,
            Err(_) => continue, // transient fetch error — try again
        };
        if let Some(operator) = earliest_accept_operator(&log) {
            let rival = operator.trim().to_lowercase();
            return if rival == self_email {
                AgencyDupOutcome::Ours
            } else {
                AgencyDupOutcome::LostToAgency { rival_email: rival }
            };
        }
        // No `@`-bearing acceptor this attempt — keep retrying.
    }
    AgencyDupOutcome::Inconclusive
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn earliest_accept_operator_picks_type4_at_earliest_create_time() {
        let log = json!({
            "retcode": 0,
            "data": { "list": [
                { "booking_operation_type": 4, "operator": "late@x.com",  "create_time": 200 },
                { "booking_operation_type": 4, "operator": "early@x.com", "create_time": 100 },
                { "booking_operation_type": 5, "operator": "reject@x.com","create_time": 50  },
                { "booking_operation_type": 4, "operator": "system",      "create_time": 10  }
            ]}
        });
        assert_eq!(earliest_accept_operator(&log).as_deref(), Some("early@x.com"));
    }

    #[test]
    fn earliest_accept_operator_none_when_no_at_operator() {
        let log = json!({ "data": { "list": [
            { "booking_operation_type": 4, "operator": "system", "create_time": 10 }
        ]}});
        assert_eq!(earliest_accept_operator(&log), None);
    }

    #[test]
    fn earliest_accept_operator_handles_string_numbers() {
        let log = json!({ "data": { "list": [
            { "booking_operation_type": "4", "operator": "a@x.com", "create_time": "300" },
            { "booking_operation_type": "4", "operator": "b@x.com", "create_time": "150" }
        ]}});
        assert_eq!(earliest_accept_operator(&log).as_deref(), Some("b@x.com"));
    }

    #[test]
    fn extract_self_email_falls_back_across_keys_and_normalizes() {
        let p = json!({ "data": { "login_email": "  Me@Example.COM " } });
        assert_eq!(extract_self_email(&p).as_deref(), Some("me@example.com"));
        assert_eq!(extract_self_email(&json!({ "data": {} })), None);
    }
}
