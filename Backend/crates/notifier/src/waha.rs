//! WAHA + n8n HTTP senders — pure fire-and-forget. A failure logs and
//! returns; it NEVER errors (the caller `tokio::spawn`s these and must be
//! unaffected — a notify hiccup can never fail an accept that already
//! succeeded). Ported from spx-portal-ref webhook.ts's `sendToWaha` /
//! `sendToWahaMany` / `parseChatIds` (best-effort direct WAHA calls) and the
//! inline n8n `fetch(...).catch(() => {})` pattern.
use serde_json::json;

use crate::BotSettings;

/// Parse a multi-target field into WAHA chatIds. Reference `parseChatIds`:
/// split on whitespace/comma/semicolon runs, "...@g.us"/"...@c.us" kept
/// as-is, bare digits -> "<num>@c.us", entries of length <= 4 dropped.
pub fn parse_chat_ids(raw: &str) -> Vec<String> {
    raw.split([' ', ',', ';', '\n', '\t', '\r'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|t| {
            if t.contains('@') {
                t.to_string()
            } else {
                format!("{}@c.us", t.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
            }
        })
        .filter(|t| t.len() > 4)
        .collect()
}

fn client() -> wreq::Client {
    // Plain client (internal WAHA/n8n; no impersonation needed). Default
    // build; `unwrap_or_default()` keeps this infallible — `wreq::Client`
    // implements `Default` (verified against the pinned =6.0.0-rc.29 source),
    // matching the same fallback pattern already used in
    // `crates/poller/src/login.rs`.
    wreq::Client::builder().build().unwrap_or_default()
}

/// POST one message to every chatId in `group`. Best-effort; returns (sent, total).
/// Reference `sendToWahaMany` + `sendToWaha`: `POST {wahaUrl}/api/sendText`,
/// header `X-Api-Key`, body `{ session, chatId, text }`.
pub async fn send_to_waha_many(s: &BotSettings, group: &str, text: &str) -> (usize, usize) {
    let targets = parse_chat_ids(group);
    if targets.is_empty() || s.waha_url.is_empty() || s.waha_api_key.is_empty() {
        return (0, targets.len());
    }
    let http = client();
    let url = format!("{}/api/sendText", s.waha_url.trim_end_matches('/'));
    let session = if s.waha_session.is_empty() { "default" } else { &s.waha_session };
    let mut sent = 0;
    for chat_id in &targets {
        let body = json!({ "session": session, "chatId": chat_id, "text": text });
        match http.post(&url).header("X-Api-Key", &s.waha_api_key).json(&body).send().await {
            Ok(r) if r.status().is_success() => sent += 1,
            Ok(r) => tracing::warn!(status = %r.status(), chat = %chat_id, "WAHA sendText non-2xx"),
            Err(e) => tracing::warn!(error = %e, chat = %chat_id, "WAHA sendText failed"),
        }
    }
    (sent, targets.len())
}

/// POST an n8n webhook (best-effort). Reference: inline `fetch(...).catch(() => {})`.
pub async fn send_n8n(s: &BotSettings, payload: serde_json::Value) {
    if s.webhook_url.is_empty() {
        return;
    }
    let http = client();
    if let Err(e) = http.post(&s.webhook_url).json(&payload).send().await {
        tracing::warn!(error = %e, "n8n webhook failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_group_ids_as_is_and_bare_numbers_as_c_us() {
        let ids = parse_chat_ids("12036@g.us, 6281234567890 ; 999");
        assert_eq!(
            ids,
            vec!["12036@g.us".to_string(), "6281234567890@c.us".to_string(), "999@c.us".to_string()]
        );
    }

    #[test]
    fn drops_entries_too_short_after_suffixing() {
        // A token already containing '@' but <= 4 chars total is dropped
        // (reference: `.filter(t => t.length > 4)`); "a@bc" is exactly 4.
        let ids = parse_chat_ids("a@bc, 12036@g.us");
        assert_eq!(ids, vec!["12036@g.us".to_string()]);
    }

    #[test]
    fn drops_empty_and_whitespace_only_entries() {
        let ids = parse_chat_ids("  \n\t 12036@g.us \n ");
        assert_eq!(ids, vec!["12036@g.us".to_string()]);
    }
}
