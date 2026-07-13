//! WAHA bot settings persisted as one row in Fase 2's generic `site_settings`
//! table (`key = 'waha_settings'`, `value jsonb`). Closes reference Gap #3 (the
//! WAHA API key was stored plaintext in Redis). Only the API key is encrypted;
//! the non-sensitive base URL + session name stay plaintext in the same JSONB.
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::envelope::{
    decrypt_waha_key, encrypt_waha_key, CryptoError, MasterKey, KEY_VERSION,
};
use crate::crypto::secret::SecretString;

/// The `site_settings.value` JSONB shape for `key = 'waha_settings'`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WahaSettings {
    /// Non-sensitive: WAHA instance base URL (plaintext).
    #[serde(default)]
    pub waha_url: String,
    /// Non-sensitive: WAHA session name (plaintext, default "default").
    #[serde(default)]
    pub waha_session: String,
    /// base64(STANDARD) of the AES-256-GCM ciphertext of the API key.
    pub api_key_ciphertext_b64: String,
    /// base64(STANDARD) of the 12-byte nonce.
    pub api_key_nonce_b64: String,
    /// Master-key version used to encrypt the API key.
    pub key_version: i32,
}

pub const SITE_SETTINGS_KEY: &str = "waha_settings";

impl WahaSettings {
    /// Build the settings row, encrypting `api_key` for `tenant_id`.
    pub fn encrypt_new(
        master: &MasterKey,
        tenant_id: Uuid,
        waha_url: &str,
        waha_session: &str,
        api_key: &str,
    ) -> Result<Self, CryptoError> {
        let ct = encrypt_waha_key(master, tenant_id, api_key)?;
        Ok(WahaSettings {
            waha_url: waha_url.to_string(),
            waha_session: waha_session.to_string(),
            api_key_ciphertext_b64: STANDARD.encode(&ct.bytes),
            api_key_nonce_b64: STANDARD.encode(ct.nonce),
            key_version: KEY_VERSION,
        })
    }

    /// Decrypt the API key back out (returned as a redacted `SecretString`).
    pub fn decrypt_api_key(
        &self,
        master: &MasterKey,
        tenant_id: Uuid,
    ) -> Result<SecretString, CryptoError> {
        let ciphertext = STANDARD
            .decode(&self.api_key_ciphertext_b64)
            .map_err(|_| CryptoError::Aead)?;
        let nonce_vec = STANDARD
            .decode(&self.api_key_nonce_b64)
            .map_err(|_| CryptoError::Aead)?;
        let nonce: [u8; 12] = nonce_vec
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::BadNonceLength(nonce_vec.len()))?;
        decrypt_waha_key(master, tenant_id, &ciphertext, &nonce)
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("WahaSettings serializes")
    }

    pub fn from_json_value(v: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> MasterKey {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).unwrap();
        MasterKey::from_bytes(b)
    }

    #[test]
    fn roundtrip_in_memory_and_plaintext_never_in_json() {
        let m = master();
        let tenant = Uuid::new_v4();
        let api_key = "waha-plaintext-key-XYZ";
        let s = WahaSettings::encrypt_new(&m, tenant, "http://waha:3000", "default", api_key).unwrap();

        // The serialized JSONB must not contain the plaintext key substring.
        let json = s.to_json_value().to_string();
        assert!(!json.contains(api_key), "plaintext WAHA key leaked into JSONB: {json}");

        use crate::crypto::secret::ExposeSecret;
        let back = WahaSettings::from_json_value(&s.to_json_value()).unwrap();
        assert_eq!(back.decrypt_api_key(&m, tenant).unwrap().expose_secret(), api_key);
    }
}
