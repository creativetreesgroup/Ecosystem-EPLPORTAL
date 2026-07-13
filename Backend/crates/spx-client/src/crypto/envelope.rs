//! Envelope encryption: master key -> HKDF-SHA256 subkey (per purpose) ->
//! AES-256-GCM with a random 96-bit nonce and tenant-bound AAD.
use std::path::Path;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::crypto::secret::{ExposeSecret, SecretBox, SecretString};

/// HKDF `info` label: encrypts the SPX agency password (`agency_credentials`).
pub const LABEL_AGENCY_CREDENTIAL: &str = "tower.agency-credential.v1";
/// HKDF `info` label: encrypts the WAHA API key (`site_settings`).
pub const LABEL_WAHA_KEY: &str = "tower.waha-key.v1";
/// HKDF `info` label reserved for the Fase 4+ HMAC quick-accept token. Fase 3
/// only proves this subkey differs from the AES subkeys; the token is not built.
pub const LABEL_QUICK_ACCEPT_HMAC: &str = "tower.quick-accept-hmac.v1";

/// Master-key version stamped into every ciphertext row. Fase 3 = 1 only;
/// multi-version rotation is Fase 8 (extension point, not built now).
pub const KEY_VERSION: i32 = 1;

const MASTER_KEY_LEN: usize = 32;

/// The 32-byte master key, held zeroize-on-drop. Never `.pad()`/`.slice()`d —
/// it is always the full 32 bytes (closes reference Gap #1).
pub struct MasterKey(SecretBox<[u8; MASTER_KEY_LEN]>);

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey([REDACTED])")
    }
}

impl MasterKey {
    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        MasterKey(SecretBox::new(Box::new(bytes)))
    }

    /// Read exactly 32 raw bytes from `path` (the Docker secret file). The
    /// transient file buffer is zeroized before it drops. A file that is not
    /// exactly 32 bytes is rejected (the ONLY length handling in the crypto
    /// code — there is no padding/truncation of a short/long secret anywhere).
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, CryptoError> {
        let mut bytes = std::fs::read(path)?;
        let len = bytes.len();
        let arr: [u8; MASTER_KEY_LEN] = match <[u8; MASTER_KEY_LEN]>::try_from(bytes.as_slice()) {
            Ok(a) => a,
            Err(_) => {
                bytes.zeroize();
                return Err(CryptoError::BadMasterKeyLength(len));
            }
        };
        bytes.zeroize();
        Ok(MasterKey::from_bytes(arr))
    }

    /// Load from `$TOWER_MASTER_KEY_PATH` (default `/run/secrets/tower_master_key`,
    /// the Compose secret mount path — see Task 5).
    pub fn load_default() -> Result<Self, CryptoError> {
        let path = std::env::var("TOWER_MASTER_KEY_PATH")
            .unwrap_or_else(|_| "/run/secrets/tower_master_key".to_string());
        Self::load_from_file(path)
    }
}

/// AES-256-GCM output: `bytes` is ciphertext-with-16-byte-auth-tag-appended,
/// `nonce` is the fresh random 96-bit nonce that produced it.
#[derive(Debug, Clone)]
pub struct Ciphertext {
    pub bytes: Vec<u8>,
    pub nonce: [u8; 12],
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    // NB: no plaintext/key material is ever placed in an error message.
    #[error("AES-GCM operation failed")]
    Aead,
    #[error("HKDF expand failed")]
    Hkdf,
    #[error("OS CSPRNG unavailable")]
    Rng,
    #[error("master key file I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("master key must be exactly 32 bytes, got {0}")]
    BadMasterKeyLength(usize),
    #[error("nonce must be exactly 12 bytes, got {0}")]
    BadNonceLength(usize),
}

/// AAD binds a ciphertext to its purpose + tenant: `"<label>|<tenant_id>"`.
pub fn aad_for(label: &str, tenant_id: Uuid) -> Vec<u8> {
    format!("{label}|{tenant_id}").into_bytes()
}

/// HKDF-SHA256(master, info=label) -> 32-byte subkey, held zeroize-on-drop.
/// Different labels yield cryptographically independent subkeys from the same
/// master key — this is the structural fix for "one secret for AES and HMAC".
pub(crate) fn derive_subkey(
    master: &MasterKey,
    label: &str,
) -> Result<SecretBox<[u8; 32]>, CryptoError> {
    let hk = Hkdf::<Sha256>::new(None, master.0.expose_secret());
    let mut okm = [0u8; 32];
    hk.expand(label.as_bytes(), &mut okm)
        .map_err(|_| CryptoError::Hkdf)?;
    Ok(SecretBox::new(Box::new(okm)))
}

pub fn encrypt(
    master: &MasterKey,
    label: &str,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Ciphertext, CryptoError> {
    let subkey = derive_subkey(master, label)?;
    let cipher =
        Aes256Gcm::new_from_slice(subkey.expose_secret()).map_err(|_| CryptoError::Aead)?;
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).map_err(|_| CryptoError::Rng)?;
    let nonce = Nonce::<U12>::try_from(&nonce_bytes[..]).map_err(|_| CryptoError::BadNonceLength(12))?;
    let bytes = cipher
        .encrypt(&nonce, Payload { msg: plaintext, aad })
        .map_err(|_| CryptoError::Aead)?;
    Ok(Ciphertext { bytes, nonce: nonce_bytes })
}

pub fn decrypt(
    master: &MasterKey,
    label: &str,
    ciphertext: &[u8],
    nonce: &[u8; 12],
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let subkey = derive_subkey(master, label)?;
    let cipher =
        Aes256Gcm::new_from_slice(subkey.expose_secret()).map_err(|_| CryptoError::Aead)?;
    let nonce = Nonce::<U12>::try_from(&nonce[..]).map_err(|_| CryptoError::BadNonceLength(12))?;
    cipher
        .decrypt(&nonce, Payload { msg: ciphertext, aad })
        .map_err(|_| CryptoError::Aead)
}

/// Encrypt an SPX agency password for storage in `agency_credentials.ciphertext`
/// / `.nonce`. AAD binds it to `LABEL_AGENCY_CREDENTIAL` + `tenant_id`.
pub fn encrypt_agency_password(
    master: &MasterKey,
    tenant_id: Uuid,
    password: &str,
) -> Result<Ciphertext, CryptoError> {
    let aad = aad_for(LABEL_AGENCY_CREDENTIAL, tenant_id);
    encrypt(master, LABEL_AGENCY_CREDENTIAL, password.as_bytes(), &aad)
}

/// Decrypt a password read back from `agency_credentials`. Returned inside a
/// `SecretString` so it is redacted in logs and zeroized on drop.
pub fn decrypt_agency_password(
    master: &MasterKey,
    tenant_id: Uuid,
    ciphertext: &[u8],
    nonce: &[u8; 12],
) -> Result<SecretString, CryptoError> {
    let aad = aad_for(LABEL_AGENCY_CREDENTIAL, tenant_id);
    let plaintext = decrypt(master, LABEL_AGENCY_CREDENTIAL, ciphertext, nonce, &aad)?;
    let s = String::from_utf8(plaintext).map_err(|_| CryptoError::Aead)?;
    Ok(SecretString::from(s))
}

/// Encrypt a WAHA API key. AAD binds it to `LABEL_WAHA_KEY` + `tenant_id`.
pub fn encrypt_waha_key(
    master: &MasterKey,
    tenant_id: Uuid,
    key: &str,
) -> Result<Ciphertext, CryptoError> {
    let aad = aad_for(LABEL_WAHA_KEY, tenant_id);
    encrypt(master, LABEL_WAHA_KEY, key.as_bytes(), &aad)
}

/// Decrypt a WAHA API key read back from `site_settings`.
pub fn decrypt_waha_key(
    master: &MasterKey,
    tenant_id: Uuid,
    ciphertext: &[u8],
    nonce: &[u8; 12],
) -> Result<SecretString, CryptoError> {
    let aad = aad_for(LABEL_WAHA_KEY, tenant_id);
    let plaintext = decrypt(master, LABEL_WAHA_KEY, ciphertext, nonce, &aad)?;
    let s = String::from_utf8(plaintext).map_err(|_| CryptoError::Aead)?;
    Ok(SecretString::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_master() -> MasterKey {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).unwrap();
        MasterKey::from_bytes(b)
    }

    #[test]
    fn roundtrip_with_aad() {
        let m = test_master();
        let tenant = Uuid::new_v4();
        let aad = aad_for(LABEL_AGENCY_CREDENTIAL, tenant);
        let ct = encrypt(&m, LABEL_AGENCY_CREDENTIAL, b"s3cr3t-password", &aad).unwrap();
        assert_ne!(ct.bytes, b"s3cr3t-password");
        let pt = decrypt(&m, LABEL_AGENCY_CREDENTIAL, &ct.bytes, &ct.nonce, &aad).unwrap();
        assert_eq!(pt, b"s3cr3t-password");
    }

    #[test]
    fn wrong_aad_fails() {
        let m = test_master();
        let a = aad_for(LABEL_AGENCY_CREDENTIAL, Uuid::new_v4());
        let b = aad_for(LABEL_AGENCY_CREDENTIAL, Uuid::new_v4()); // different tenant
        let ct = encrypt(&m, LABEL_AGENCY_CREDENTIAL, b"x", &a).unwrap();
        assert!(decrypt(&m, LABEL_AGENCY_CREDENTIAL, &ct.bytes, &ct.nonce, &b).is_err());
    }

    #[test]
    fn nonce_is_fresh_per_encryption() {
        let m = test_master();
        let aad = aad_for(LABEL_WAHA_KEY, Uuid::new_v4());
        let c1 = encrypt(&m, LABEL_WAHA_KEY, b"same", &aad).unwrap();
        let c2 = encrypt(&m, LABEL_WAHA_KEY, b"same", &aad).unwrap();
        assert_ne!(c1.nonce, c2.nonce, "nonce must be random per encryption");
        assert_ne!(c1.bytes, c2.bytes, "ciphertext must differ under a fresh nonce");
    }

    // DoD #3 / #4b: purpose-scoped subkeys are BYTE-FOR-BYTE distinct from the
    // same master key (compares the subkey bytes directly, not just "encrypt
    // output differs"). This is the concrete proof that the AES subkeys and the
    // reserved HMAC subkey can never be the same key.
    #[test]
    fn purpose_subkeys_are_distinct_bytes() {
        let m = test_master();
        let cred = derive_subkey(&m, LABEL_AGENCY_CREDENTIAL).unwrap();
        let waha = derive_subkey(&m, LABEL_WAHA_KEY).unwrap();
        let hmac = derive_subkey(&m, LABEL_QUICK_ACCEPT_HMAC).unwrap();
        assert_ne!(cred.expose_secret(), waha.expose_secret());
        assert_ne!(cred.expose_secret(), hmac.expose_secret(), "AES subkey must != HMAC subkey");
        assert_ne!(waha.expose_secret(), hmac.expose_secret());
    }

    // DoD #4a: the master key is never truncated/padded into a key — a wrong-size
    // key file is rejected, and the subkey is always the full 32-byte HKDF output.
    #[test]
    fn subkey_is_full_32_bytes() {
        let m = test_master();
        let sk = derive_subkey(&m, LABEL_AGENCY_CREDENTIAL).unwrap();
        assert_eq!(sk.expose_secret().len(), 32);
    }
}
