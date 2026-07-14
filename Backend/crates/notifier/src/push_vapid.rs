// Backend/crates/notifier/src/push_vapid.rs
//! Web Push (VAPID) via web-push-native 0.4.0. Reads subscriptions from
//! `spx:push_subs:<acct>` (Redis SET of subscription JSON), builds an encrypted
//! aes128gcm + ES256-signed request per subscription, sends via wreq, prunes
//! expired (404/410) subscriptions. Fire-and-forget (errors log, never
//! propagate). See Global Constraints for the RUSTSEC-2023-0071 rationale.
use base64::Engine;
use jwt_simple::algorithms::ES256KeyPair;
use redis::AsyncCommands;
use serde::Deserialize;
use web_push_native::{Auth, WebPushBuilder};

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("vapid key: {0}")]
    Vapid(String),
    #[error("subscription: {0}")]
    Sub(String),
    #[error("build: {0}")]
    Build(String),
}

#[derive(Debug, Clone)]
pub struct VapidConfig {
    pub subject: String,     // e.g. "mailto:ops@example.com"
    pub public_key: String,  // base64url
    pub private_key: String, // base64url (32-byte P-256 scalar)
}

impl VapidConfig {
    pub fn from_env() -> Option<Self> {
        let subject = std::env::var("VAPID_SUBJECT").ok()?;
        let public_key = std::env::var("VAPID_PUBLIC").ok()?;
        let private_key = std::env::var("VAPID_PRIVATE").ok()?;
        if public_key.is_empty() || private_key.is_empty() {
            return None;
        }
        Some(Self {
            subject,
            public_key,
            private_key,
        })
    }

    fn keypair(&self) -> Result<ES256KeyPair, PushError> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(self.private_key.trim())
            .map_err(|e| PushError::Vapid(e.to_string()))?;
        ES256KeyPair::from_bytes(&raw).map_err(|e| PushError::Vapid(e.to_string()))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    #[serde(default)]
    pub p256dh: String,
    #[serde(default)]
    pub auth: String,
}

/// Some browsers store keys nested under `keys: {p256dh, auth}`; accept both.
#[derive(Deserialize)]
struct RawSub {
    endpoint: String,
    #[serde(default)]
    keys: Option<RawKeys>,
    #[serde(default)]
    p256dh: Option<String>,
    #[serde(default)]
    auth: Option<String>,
}
#[derive(Deserialize)]
struct RawKeys {
    #[serde(default)]
    p256dh: String,
    #[serde(default)]
    auth: String,
}

fn parse_sub(raw: &str) -> Option<PushSubscription> {
    let r: RawSub = serde_json::from_str(raw).ok()?;
    let (p256dh, auth) = match r.keys {
        Some(k) => (k.p256dh, k.auth),
        None => (r.p256dh.unwrap_or_default(), r.auth.unwrap_or_default()),
    };
    Some(PushSubscription {
        endpoint: r.endpoint,
        p256dh,
        auth,
    })
}

#[derive(Debug, Clone)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub tag: Option<String>,
}

impl PushPayload {
    fn to_json(&self) -> Vec<u8> {
        serde_json::json!({
            "title": self.title, "body": self.body,
            "url": self.url, "tag": self.tag,
        })
        .to_string()
        .into_bytes()
    }
}

/// Build the encrypted, VAPID-signed HTTP request for one subscription.
///
/// `web_push_native::WebPushBuilder::build` returns an `http::Request<Vec<u8>>`
/// with `Content-Encoding: aes128gcm`, `TTL`, `Content-Length`, and (via
/// `.with_vapid`) an `Authorization: vapid t=<jwt>, k=<pubkey>` header —
/// verified directly against the installed 0.4.0 source (`src/lib.rs`,
/// `src/vapid.rs`).
pub fn build_push_request(
    vapid: &VapidConfig,
    sub: &PushSubscription,
    payload: &PushPayload,
) -> Result<http::Request<Vec<u8>>, PushError> {
    let kp = vapid.keypair()?;
    let ua_public_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sub.p256dh.trim())
        .map_err(|e| PushError::Sub(e.to_string()))?;
    let ua_auth_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sub.auth.trim())
        .map_err(|e| PushError::Sub(e.to_string()))?;
    let ua_public = p256::PublicKey::from_sec1_bytes(&ua_public_bytes)
        .map_err(|e| PushError::Sub(e.to_string()))?;
    // `Auth = GenericArray<u8, U16>` (web-push-native's re-export). This pulls
    // in `generic-array` 0.14.x transitively via `aes-gcm`/`p256`'s own
    // dependency trees — that version is *itself* pinned by those upstream
    // crates, not by us, and unconditionally self-deprecates on rustc
    // >=1.65.0 (see its `build.rs`: `cargo:rustc-cfg=ga_is_deprecated` fires
    // regardless of correct usage, nudging consumers toward generic-array
    // 1.x). There is no non-deprecated way to construct a `GenericArray` on
    // this pinned 0.14 line; `#[allow]` scoped to this one call.
    #[allow(deprecated)]
    let ua_auth = Auth::clone_from_slice(&ua_auth_bytes); // 16-byte GenericArray
    let endpoint: http::Uri = sub
        .endpoint
        .parse()
        .map_err(|e| PushError::Sub(format!("{e}")))?;

    WebPushBuilder::new(endpoint, ua_public, ua_auth)
        .with_vapid(&kp, &vapid.subject)
        .build(payload.to_json())
        .map_err(|e| PushError::Build(format!("{e}")))
}

/// Send to every subscription for `account_id`. Prunes 404/410. Returns count
/// sent. Fire-and-forget: all errors log, none propagate.
pub async fn send_push_to_account(
    redis_url: &str,
    vapid: &VapidConfig,
    account_id: &str,
    payload: &PushPayload,
) -> usize {
    let key = format!("spx:push_subs:{}", account_id.to_lowercase());
    let client = match redis::Client::open(redis_url) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error=%e, "push: redis open");
            return 0;
        }
    };
    let mut con = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error=%e, "push: redis conn");
            return 0;
        }
    };
    let raws: Vec<String> = con.smembers(&key).await.unwrap_or_default();
    if raws.is_empty() {
        return 0;
    }
    let http = wreq::Client::builder().build().unwrap_or_default();
    let mut sent = 0;
    for raw in raws {
        let Some(sub) = parse_sub(&raw) else { continue };
        let req = match build_push_request(vapid, &sub, payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error=%e, "push: build");
                continue;
            }
        };
        // Replay the http::Request via wreq. `wreq::header::HeaderMap`/`Uri`
        // are literal re-exports of the `http` crate's types (verified: `pub
        // use http::header::*;` in wreq's src/header.rs, `pub use
        // http::{..., Uri, ...}` in src/lib.rs), and `cargo tree -i http`
        // shows a single unified `http` version across notifier/wreq/
        // web-push-native — so `parts.uri`/`parts.headers` pass straight
        // through with no re-encoding. `.body(Vec<u8>)` is covered by wreq's
        // `impl From<Vec<u8>> for Body`.
        let (parts, body) = req.into_parts();
        let rb = http.post(parts.uri).headers(parts.headers).body(body);
        match rb.send().await {
            Ok(r) if r.status().is_success() => sent += 1,
            Ok(r) if r.status().as_u16() == 404 || r.status().as_u16() == 410 => {
                let _: Result<i64, _> = con.srem(&key, &raw).await; // prune expired
            }
            Ok(r) => tracing::warn!(status = %r.status(), "push: non-2xx"),
            Err(e) => tracing::warn!(error=%e, "push: send"),
        }
    }
    sent
}
