//! Signed, expiring, single-purpose token embedded in the WhatsApp "Terima cepat" link (mirrors
//! the reference's `lib/quicktoken.ts`). Lets an operator accept ONE specific booking straight
//! from the notification — no portal login. Forgery-proof (HMAC-SHA256 over a purpose-scoped
//! subkey derived from the master key via `LABEL_QUICK_ACCEPT_HMAC`), time-boxed, and — a
//! TOWER-specific hardening beyond the reference (which is single-tenant) — scoped to a single
//! `tenant_id` as well as a single `spx_id`, so a token can never be replayed against a
//! different tenant's booking even if the same `spx_id` string existed there.
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use super::envelope::{derive_subkey, LABEL_QUICK_ACCEPT_HMAC, MasterKey};
use super::CryptoError;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuickTokenPayload {
    /// SPX platform booking id (`bookings.spx_id`), not the internal UUID row id.
    b: String,
    t: Uuid,
    /// Unix millis expiry.
    e: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickTokenClaims {
    pub spx_id: String,
    pub tenant_id: Uuid,
}

fn hmac_key(master: &MasterKey) -> Result<HmacSha256, CryptoError> {
    use secrecy::ExposeSecret;
    let subkey = derive_subkey(master, LABEL_QUICK_ACCEPT_HMAC)?;
    HmacSha256::new_from_slice(subkey.expose_secret()).map_err(|_| CryptoError::Hkdf)
}

/// Default TTL matches the reference's own `signQuickToken` default (30 minutes).
pub const DEFAULT_TTL_MS: i64 = 30 * 60 * 1000;

pub fn sign_quick_token(
    master: &MasterKey,
    tenant_id: Uuid,
    spx_id: &str,
    ttl_ms: i64,
    now_ms: i64,
) -> Result<String, CryptoError> {
    let payload = QuickTokenPayload {
        b: spx_id.to_string(),
        t: tenant_id,
        e: now_ms + ttl_ms,
    };
    let payload_json = serde_json::to_vec(&payload).map_err(|_| CryptoError::Hkdf)?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload_json);

    let mut mac = hmac_key(master)?;
    mac.update(payload_b64.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig);

    Ok(format!("{payload_b64}.{sig_b64}"))
}

/// `now_ms` is caller-supplied (not read from the system clock inside this fn) so tests can
/// exercise expiry deterministically without sleeping.
pub fn verify_quick_token(
    master: &MasterKey,
    tenant_id: Uuid,
    token: &str,
    now_ms: i64,
) -> Option<QuickTokenClaims> {
    let (payload_b64, sig_b64) = token.split_once('.')?;
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return None;
    }
    let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;

    let mut mac = hmac_key(master).ok()?;
    mac.update(payload_b64.as_bytes());
    // `verify_slice` is `hmac`'s own constant-time comparison — no separate `subtle` dependency
    // needed, this crate already depends on the tool built for exactly this job.
    mac.verify_slice(&sig).ok()?;

    let payload_json = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload: QuickTokenPayload = serde_json::from_slice(&payload_json).ok()?;

    if payload.t != tenant_id {
        return None;
    }
    if now_ms > payload.e {
        return None;
    }
    Some(QuickTokenClaims {
        spx_id: payload.b,
        tenant_id: payload.t,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_master() -> MasterKey {
        MasterKey::from_bytes([9u8; 32])
    }

    #[test]
    fn valid_token_round_trips() {
        let m = test_master();
        let tenant_id = Uuid::new_v4();
        let token = sign_quick_token(&m, tenant_id, "SPX123", DEFAULT_TTL_MS, 1_000_000).unwrap();
        let claims = verify_quick_token(&m, tenant_id, &token, 1_000_500).expect("valid");
        assert_eq!(claims.spx_id, "SPX123");
        assert_eq!(claims.tenant_id, tenant_id);
    }

    #[test]
    fn expired_token_is_rejected() {
        let m = test_master();
        let tenant_id = Uuid::new_v4();
        let token = sign_quick_token(&m, tenant_id, "SPX123", DEFAULT_TTL_MS, 1_000_000).unwrap();
        let past_expiry = 1_000_000 + DEFAULT_TTL_MS + 1;
        assert!(verify_quick_token(&m, tenant_id, &token, past_expiry).is_none());
    }

    #[test]
    fn wrong_tenant_is_rejected() {
        let m = test_master();
        let tenant_id = Uuid::new_v4();
        let other_tenant = Uuid::new_v4();
        let token = sign_quick_token(&m, tenant_id, "SPX123", DEFAULT_TTL_MS, 1_000_000).unwrap();
        assert!(verify_quick_token(&m, other_tenant, &token, 1_000_500).is_none());
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let m = test_master();
        let tenant_id = Uuid::new_v4();
        let token = sign_quick_token(&m, tenant_id, "SPX123", DEFAULT_TTL_MS, 1_000_000).unwrap();
        let (payload, sig) = token.split_once('.').unwrap();
        // Tamper the payload's booking id without re-signing — a real forgery attempt.
        let tampered_payload = URL_SAFE_NO_PAD.encode(
            format!(r#"{{"b":"SPX999","t":"{tenant_id}","e":{}}}"#, 1_000_000 + DEFAULT_TTL_MS),
        );
        let tampered = format!("{tampered_payload}.{sig}");
        assert!(verify_quick_token(&m, tenant_id, &tampered, 1_000_500).is_none());
        // Sanity: the original, untampered token still verifies.
        assert!(verify_quick_token(&m, tenant_id, &format!("{payload}.{sig}"), 1_000_500).is_some());
    }

    #[test]
    fn malformed_tokens_are_rejected_not_panicking() {
        let m = test_master();
        let tenant_id = Uuid::new_v4();
        for bad in ["", "no-dot-at-all", ".", "abc.", ".xyz", "not-base64!!!.also-not-base64!!!"] {
            assert!(verify_quick_token(&m, tenant_id, bad, 1_000_000).is_none(), "input={bad:?}");
        }
    }

    #[test]
    fn different_master_key_rejects_the_token() {
        let m1 = test_master();
        let m2 = MasterKey::from_bytes([7u8; 32]);
        let tenant_id = Uuid::new_v4();
        let token = sign_quick_token(&m1, tenant_id, "SPX123", DEFAULT_TTL_MS, 1_000_000).unwrap();
        assert!(verify_quick_token(&m2, tenant_id, &token, 1_000_500).is_none());
    }
}
