// Backend/crates/spx-client/src/crypto/session_token.rs
//! Opaque 256-bit session tokens. The plaintext token is sent to the client
//! ONCE (Set-Cookie) and never stored; only its SHA-256 hash is persisted to
//! `portal_sessions.token_hash`. Session lookup hashes the incoming cookie and
//! matches on the unique index — a DB dump cannot be replayed as a session.
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::crypto::envelope::CryptoError;
use crate::crypto::secret::SecretString;

/// SHA-256 of the token string (what goes into `portal_sessions.token_hash`).
pub fn hash_session_token(token: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().into()
}

/// Generate a fresh 256-bit token. Returns `(plaintext_token, sha256_hash)`:
/// send the plaintext as the cookie exactly once; persist only the hash.
pub fn generate_session_token() -> Result<(SecretString, [u8; 32]), CryptoError> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|_| CryptoError::Rng)?;
    let token = URL_SAFE_NO_PAD.encode(raw);
    let hash = hash_session_token(&token);
    Ok((SecretString::from(token), hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::secret::ExposeSecret;

    #[test]
    fn hash_is_deterministic_and_32_bytes() {
        let h1 = hash_session_token("abc");
        let h2 = hash_session_token("abc");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    #[test]
    fn tokens_are_unique_and_hash_matches() {
        let (t1, h1) = generate_session_token().unwrap();
        let (t2, _h2) = generate_session_token().unwrap();
        assert_ne!(
            t1.expose_secret(),
            t2.expose_secret(),
            "256-bit tokens must be unique"
        );
        assert_eq!(hash_session_token(t1.expose_secret()), h1);
    }

    // DoD #8 sanity check: the stored hash is NOT the token, and it is a fixed
    // 32 bytes — storing the raw token string as `token_hash` would be the wrong
    // length/shape. `portal_sessions.token_hash` is BYTEA (Vec<u8>); persist
    // `hash.to_vec()`, never the token.
    #[test]
    fn stored_hash_differs_from_plaintext_token() {
        let (token, hash) = generate_session_token().unwrap();
        let token_bytes = token.expose_secret().as_bytes();
        assert_ne!(token_bytes, &hash[..], "must store the hash, not the token");
        // base64url of 32 bytes is 43 chars; the hash is 32 bytes — different length.
        assert_ne!(token_bytes.len(), hash.len());
    }
}
