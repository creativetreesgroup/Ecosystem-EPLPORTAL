//! Argon2id password hashing (replaces the reference's bcrypt, per master spec).
//! Parameters are the OWASP-recommended argon2id defaults: m=19456 KiB, t=2, p=1.
use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};

use crate::crypto::envelope::CryptoError;

// OWASP argon2id recommendation (second option): 19 MiB memory, 2 iterations,
// 1 lane. Documented here so these are not "magic numbers".
const M_COST_KIB: u32 = 19_456;
const T_COST: u32 = 2;
const P_COST: u32 = 1;

fn argon2() -> Result<Argon2<'static>, CryptoError> {
    let params = Params::new(M_COST_KIB, T_COST, P_COST, None).map_err(|_| CryptoError::Aead)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hash `password` with a fresh random 16-byte salt. Returns the PHC string
/// (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`) for `portal_users.password_hash`.
pub fn hash_password(password: &str) -> Result<String, CryptoError> {
    let mut salt_bytes = [0u8; 16];
    getrandom::fill(&mut salt_bytes).map_err(|_| CryptoError::Rng)?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| CryptoError::Aead)?;
    let hash = argon2()?
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| CryptoError::Aead)?;
    Ok(hash.to_string())
}

/// Constant-time verify against a stored PHC hash. Returns `false` on any
/// parse/verify failure (never panics, never distinguishes "bad hash format"
/// from "wrong password" to the caller).
pub fn verify_password(password: &str, phc_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_verify_roundtrip() {
        let h = hash_password("correct horse battery staple").unwrap();
        assert!(h.starts_with("$argon2id$"), "PHC format, got: {h}");
        assert!(verify_password("correct horse battery staple", &h));
        assert!(!verify_password("wrong password", &h));
    }

    // DoD #7: salt is random per-hash, so two hashes of the same password differ.
    #[test]
    fn same_password_hashes_differ() {
        let a = hash_password("dupe").unwrap();
        let b = hash_password("dupe").unwrap();
        assert_ne!(a, b, "random salt must make each hash unique");
        assert!(verify_password("dupe", &a));
        assert!(verify_password("dupe", &b));
    }

    #[test]
    fn malformed_hash_verifies_false() {
        assert!(!verify_password("x", "not-a-phc-string"));
        assert!(!verify_password("x", ""));
    }
}
