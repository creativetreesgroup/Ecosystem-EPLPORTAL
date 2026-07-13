# Fase 3 — spx-client + security kripto Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `spx-client` crate — a Chrome-impersonating SPX HTTP client (`SpxBooking` + `normalize_booking`, retcode `classify_accept_response`) plus a `crypto` module (envelope encryption master-key → HKDF → AES-256-GCM, argon2id passwords, SHA-256 session tokens) that closes the three confirmed reference security gaps and encrypts SPX credentials + the WAHA key into Fase 2's existing tables.

**Architecture:** Crypto lives as `spx_client::crypto::{secret, envelope, password, session_token}` (public modules, **not** a 9th workspace crate — the master spec's "ikuti persis" 8-crate/2-bin architecture is followed literally; the trade-off is accepted per the design doc). A Docker-secret file (0400, `/run/secrets/tower_master_key`, 32 random bytes) is loaded once into a `SecretBox<[u8;32]>`; per-purpose HKDF-SHA256 subkeys (distinct `info` labels) feed AES-256-GCM with a random 96-bit nonce and tenant-bound AAD. `SpxBooking` (29 fields) mirrors the reference and maps down to the untouched Fase 1 `core_domain::Booking` via `to_core_booking`. Encrypted credentials write into Fase 2's `agency_credentials(ciphertext,nonce,key_version)`; the WAHA key writes into `site_settings(value jsonb)` — no new migration.

**Tech Stack (all versions confirmed real via `cargo add --dry-run` + docs.rs on 2026-07-13, and the crypto stack was compile+run verified in a scratch crate — see Global Constraints):**
- HTTP client: **`wreq` 5.3.0** (Apache-2.0) — the maintained successor to the design doc's `rquest`, which is **no longer published** (renamed). Browser-impersonation presets: **`wreq-util`** (see the license caveat in Global Constraints — this is the one dependency that needs a deliberate version choice).
- Crypto: `aes-gcm` 0.11.0, `hkdf` 0.13.0, `sha2` 0.11.0, `argon2` 0.5.3, `secrecy` 0.10.3, `zeroize` 1.9.0, `getrandom` 0.4.3, `base64` 0.22.1, `thiserror` 2.0.18.
- Booking/accept: `serde_json` 1, `chrono` 0.4, `regex` 1, `core-domain` (workspace path dep).
- Integration/tests: `uuid` 1, `store` (path, dev-dep), `sqlx` (dev-dep), `tokio` (dev-dep), `wiremock` 0.6.5 (dev-dep, MIT).

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and [`Docs/superpowers/specs/2026-07-13-fase-3-spx-client-crypto-design.md`](../specs/2026-07-13-fase-3-spx-client-crypto-design.md). Read the design doc before starting; it is the source of truth for scope. Do not redesign.

**Architecture (copied from the design doc, "ikuti persis"):** Crypto is a **module inside `spx-client`**, not a new workspace crate. The master spec lists exactly 8 crates + 2 bins and says follow it exactly; adding a 9th crate violates that. The accepted trade-off: Fase 6 `api-gateway` will path-depend on `spx-client` for `password`/`session_token`, pulling `wreq` it does not directly use. This is a compile-cost trade-off, revisitable in Fase 6, **not** a reason to split out a crate now.

**Purpose-scoped HKDF `info` labels (verbatim from design doc — these are the structural fix for reference Gap #2, "one secret for AES and HMAC"):**
- `"tower.agency-credential.v1"` — encrypts SPX password (`agency_credentials`).
- `"tower.waha-key.v1"` — encrypts the WAHA API key (`site_settings`).
- `"tower.quick-accept-hmac.v1"` — reserved for the Fase 4+ HMAC quick-accept token (a *separate* HKDF subkey; the HMAC token itself is out of Fase 3 scope). Fase 3 defines this label and proves its subkey differs from the AES subkeys (DoD #4b), but does **not** implement the HMAC token.

**Envelope encryption invariants:**
- Master key is ALWAYS a full 32-byte value loaded from a file; there is **never** any `.pad()`/`.slice()`/truncation of a secret into a key anywhere in the crypto code (closes Gap #1). The only "length check" is `load_from_file` rejecting a file that is not exactly 32 bytes.
- Subkeys are ALWAYS the full 32-byte HKDF-SHA256 output. Never a raw secret used directly as a key.
- Nonce is ALWAYS 12 random bytes from the OS CSPRNG (`getrandom::fill`), fresh per encryption — never a counter/deterministic value.
- AAD binds ciphertext to its context: `format!("{label}|{tenant_id}")`. A ciphertext from one tenant/purpose cannot be silently relocated to another row and still decrypt.
- AES-256-GCM ciphertext INCLUDES the 16-byte auth tag appended at the end (the `aes-gcm`/`aead` allocating API does this automatically — confirmed by compile test).
- `key_version` = `1` for all of Fase 3 (stored in `agency_credentials.key_version` / the `site_settings` JSONB). Multi-master-key rotation is Fase 8 (extension point only, YAGNI).

**Secret handling (Aturan Keras #5):** Every value that is a plaintext secret in memory (master key, HKDF subkeys, plaintext password before hashing, plaintext SPX password before encryption / after decryption, session token before hashing) is carried in `secrecy::SecretString`/`SecretBox` (which zeroizes on drop — `secrecy` depends on `zeroize` and `SecretBox<S: Zeroize>` impls `ZeroizeOnDrop`). `Debug` is redacted. Any transient `Vec<u8>` holding key material (e.g. the file buffer in `load_from_file`) is `.zeroize()`d before it drops. Secrets are never logged.

**CRITICAL — crate versions are bleeding-edge; the code below was compile-verified, do not "modernize" it from memory.** On 2026-07-13 the installed toolchain is `cargo 1.97.0 / rustc 1.97.0`, and `cargo add` resolves the RustCrypto stack to a **new coordinated major release**: `aes-gcm 0.11` (migrated to `hybrid-array`; `aead 0.6`), `sha2 0.11`, `hkdf 0.13`, `hmac 0.13`, and `rand 0.10`. These differ in breaking ways from the 0.10/0.12-era APIs. Two findings you MUST respect:
  1. **`rand` 0.10 removed `OsRng`** (it is now `rand::rngs::SysRng`). To avoid that churn entirely, this plan does **not** use `rand` for randomness — it uses **`getrandom::fill(&mut buf)`** directly (getrandom 0.4.3, `MIT OR Apache-2.0`) for all nonce/token/salt bytes. Do not add `rand` and do not write `rand::rngs::OsRng`.
  2. **`aes-gcm 0.11`** uses `hybrid-array`: `Key::from_slice`/`Nonce::from_slice` are deprecated. Build the cipher with `Aes256Gcm::new_from_slice(&subkey)` (the `KeyInit` trait) and the nonce with `Nonce::<U12>::try_from(&nonce_bytes[..])` where `U12` is `aes_gcm::aead::consts::U12`. The allocating `encrypt`/`decrypt` (returning `Vec<u8>`, taking `Payload { msg, aad }`) come from `aes_gcm::aead::Aead` and require the `alloc` feature (default-on). This exact shape was compiled and its round-trip + AAD-rejection + purpose-label-distinctness tests pass — reproduce it exactly.
- `argon2 0.5.3`: generate the salt with `SaltString::encode_b64(&random_16_bytes)` (avoids depending on `password-hash`'s `getrandom` feature / an incompatible `rand_core` version). `Params::new(19456, 2, 1, None)` + `Argon2::new(Algorithm::Argon2id, Version::V0x13, params)` produces `$argon2id$...` — verified.
- `secrecy 0.10.3`: types are `SecretString` (= `SecretBox<str>`) and `SecretBox<S>`. Construct with `SecretString::from(String)` / `SecretBox::new(Box::new(value))`; read with `.expose_secret()` (`use secrecy::ExposeSecret`). Debug is redacted. Verified.

**Dependency licenses / `cargo deny`:** Baseline `cargo deny check` on the current workspace **passes** (`advisories ok, bans ok, licenses ok, sources ok`) — confirmed 2026-07-13. The allow-list is MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, Zlib. Findings for the new deps:
  - **Crypto stack (`aes-gcm`, `hkdf`, `sha2`, `argon2`, `secrecy`, `zeroize`, `getrandom`, `base64`, `thiserror`, `regex`, `serde_json`, `chrono`, `uuid`), `wreq`, `wiremock`:** all `MIT OR Apache-2.0` (or dual equivalents) — **in the allow-list, no deny action needed.** `wreq` itself is Apache-2.0.
  - **`wreq-util` (browser-emulation presets) — the ONE license landmine (mirrors Fase 2 Task 1's `webpki-roots` situation):** the **stable** line `wreq-util 2.2.6` is **GPL-3.0** — copyleft, NOT in the allow-list, and `cargo deny` will reject it. Its **Apache-2.0** licensing only exists on the `3.0.0-rc.x` pre-release line. **Do NOT add `wreq-util` 2.x and do NOT add GPL-3.0 to `deny.toml`** (copyleft is a real legal concern for this proprietary codebase, not a rubber-stamp). Task 8 documents the two acceptable paths (pin the Apache-2.0 pre-release, or go `wreq`-only with manual headers). Re-run `cargo add --dry-run` + check the license field at implementation time — pre-release licensing can shift.

**TLS-impersonation is best-effort (design doc):** The reference did NOT do real JA3/TLS impersonation — only static Chrome header spoofing. The design doc anticipated the exact target (Chrome 148) not being a bundled preset. **Confirmed: the highest Chrome preset in `wreq-util` is `Emulation::Chrome137`** (full list: Chrome100…137). Use `Chrome137` as the closest available, set the client-hints headers to match **Chrome 137** (not 148) so UA and fingerprint are self-consistent, and document it as best-effort needing periodic refresh. Do not hardcode 148 into the headers if the preset is 137.

**Honest test-fixture gap (design doc, verbatim intent):** There are **no recorded real SPX JSON bodies** anywhere in the reference repo (verified). The only real SPX message strings are the 8 retcode cases in `spx-accept.test.ts` — used verbatim for `classify_accept_response` (Task 7). `normalize_booking` fixtures (Task 6) are hand-built from the documented multi-key fallback field names in `normalizeBooking` (`spx.ts:116-195`), NOT from recorded bodies. **Every such synthetic fixture MUST carry a code comment stating it is synthesized from documented field names, not a recorded body** (DoD #9). Do not silently pass synthetic data off as real captures.

**No new migration needed.** Fase 2 already shipped `agency_credentials(ciphertext bytea, nonce bytea, key_version int)` (`0004_agency_credentials.sql`) and the generic `site_settings(tenant_id, key, value jsonb)` (`0012_site_settings.sql`). Fase 3 only writes into them. If a later Fase 3 review concludes a schema change is unavoidable, it must be a **new forward-only** `sqlx migrate add` file — never edit an existing migration.

**Reuse the untouched Fase 1 contract.** `core_domain::Booking`, `BookingType`, `booking_type_of`, `is_coc_name`, `parse_route_stops`, `parse_route_detail_list`, `RouteNode` are consumed as-is. Do NOT modify `core-domain`. `to_core_booking` maps into `core_domain::Booking`'s exact 11 fields; `normalize_booking`'s `booking_type` is derived via `core_domain::booking_type_of(real_booking_name)`.

**Workflow:** Run all `cargo` commands from `Backend/` (workspace root). Postgres-backed tests (Tasks 3, 4) connect to the Fase 2 `tower-postgres` container at **`127.0.0.1:15432`** (the temporary dev port publish from Fase 2 — do not remove it). Reuse `store::connect` / `store::run_migrations` / `store::begin_tenant_tx`. Run DB tests with `-- --test-threads=1`.

---

### Task 1: `spx-client` deps + `crypto::secret` + `crypto::envelope` (master key, HKDF, AES-256-GCM)

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (add crypto deps)
- Overwrite: `Backend/crates/spx-client/src/lib.rs`
- Create: `Backend/crates/spx-client/src/crypto/mod.rs`
- Create: `Backend/crates/spx-client/src/crypto/secret.rs`
- Create: `Backend/crates/spx-client/src/crypto/envelope.rs`

**Interfaces:**
- Consumes: nothing (first task; `spx-client` is already a registered workspace member with an empty `lib.rs`).
- Produces (used by later tasks — signatures are load-bearing, keep them exact):
  - `crypto::secret`: re-exports `SecretString`, `SecretBox`, `ExposeSecret` from `secrecy`.
  - `crypto::envelope::MasterKey` with `from_bytes([u8;32]) -> MasterKey`, `load_from_file(impl AsRef<Path>) -> Result<MasterKey, CryptoError>`, `load_default() -> Result<MasterKey, CryptoError>` (Task 5 fills in Docker wiring for the default path).
  - `pub struct Ciphertext { pub bytes: Vec<u8>, pub nonce: [u8; 12] }`
  - `pub fn encrypt(master: &MasterKey, label: &str, plaintext: &[u8], aad: &[u8]) -> Result<Ciphertext, CryptoError>`
  - `pub fn decrypt(master: &MasterKey, label: &str, ciphertext: &[u8], nonce: &[u8; 12], aad: &[u8]) -> Result<Vec<u8>, CryptoError>`
  - `pub fn aad_for(label: &str, tenant_id: Uuid) -> Vec<u8>`
  - Label consts `LABEL_AGENCY_CREDENTIAL`, `LABEL_WAHA_KEY`, `LABEL_QUICK_ACCEPT_HMAC`; `KEY_VERSION: i32 = 1`; `CryptoError`.
  - `pub(crate) fn derive_subkey(master: &MasterKey, label: &str) -> Result<SecretBox<[u8;32]>, CryptoError>` (used by the DoD #3/#4b byte-comparison tests).

- [x] **Step 1: Add the crypto dependencies**

```bash
cd Backend
cargo add --package spx-client aes-gcm@0.11 hkdf@0.13 sha2@0.11 secrecy@0.10 base64@0.22 getrandom@0.4 thiserror@2 uuid@1
cargo add --package spx-client zeroize@1
cd ..
```

Note: `uuid` is a normal (not dev) dep — `aad_for` and the Task 3/4 wrappers take a `Uuid`. `zeroize` is added directly (used to wipe the transient master-key file buffer) even though `secrecy` also pulls it transitively — this is intentional, not redundant. All of these are `MIT OR Apache-2.0` (verified) → no `cargo deny` action.

- [x] **Step 2: Write `crypto::secret`**

```rust
// Backend/crates/spx-client/src/crypto/secret.rs
//! Re-exports of the `secrecy` primitives used across the crypto module.
//!
//! Every plaintext secret in memory MUST be carried in one of these so that
//! `Debug`/`Display` are redacted and the memory is zeroized on drop (secrecy
//! depends on `zeroize`; `SecretBox<S: Zeroize>` implements `ZeroizeOnDrop`).
pub use secrecy::{ExposeSecret, SecretBox, SecretString};

/// A 32-byte key (master key or HKDF subkey) held in zeroize-on-drop memory.
pub type SecretKeyBytes = SecretBox<[u8; 32]>;
```

- [x] **Step 3: Write `crypto::envelope`**

This is the compile-verified core. Reproduce the API shape exactly (see Global Constraints for why each choice is what it is).

```rust
// Backend/crates/spx-client/src/crypto/envelope.rs
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

use crate::crypto::secret::{ExposeSecret, SecretBox};

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
```

- [x] **Step 4: Write `crypto::mod.rs` and `lib.rs`**

```rust
// Backend/crates/spx-client/src/crypto/mod.rs
pub mod envelope;
pub mod secret;

// NB: `derive_subkey` is `pub(crate)` (used only by envelope.rs's own tests) —
// do NOT add it to this `pub use`, re-exporting a pub(crate) item as pub is E0365.
pub use envelope::{
    aad_for, decrypt, encrypt, Ciphertext, CryptoError, MasterKey, KEY_VERSION,
    LABEL_AGENCY_CREDENTIAL, LABEL_QUICK_ACCEPT_HMAC, LABEL_WAHA_KEY,
};
pub use secret::{ExposeSecret, SecretBox, SecretKeyBytes, SecretString};
```

```rust
// Backend/crates/spx-client/src/lib.rs
pub mod crypto;
```

(Later tasks add `pub mod password;`/`session_token`/`booking`/`accept`/`cookies`/`client` to `crypto/mod.rs` and `lib.rs` respectively — do not remove this line.)

- [x] **Step 5: Build, test, clippy**

```bash
cd Backend
cargo test -p spx-client
cargo clippy -p spx-client -- -D warnings
cd ..
```

Expected: all `envelope` tests pass (round-trip, wrong-AAD fails, fresh nonce, distinct subkey bytes, full-32-byte subkey), clippy clean. If the compiler rejects any aes-gcm call, re-read Global Constraints' aes-gcm note — the shape (`new_from_slice`, `Nonce::<U12>::try_from`, `Payload`) was verified against 0.11.0; a rejection means a version drift you must reconcile against the installed version before proceeding, not a reason to revert to `from_slice`.

- [x] **Step 6: Commit**

```bash
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): crypto::envelope — master key/HKDF/AES-256-GCM envelope encryption"
```

---

### Task 2: `crypto::password` (argon2id) + `crypto::session_token` (256-bit token, SHA-256 hash)

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (add `argon2`)
- Create: `Backend/crates/spx-client/src/crypto/password.rs`
- Create: `Backend/crates/spx-client/src/crypto/session_token.rs`
- Modify: `Backend/crates/spx-client/src/crypto/mod.rs`

**Interfaces:**
- Consumes: `crypto::secret` (Task 1).
- Produces:
  - `pub fn hash_password(password: &str) -> Result<String, CryptoError>` — argon2id PHC string (`$argon2id$...`) for `portal_users.password_hash TEXT`.
  - `pub fn verify_password(password: &str, phc_hash: &str) -> bool`.
  - `pub fn generate_session_token() -> Result<(SecretString, [u8; 32]), CryptoError>` — returns `(plaintext_token, sha256_hash)`. Only the hash is ever persisted (to `portal_sessions.token_hash BYTEA`).
  - `pub fn hash_session_token(token: &str) -> [u8; 32]`.

Reuses `CryptoError` from `crypto::envelope`.

- [x] **Step 1: Add `argon2`**

```bash
cd Backend && cargo add --package spx-client argon2@0.5 && cd ..
```

`argon2 0.5.3` default features (`alloc`, `password-hash`) are sufficient — no `getrandom`/`rand` feature is needed because the salt is generated via `getrandom` + `SaltString::encode_b64` (see Global Constraints). `MIT OR Apache-2.0` → no deny action.

- [x] **Step 2: Write `crypto::password`**

```rust
// Backend/crates/spx-client/src/crypto/password.rs
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
```

- [x] **Step 3: Write `crypto::session_token`**

```rust
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
        assert_ne!(t1.expose_secret(), t2.expose_secret(), "256-bit tokens must be unique");
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
```

- [x] **Step 4: Wire into `crypto::mod.rs`**

Add to `Backend/crates/spx-client/src/crypto/mod.rs`:

```rust
pub mod password;
pub mod session_token;

pub use password::{hash_password, verify_password};
pub use session_token::{generate_session_token, hash_session_token};
```

- [x] **Step 5: Test + clippy + commit**

```bash
cd Backend
cargo test -p spx-client
cargo clippy -p spx-client -- -D warnings
cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): crypto::password (argon2id) + crypto::session_token (256-bit, SHA-256 hashed)"
```

Expected: all password + session_token tests pass; clippy clean.

---

### Task 3: Agency-credential envelope integration + real Postgres round-trip

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (dev-deps: `store`, `sqlx`, `tokio`)
- Modify: `Backend/crates/spx-client/src/crypto/envelope.rs` (add `encrypt_agency_password` / `decrypt_agency_password` + a `#[cfg(test)]` Postgres integration module)

**Interfaces:**
- Consumes: `encrypt`/`decrypt`/`aad_for`/`LABEL_AGENCY_CREDENTIAL`/`KEY_VERSION` (Task 1), Fase 2's `store::{connect, run_migrations, begin_tenant_tx}` + the `agency_credentials` table.
- Produces:
  - `pub fn encrypt_agency_password(master: &MasterKey, tenant_id: Uuid, password: &str) -> Result<Ciphertext, CryptoError>`
  - `pub fn decrypt_agency_password(master: &MasterKey, tenant_id: Uuid, ciphertext: &[u8], nonce: &[u8; 12]) -> Result<SecretString, CryptoError>`

**Design note (read before coding):** Fase 2's `agency_credentials` has a plaintext `username TEXT` column PLUS `ciphertext`/`nonce`/`key_version`. The reference stored username plaintext and encrypted only the password; this integration does the same — the plaintext `username` column is the surface-safe display value (mirrors the reference's `getStoredSpxUsername`), and **`ciphertext` holds the encrypted password**. The design doc's phrase "enkripsi username+password" refers to protecting the SPX credential *pair*; the password is the secret placed in `ciphertext`, and the username is bound into the ciphertext's context by the tenant-scoped AAD + its own row. `key_version` is written as `KEY_VERSION` (1).

- [x] **Step 1: Add dev-dependencies (same pattern as Fase 2's store tests)**

```bash
cd Backend
cargo add --package spx-client --dev --path crates/store store
cargo add --package spx-client --dev sqlx --features postgres,runtime-tokio-rustls,macros,uuid,chrono
cargo add --package spx-client --dev tokio --features rt-multi-thread,macros
cd ..
```

(`store` is a dev-dependency only — `spx-client`'s production code must not depend on `store`; these are for the Postgres round-trip test. `cargo add --path crates/store` resolves the workspace member.)

- [x] **Step 2: Add the agency-credential wrappers to `crypto::envelope`**

Append to `Backend/crates/spx-client/src/crypto/envelope.rs` (before the `#[cfg(test)]` module), reusing `SecretString`:

```rust
use crate::crypto::secret::SecretString;

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
```

(If `SecretString` is already imported at the top of the file from Task 1's edits, do not duplicate the `use`. Keep a single import.)

- [x] **Step 3: Write the Postgres round-trip integration test**

Create `Backend/crates/spx-client/tests/agency_credentials_pg.rs` (an integration test crate so it can pull the `store` dev-dep cleanly):

```rust
// Backend/crates/spx-client/tests/agency_credentials_pg.rs
//! Real Postgres round-trip: encrypt an SPX password, insert into Fase 2's
//! `agency_credentials`, fetch, decrypt, assert equality — and assert the stored
//! ciphertext never contains the plaintext. Connects to the Fase 2 tower-postgres
//! container at 127.0.0.1:15432. Run with `-- --test-threads=1`.
use spx_client::crypto::envelope::{
    decrypt_agency_password, encrypt_agency_password, MasterKey, KEY_VERSION,
};
use uuid::Uuid;

fn test_database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn test_master() -> MasterKey {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    MasterKey::from_bytes(b)
}

#[tokio::test]
async fn agency_credential_encrypt_store_fetch_decrypt() {
    let pool = store::connect(&test_database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");

    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("SPX Test Tenant")
        .bind(format!("spx-{tenant_id}"))
        .execute(&pool)
        .await
        .expect("insert tenant");

    let master = test_master();
    let plaintext_password = "sup3r-s3cret-spx-pw";
    let ct = encrypt_agency_password(&master, tenant_id, plaintext_password).expect("encrypt");

    // Insert under RLS via the tenant-scoped transaction (Fase 2 pattern).
    let mut tx = store::begin_tenant_tx(&pool, tenant_id).await.expect("tx");
    sqlx::query(
        "INSERT INTO agency_credentials \
         (tenant_id, label, username, ciphertext, nonce, key_version) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tenant_id)
    .bind("primary")
    .bind("agency-login@example.com") // username stays plaintext (surface-safe)
    .bind(&ct.bytes)
    .bind(&ct.nonce[..])
    .bind(KEY_VERSION)
    .execute(&mut *tx)
    .await
    .expect("insert credential");
    tx.commit().await.expect("commit");

    // Fetch back and decrypt.
    let row: store::models::AgencyCredential = {
        let mut tx = store::begin_tenant_tx(&pool, tenant_id).await.expect("tx2");
        let r = sqlx::query_as::<_, store::models::AgencyCredential>(
            "SELECT * FROM agency_credentials WHERE tenant_id = $1 AND label = 'primary'",
        )
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await
        .expect("fetch credential");
        tx.commit().await.ok();
        r
    };

    assert_eq!(row.key_version, KEY_VERSION);
    // The stored ciphertext must NEVER contain the plaintext bytes.
    assert!(
        !row.ciphertext.windows(plaintext_password.len()).any(|w| w == plaintext_password.as_bytes()),
        "plaintext password leaked into stored ciphertext"
    );

    let nonce: [u8; 12] = row.nonce.as_slice().try_into().expect("12-byte nonce");
    let decrypted = decrypt_agency_password(&master, tenant_id, &row.ciphertext, &nonce)
        .expect("decrypt");
    use spx_client::crypto::secret::ExposeSecret;
    assert_eq!(decrypted.expose_secret(), plaintext_password);

    // Cleanup.
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
}
```

(`getrandom` is already a normal dependency of `spx-client` from Task 1, so it is usable from the integration test without a separate add.)

- [x] **Step 4: Bring up Postgres, run the test**

```bash
cd Docker && docker compose up -d tower-postgres && cd ..
# wait for healthy: docker compose -f Docker/docker-compose.yml ps
cd Backend
cargo test -p spx-client --test agency_credentials_pg -- --test-threads=1
cd ..
```

Expected: `test result: ok. 1 passed`. If it cannot connect, confirm the container is healthy and that the temporary `127.0.0.1:15432` publish from Fase 2 is present in `Docker/docker-compose.yml` (it is; do not remove it).

- [x] **Step 5: Clippy + commit**

```bash
cd Backend && cargo clippy -p spx-client --all-targets -- -D warnings && cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): agency-credential envelope encryption + Postgres round-trip test"
```

---

### Task 4: WAHA-key envelope integration into `site_settings` (JSONB, base64 ciphertext)

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (add `serde`, `serde_json`)
- Modify: `Backend/crates/spx-client/src/crypto/envelope.rs` (add `encrypt_waha_key` / `decrypt_waha_key`)
- Create: `Backend/crates/spx-client/src/waha_settings.rs` (the `site_settings` JSONB shape)
- Modify: `Backend/crates/spx-client/src/lib.rs` (`pub mod waha_settings;`)
- Create: `Backend/crates/spx-client/tests/waha_settings_pg.rs`

**Interfaces:**
- Consumes: `encrypt`/`decrypt`/`aad_for`/`LABEL_WAHA_KEY`/`KEY_VERSION` (Task 1), Fase 2's `site_settings(tenant_id, key, value jsonb)`.
- Produces:
  - `pub fn encrypt_waha_key(master, tenant_id, key) -> Result<Ciphertext, CryptoError>` / `pub fn decrypt_waha_key(...) -> Result<SecretString, CryptoError>`.
  - `waha_settings::WahaSettings` — the JSONB value: non-sensitive `waha_url`/`waha_session` stay plaintext; only the API key is stored as base64 ciphertext+nonce+key_version. `to_json_value()` / `from_json_value()` helpers.

- [x] **Step 1: Add serde deps**

```bash
cd Backend
cargo add --package spx-client serde --features derive
cargo add --package spx-client serde_json
cd ..
```

- [x] **Step 2: Add the WAHA-key wrappers to `crypto::envelope`**

Append to `envelope.rs` (before the test module):

```rust
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
```

- [x] **Step 3: Write `waha_settings`**

```rust
// Backend/crates/spx-client/src/waha_settings.rs
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
```

Add `pub mod waha_settings;` to `Backend/crates/spx-client/src/lib.rs`.

- [x] **Step 4: Write the Postgres round-trip + plaintext-absence test**

```rust
// Backend/crates/spx-client/tests/waha_settings_pg.rs
//! Store the encrypted WAHA settings in `site_settings`, fetch the JSONB back,
//! decrypt, assert equality — and assert the stored JSONB column text never
//! contains the plaintext key (DoD #4c). Connects to tower-postgres @ :15432.
use spx_client::crypto::envelope::MasterKey;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};
use uuid::Uuid;

fn test_database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn master() -> MasterKey {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    MasterKey::from_bytes(b)
}

#[tokio::test]
async fn waha_key_encrypted_in_site_settings_jsonb() {
    let pool = store::connect(&test_database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");

    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id).bind("WAHA Tenant").bind(format!("waha-{tenant_id}"))
        .execute(&pool).await.expect("insert tenant");

    let m = master();
    let api_key = "waha-secret-APIKEY-9988";
    let settings = WahaSettings::encrypt_new(&m, tenant_id, "http://waha:3000", "default", api_key)
        .expect("encrypt settings");

    let mut tx = store::begin_tenant_tx(&pool, tenant_id).await.expect("tx");
    sqlx::query("INSERT INTO site_settings (tenant_id, key, value) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(SITE_SETTINGS_KEY)
        .bind(settings.to_json_value())
        .execute(&mut *tx)
        .await
        .expect("insert site_settings");
    tx.commit().await.expect("commit");

    // Assert at the DB level that the JSONB text has no plaintext key.
    let (stored_text,): (String,) = sqlx::query_as(
        "SELECT value::text FROM site_settings WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(SITE_SETTINGS_KEY)
    .fetch_one(&pool)
    .await
    .expect("fetch jsonb text");
    assert!(!stored_text.contains(api_key), "plaintext WAHA key in stored JSONB: {stored_text}");

    // Fetch JSONB, decrypt, assert equality.
    let (value,): (serde_json::Value,) = sqlx::query_as(
        "SELECT value FROM site_settings WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(SITE_SETTINGS_KEY)
    .fetch_one(&pool)
    .await
    .expect("fetch jsonb");
    let parsed = WahaSettings::from_json_value(&value).expect("parse");
    use spx_client::crypto::secret::ExposeSecret;
    assert_eq!(parsed.decrypt_api_key(&m, tenant_id).unwrap().expose_secret(), api_key);
    assert_eq!(parsed.waha_url, "http://waha:3000");

    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
}
```

The `tests/` crate needs `serde_json` and `tokio` — already available (`serde_json` is a normal dep from Step 1; `tokio` + `store` + `sqlx` are dev-deps from Task 3). If `cargo` reports `serde_json` unavailable to the test target, it is a normal dep so it is already in scope; no extra add.

- [x] **Step 5: Run + clippy + commit**

```bash
cd Docker && docker compose up -d tower-postgres && cd ..
cd Backend
cargo test -p spx-client -- --test-threads=1
cargo clippy -p spx-client --all-targets -- -D warnings
cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): WAHA-key envelope encryption into site_settings JSONB"
```

Expected: the in-memory `waha_settings` test + both Postgres round-trip tests (Task 3 + Task 4) pass; the `value::text` assertion genuinely observes no plaintext.

---

### Task 5: Docker Compose `secrets:` wiring + master-key loader test

**Files:**
- Modify: `Docker/docker-compose.yml` (add top-level `secrets:` + per-service mount)
- Modify: `Docker/.env.example` (replace the "arrives Fase 3" comment with concrete instructions)
- Modify: `.gitignore` (ensure `Docker/secrets/` is ignored — the root already ignores `secrets/`; make it explicit)
- Create: `Backend/crates/spx-client/tests/master_key_loader.rs`

**Interfaces:**
- Consumes: `MasterKey::load_from_file` / `load_default` (Task 1).
- Produces: Compose secret wiring so `reactor-core` / `auth-sidecar` see `/run/secrets/tower_master_key`; a filesystem-level unit test of the loader.

**Scope note (explicit, per design doc):** The loader is tested at the **filesystem level against a temp file** — this needs NO live container. A running-container test of the Compose mount is out of scope for Fase 3 (it belongs to Fase 8 deployment verification). The unit test below is sufficient and is stated as such.

- [x] **Step 1: Add the `secrets:` stanza to `Docker/docker-compose.yml`**

Add a top-level `secrets:` block (a sibling of `services:`/`networks:`/`volumes:`) pointing at a gitignored local file, and mount it into the two Rust services that load the master key. Do NOT touch the `tower-postgres` `ports: 127.0.0.1:15432` publish (Fase 2's temporary dev convenience).

```yaml
# top-level, sibling of `services:` / `volumes:` / `networks:`
secrets:
  tower_master_key:
    # 32 raw random bytes, created ONCE by the operator (not auto-generated by
    # code, so the operator knows it exists and can back it up):
    #   mkdir -p Docker/secrets && openssl rand -out Docker/secrets/tower_master_key 32 && chmod 0400 Docker/secrets/tower_master_key
    file: ./secrets/tower_master_key
```

Then add a `secrets:` list to `tower-reactor-core` and `tower-auth-sidecar` (Compose mounts each at `/run/secrets/<name>` with mode 0400 by default) and point the loader at it via env:

```yaml
  tower-reactor-core:
    # ...existing keys unchanged...
    environment:
      TOWER_MASTER_KEY_PATH: /run/secrets/tower_master_key
    secrets:
      - tower_master_key

  tower-auth-sidecar:
    # ...existing keys unchanged...
    environment:
      TOWER_MASTER_KEY_PATH: /run/secrets/tower_master_key
    secrets:
      - tower_master_key
```

Rationale (design doc): a file secret at 0400 is not exposed via `docker inspect` / `/proc/<pid>/environ`, unlike an env var. The env var here carries only the *path*, not the key.

- [x] **Step 2: Update `Docker/.env.example`'s comment**

Replace the Postgres-line comment `# Postgres (Fase 0 placeholder — real secrets management arrives Fase 3)` and the trailing "This file grows..." block with a concrete instruction that the master key is a Docker **file secret**, not an env var:

```
# Postgres dev password (local only). Everything sensitive beyond this — the
# envelope-encryption master key, SPX credentials, WAHA key — is NOT stored in
# .env. The master key is a Docker *file secret*:
#   mkdir -p Docker/secrets
#   openssl rand -out Docker/secrets/tower_master_key 32
#   chmod 0400 Docker/secrets/tower_master_key
# Docker/secrets/ is gitignored; back up tower_master_key out-of-band (losing it
# makes every stored ciphertext unrecoverable). Containers read it at
# /run/secrets/tower_master_key via the compose `secrets:` stanza.
POSTGRES_PASSWORD=tower_dev_only

# Rust logging
RUST_LOG=info
```

- [x] **Step 3: Ensure the secrets dir is gitignored**

The root `.gitignore` already has a `secrets/` entry (matches any `secrets/` dir). Add an explicit, unambiguous entry so a future reader is not surprised:

```gitignore
# Docker file-secrets (master key etc.) — never commit these
Docker/secrets/
```

Verify nothing under `Docker/secrets/` is tracked: `git status --porcelain Docker/secrets/ 2>/dev/null` should show nothing (the dir may not even exist yet — that's fine).

- [x] **Step 4: Write the loader unit test (temp file, no container)**

```rust
// Backend/crates/spx-client/tests/master_key_loader.rs
//! Filesystem-level test of MasterKey::load_from_file — no container needed.
use spx_client::crypto::envelope::MasterKey;

fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut n = [0u8; 8];
    getrandom::fill(&mut n).unwrap();
    let suffix = u64::from_le_bytes(n);
    std::env::temp_dir().join(format!("tower_master_key_{tag}_{suffix}"))
}

#[test]
fn loads_exactly_32_bytes() {
    let path = temp_path("ok");
    let key_bytes: [u8; 32] = [7u8; 32];
    std::fs::write(&path, key_bytes).unwrap();

    let mk = MasterKey::load_from_file(&path).expect("load 32-byte key");
    // Debug must be redacted (no key material).
    assert_eq!(format!("{mk:?}"), "MasterKey([REDACTED])");

    std::fs::remove_file(&path).ok();
}

#[test]
fn rejects_wrong_length() {
    let short = temp_path("short");
    std::fs::write(&short, [1u8; 16]).unwrap(); // 16 bytes, not 32
    assert!(MasterKey::load_from_file(&short).is_err(), "must reject a 16-byte file");
    std::fs::remove_file(&short).ok();

    let long = temp_path("long");
    std::fs::write(&long, [1u8; 64]).unwrap(); // 64 bytes
    assert!(MasterKey::load_from_file(&long).is_err(), "must reject a 64-byte file");
    std::fs::remove_file(&long).ok();
}

#[test]
fn missing_file_is_io_error() {
    assert!(MasterKey::load_from_file("/nonexistent/tower_master_key_xyz").is_err());
}
```

- [x] **Step 5: Run + validate compose + commit**

```bash
cd Backend && cargo test -p spx-client --test master_key_loader && cd ..
# Validate compose parses (needs a placeholder secret file to exist for `config`):
mkdir -p Docker/secrets && head -c 32 /dev/urandom > Docker/secrets/tower_master_key && chmod 0400 Docker/secrets/tower_master_key
docker compose -f Docker/docker-compose.yml config >/dev/null && echo "compose OK"
git add Docker/docker-compose.yml Docker/.env.example .gitignore Backend/crates/spx-client
git commit -m "feat(docker): master-key file-secret wiring + loader test"
```

Expected: loader tests pass; `docker compose config` validates; `git status` confirms `Docker/secrets/tower_master_key` is NOT staged (gitignored).

---

### Task 6: `SpxBooking` (29 fields) + `normalize_booking` + `to_core_booking`

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (add `chrono`, `core-domain` path dep; `serde_json` already added in Task 4)
- Create: `Backend/crates/spx-client/src/booking.rs`
- Modify: `Backend/crates/spx-client/src/lib.rs` (`pub mod booking;`)

**Interfaces:**
- Consumes: `core_domain::{Booking, BookingType, booking_type_of, parse_route_stops, parse_route_detail_list, RouteNode}` (Fase 1, unchanged).
- Produces: `SpxBooking` (29 fields), `pub fn normalize_booking(raw: &serde_json::Value) -> SpxBooking`, `pub fn to_core_booking(b: &SpxBooking) -> core_domain::Booking`.

**Fidelity source:** `normalizeBooking` (`spx.ts:116-195`) and `parseProvinces` (`spx.ts:92-114`). The multi-key `pick`, `to_ms`, numeric-vehicle discard, status map, and WIB time formatting below were **compile+run verified** against `serde_json` 1 + `chrono` 0.4 in a scratch crate. `to_core_booking` maps into `core_domain::Booking`'s exact 11 fields.

- [x] **Step 1: Add deps**

```bash
cd Backend
cargo add --package spx-client chrono@0.4
cargo add --package spx-client --path crates/core-domain core-domain
cd ..
```

- [x] **Step 2: Write `booking.rs`**

```rust
// Backend/crates/spx-client/src/booking.rs
//! Full SPX booking shape (mirror of the reference `SpxBooking`, spx.ts:7-37)
//! + normalize_booking (spx.ts:116-195) + a mapping down to Fase 1's
//! core_domain::Booking (which is NOT modified).
use chrono::{DateTime, FixedOffset, Utc};
use core_domain::{booking_type_of, parse_route_detail_list, parse_route_stops, BookingType};
use serde_json::Value;

/// 29-field SPX booking. `booking_type` reuses `core_domain::BookingType`; the
/// timestamp fields are epoch-ms (`deadline_at`/`created_at`) or preformatted
/// strings (`pickup_time` = ISO-8601 UTC, `pickup_time_str` = WIB HH:MM). Not
/// serde-derived: `core_domain::BookingType` is not `Serialize` and Fase 1 must
/// not be modified — the API-serialization layer (Fase 6) handles conversion.
#[derive(Debug, Clone)]
pub struct SpxBooking {
    pub id: String,
    pub booking_id: String,
    pub request_id: String,
    pub onsite_id: Option<String>,
    pub spx_tx_id: String,
    pub booking_type: BookingType,
    pub status: String,
    pub vehicle_type: String,
    pub vehicle_capacity: String,
    pub weight: f64,
    pub cod: bool,
    pub cod_amount: f64,
    pub coc_count: i64,
    pub shift_type: i32,
    pub trip_type: i32,
    pub tier_per_round: i64,
    pub time_per_round: i64,
    pub award_logic: i64,
    pub route_stops: Vec<String>,
    pub report_station: String,
    pub origin_province: String,
    pub origin_region: String,
    pub destination_province: String,
    pub destination_region: String,
    pub pickup_time: String,
    pub pickup_time_str: String,
    pub deadline_at: i64,
    pub created_at: i64,
    pub raw: Value,
}

// ── Coercion helpers (mirror the reference's pick/Number/String/toMs) ──────────

/// `pick(obj, ...keys)`: first key present, non-null, non-empty-string.
fn pick<'a>(raw: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let obj = raw.as_object()?;
    for k in keys {
        if let Some(v) = obj.get(*k) {
            let empty = v.is_null() || v.as_str() == Some("");
            if !empty {
                return Some(v);
            }
        }
    }
    None
}

/// `String(pick(...) ?? '')`.
fn pick_str(raw: &Value, keys: &[&str]) -> String {
    match pick(raw, keys) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// `Number(pick(...) ?? 0)` — JSON numbers and numeric strings coerce; else 0.
fn to_num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

fn pick_num(raw: &Value, keys: &[&str]) -> f64 {
    pick(raw, keys).map(to_num).unwrap_or(0.0)
}

/// `Boolean(pick(...) ?? false)` — JS truthiness of the picked value.
fn pick_bool(raw: &Value, keys: &[&str]) -> bool {
    match pick(raw, keys) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
        None => false,
    }
}

/// `toMs(v)`: 0 -> 0; > 1e12 already ms; else seconds*1000.
fn to_ms(v: f64) -> i64 {
    if v == 0.0 {
        0
    } else if v > 1e12 {
        v as i64
    } else {
        (v * 1000.0) as i64
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// ISO-8601 UTC + WIB (UTC+7 fixed offset, no DST) HH:MM.
fn format_times(ms: i64) -> (String, String) {
    let dt: DateTime<Utc> = DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(Utc::now);
    let iso = dt.to_rfc3339();
    let wib = FixedOffset::east_opt(7 * 3600).expect("valid +7 offset");
    let hhmm = dt.with_timezone(&wib).format("%H:%M").to_string();
    (iso, hhmm)
}

/// `x ?? y ?? z` (nullish) as a String: null/missing skip, a present empty
/// string is kept (distinct from `pick`, which also skips empty strings). Used
/// for the province chain, which the reference resolves with `??`, not `pick`.
fn nullish_str(raw: &Value, keys: &[&str]) -> Option<String> {
    let obj = raw.as_object()?;
    for k in keys {
        match obj.get(*k) {
            None | Some(Value::Null) => continue,
            Some(Value::String(s)) => return Some(s.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            Some(Value::Bool(b)) => return Some(b.to_string()),
            Some(v) => return Some(v.to_string()),
        }
    }
    None
}

struct Provinces {
    origin: String,
    dest: String,
    origin_region: String,
    dest_region: String,
}

/// Port of parseProvinces (spx.ts:92-114).
fn parse_provinces(raw: &Value) -> Provinces {
    let nodes = parse_route_detail_list(raw);
    if nodes.len() >= 2 {
        let first = &nodes[0];
        let last = &nodes[nodes.len() - 1];
        return Provinces {
            origin: first.province.clone(),
            dest: last.province.clone(),
            origin_region: first.name.clone(),
            dest_region: last.name.clone(),
        };
    }
    let province_full = nullish_str(raw, &["sgi_province_name", "province_name"]).unwrap_or_default();
    let parts: Vec<&str> = province_full.split(" -> ").collect();
    let first_part = parts.first().copied().unwrap_or("").to_string();
    let last_part = parts.last().copied().unwrap_or("").to_string();
    Provinces {
        origin: nullish_str(raw, &["origin_province", "pickup_province"]).unwrap_or(first_part),
        dest: nullish_str(raw, &["dest_province", "delivery_province"]).unwrap_or(last_part),
        origin_region: nullish_str(raw, &["origin_dc_name", "origin_hub", "report_station_name"])
            .unwrap_or_default(),
        dest_region: nullish_str(raw, &["dest_dc_name", "dest_hub"]).unwrap_or_default(),
    }
}

/// Port of normalizeBooking (spx.ts:116-195).
pub fn normalize_booking(raw: &Value) -> SpxBooking {
    let booking_id = pick_str(raw, &["booking_id", "bookingId", "booking_sn", "id"]);
    let request_id = pick_str(raw, &["request_id", "requestId", "req_id"]);
    let spx_tx_id = {
        let v = pick_str(raw, &["booking_name", "spx_tx_id", "spxTxId", "tx_id", "tracking_no"]);
        if v.is_empty() {
            booking_id.clone()
        } else {
            v
        }
    };

    let route_stops = parse_route_stops(raw);
    let provinces = parse_provinces(raw);

    let deadline_at = match pick(raw, &["bidding_ddl", "deadline_at", "pickup_time_ms", "expired_at"]) {
        Some(v) => to_ms(to_num(v)),
        None => now_ms() + 3_600_000,
    };
    let pickup_ms = match pick(raw, &["booking_date", "schedule_at", "pickup_time", "pickup_date"]) {
        Some(v) => to_ms(to_num(v)),
        None => deadline_at,
    };
    let (pickup_time, pickup_time_str) = format_times(pickup_ms);
    let created_at = match pick(raw, &["ctime", "created_at", "create_time", "createdAt"]) {
        Some(v) => to_ms(to_num(v)),
        None => now_ms(),
    };

    // Vehicle type: prefer display name; a BARE-NUMERIC code is discarded (M5).
    let vtype_name = pick_str(raw, &["vehicle_type_name", "right_vehicle_type_name", "sgi_vehicle_name"]);
    let vtype_code = pick_str(raw, &["truck_type", "vehicle_type", "vehicleType", "service_type"]);
    let vtype_code_clean = {
        let t = vtype_code.trim();
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            String::new()
        } else {
            vtype_code.clone()
        }
    };
    let vehicle_type = if !vtype_name.is_empty() {
        vtype_name
    } else {
        vtype_code_clean
    };
    let vehicle_capacity = pick_str(raw, &["truck_capacity", "vehicle_capacity", "vehicleCapacity"]);

    // Status: numeric 1/2/3 -> pending/accepted/failed, else stringify (default pending).
    let status = {
        let v = pick(raw, &["request_acceptance_status", "status", "booking_status"]);
        let code = v.and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        });
        match code {
            Some(1) => "pending".to_string(),
            Some(2) => "accepted".to_string(),
            Some(3) => "failed".to_string(),
            _ => match v {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "pending".to_string(),
            },
        }
    };

    // COC/SPXID type from the REAL transaction name (booking_name), NOT the
    // bookingId fallback (M4). Absent real name -> reguler.
    let tx_name_for_type =
        pick_str(raw, &["booking_name", "spx_tx_id", "spxTxId", "tx_id", "tracking_no"]);
    let booking_type = booking_type_of(&tx_name_for_type);

    let onsite_raw = pick_str(raw, &["onsite_id", "onsiteId"]);
    let onsite_id = if onsite_raw.is_empty() {
        None
    } else {
        Some(onsite_raw)
    };

    let id = {
        if !booking_id.is_empty() {
            booking_id.clone()
        } else if !request_id.is_empty() {
            request_id.clone()
        } else {
            spx_tx_id.clone()
        }
    };

    SpxBooking {
        id,
        booking_id,
        request_id,
        onsite_id,
        spx_tx_id,
        booking_type,
        status,
        vehicle_type,
        vehicle_capacity,
        weight: pick_num(raw, &["weight", "total_weight", "item_weight"]),
        cod: pick_bool(raw, &["is_coc", "is_cod", "cod", "has_coc"]),
        cod_amount: pick_num(raw, &["cod_amount", "coc_amount", "codAmount"]),
        coc_count: pick_num(raw, &["coc_count", "cod_count"]) as i64,
        shift_type: pick_num(raw, &["shift_type"]) as i32,
        trip_type: pick_num(raw, &["trip_type"]) as i32,
        tier_per_round: pick_num(raw, &["tier_per_round"]) as i64,
        time_per_round: pick_num(raw, &["time_per_round"]) as i64,
        award_logic: pick_num(raw, &["award_logic"]) as i64,
        route_stops,
        report_station: pick_str(raw, &["report_station_name"]),
        origin_province: provinces.origin,
        origin_region: provinces.origin_region,
        destination_province: provinces.dest,
        destination_region: provinces.dest_region,
        pickup_time,
        pickup_time_str,
        deadline_at,
        created_at,
        raw: raw.clone(),
    }
}

/// Map the full SpxBooking down to Fase 1's core_domain::Booking (11 fields).
/// core_domain::Booking is NOT modified; this only consumes it.
pub fn to_core_booking(b: &SpxBooking) -> core_domain::Booking {
    core_domain::Booking {
        route_stops: b.route_stops.clone(),
        report_station: b.report_station.clone(),
        spx_tx_id: b.spx_tx_id.clone(),
        booking_id: b.booking_id.clone(),
        request_id: b.request_id.clone(),
        booking_type: b.booking_type,
        vehicle_type: b.vehicle_type.clone(),
        weight: b.weight,
        cod_amount: b.cod_amount,
        shift_type: b.shift_type,
        trip_type: b.trip_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // NOTE (DoD #9): every fixture below is SYNTHESIZED from the documented
    // multi-key field names in normalizeBooking (spx.ts:116-195). There are NO
    // recorded real SPX booking bodies anywhere in the reference repo — these are
    // NOT captured payloads, and are not represented as such.

    #[test]
    fn multi_key_fallback_priority() {
        // booking_id empty -> skip -> bookingId wins; id is never reached.
        let raw = json!({ "booking_id": "", "bookingId": "B123", "id": "IGNORED" });
        let b = normalize_booking(&raw);
        assert_eq!(b.booking_id, "B123");
    }

    #[test]
    fn numeric_only_vehicle_type_is_discarded() {
        // A bare numeric code ("3") is an internal id, not a vehicle TYPE -> "".
        let raw = json!({ "vehicle_type": "3" });
        assert_eq!(normalize_booking(&raw).vehicle_type, "");
        // A real name is kept.
        let raw2 = json!({ "vehicle_type": "CDD" });
        assert_eq!(normalize_booking(&raw2).vehicle_type, "CDD");
        // Display name beats a numeric code.
        let raw3 = json!({ "vehicle_type_name": "CDD LONG (6WH)", "vehicle_type": "3" });
        assert_eq!(normalize_booking(&raw3).vehicle_type, "CDD LONG (6WH)");
    }

    #[test]
    fn status_code_mapping() {
        assert_eq!(normalize_booking(&json!({ "status": 1 })).status, "pending");
        assert_eq!(normalize_booking(&json!({ "status": "2" })).status, "accepted");
        assert_eq!(normalize_booking(&json!({ "request_acceptance_status": 3 })).status, "failed");
        assert_eq!(normalize_booking(&json!({ "status": "weird" })).status, "weird");
        assert_eq!(normalize_booking(&json!({})).status, "pending");
    }

    #[test]
    fn booking_type_from_booking_name_not_booking_id_fallback() {
        // A real booking_name of SPXID... classifies as spxid, even when the
        // numeric booking_id would look "reguler".
        let coc = json!({ "booking_id": "884412771", "booking_name": "SPXID99887766" });
        assert_eq!(normalize_booking(&coc).booking_type, BookingType::Spxid);
        // The M4 guarantee: with NO real booking_name and a non-SPXID booking_id,
        // the type must be reguler (it must NOT be inferred from anything but the
        // real transaction name; an absent name cannot prove SPXID).
        let reg = json!({ "booking_id": "884412771" });
        assert_eq!(normalize_booking(&reg).booking_type, BookingType::Reguler);
    }

    #[test]
    fn to_core_booking_maps_all_11_fields() {
        let raw = json!({
            "booking_id": "B1", "request_id": "R1", "booking_name": "SPXID1",
            "vehicle_type_name": "CDD", "weight": 12.5, "cod_amount": 300000,
            "shift_type": 1, "trip_type": 2, "report_station_name": "Padang DC",
            "route_stops": ["Padang DC", "Cileungsi DC"]
        });
        let b = normalize_booking(&raw);
        let core = to_core_booking(&b);
        assert_eq!(core.booking_id, "B1");
        assert_eq!(core.request_id, "R1");
        assert_eq!(core.spx_tx_id, "SPXID1");
        assert_eq!(core.booking_type, BookingType::Spxid);
        assert_eq!(core.vehicle_type, "CDD");
        assert_eq!(core.weight, 12.5);
        assert_eq!(core.cod_amount, 300000.0);
        assert_eq!(core.shift_type, 1);
        assert_eq!(core.trip_type, 2);
        assert_eq!(core.report_station, "Padang DC");
        assert_eq!(core.route_stops, vec!["Padang DC", "Cileungsi DC"]);
    }
}
```

Add `pub mod booking;` to `lib.rs`.

- [x] **Step 3: Test + clippy + commit**

```bash
cd Backend
cargo test -p spx-client --lib booking
cargo clippy -p spx-client --all-targets -- -D warnings
cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): SpxBooking + normalize_booking + to_core_booking"
```

Expected: all booking tests pass (multi-key fallback, numeric-vehicle discard, status map, booking_type-from-name, 11-field mapping).

---

### Task 7: `classify_accept_response` (6-category retcode classifier)

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (add `regex`)
- Create: `Backend/crates/spx-client/src/accept.rs`
- Modify: `Backend/crates/spx-client/src/lib.rs` (`pub mod accept;`)

**Interfaces:**
- Produces: `AcceptReason` (enum: `Ok, AgencyDup, Taken, Transient, Auth, Error`), `AcceptResult`, `pub fn classify_accept_response(retcode: i64, json_success: bool, raw_msg: &str) -> AcceptResult`.

**Fidelity source:** `classifyAcceptResponse` (`spx.ts:922-944`). The regex port + all 8 real message cases + the `agency_dup`-before-`ok` ordering were **compile+run verified** in a scratch crate against `regex` 1. The 8 cases are the ones in `spx-accept.test.ts` (the only real SPX message corpus that exists). **Check-order is load-bearing:** `agency_dup` is matched BEFORE the idempotent-`ok` pattern, so "your agency already accepted" is not swallowed as a self-win.

- [x] **Step 1: Add `regex`**

```bash
cd Backend && cargo add --package spx-client regex@1 && cd ..
```

- [x] **Step 2: Write `accept.rs`**

```rust
// Backend/crates/spx-client/src/accept.rs
//! Pure 6-category classification of an SPX accept response (port of
//! spx.ts:922-944). Order matters: agency_dup is checked BEFORE the idempotent
//! "already accepted by you" pattern.
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptReason {
    /// Accepted (incl. idempotent "already yours").
    Ok,
    /// SPX says OUR AGENCY already accepted — may be another account in the same
    /// agency (kasus Neva). Terminal like `Ok` (never retried), but the caller
    /// MUST verify the real acceptor and reclassify.
    AgencyDup,
    /// Another agency won / expired / closed — definitive, do not retry.
    Taken,
    /// Network/timeout/5xx/429/rate-limit — safe to retry.
    Transient,
    /// 401/403 — cookies expired — trigger relogin.
    Auth,
    /// Unexpected — logged for diagnosis.
    Error,
}

#[derive(Debug, Clone)]
pub struct AcceptResult {
    pub success: bool,
    pub reason: AcceptReason,
    pub retcode: i64,
    pub message: String,
}

static RE_AGENCY_DUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"agency.{0,12}already.{0,12}(accept|take|took)").unwrap());
static RE_OK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"already.*(accept|own|your)|accepted by you|duplicate|telah .*terima").unwrap()
});
static RE_TAKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"taken|awarded|no longer|not available|unavailable|expired|closed|full|grabbed|assigned|sudah .*diambil|habis|tidak tersedia|kedaluwarsa",
    )
    .unwrap()
});
static RE_TRANSIENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"rate|too many|frequent|try again|busy|timeout|sibuk|coba lagi").unwrap()
});

/// Classify the accept response body. `retcode`/`json_success` come from the
/// SPX JSON; `raw_msg` is its `message`/`msg` field. HTTP-status-based `Auth`
/// and network `Transient` are decided by the caller (Task 9) BEFORE this — this
/// function only classifies a parsed JSON body.
pub fn classify_accept_response(retcode: i64, json_success: bool, raw_msg: &str) -> AcceptResult {
    let m = raw_msg.to_lowercase();
    let done = |success: bool, reason: AcceptReason| AcceptResult {
        success,
        reason,
        retcode,
        message: raw_msg.to_string(),
    };

    if retcode == 0 || json_success {
        return done(true, AcceptReason::Ok);
    }
    if RE_AGENCY_DUP.is_match(&m) {
        return done(true, AcceptReason::AgencyDup);
    }
    if RE_OK.is_match(&m) {
        return done(true, AcceptReason::Ok);
    }
    if RE_TAKEN.is_match(&m) {
        return done(false, AcceptReason::Taken);
    }
    if RE_TRANSIENT.is_match(&m) {
        return done(false, AcceptReason::Transient);
    }
    done(false, AcceptReason::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The 8 REAL cases, verbatim from spx-accept.test.ts (the only recorded SPX
    // message corpus that exists — see the design doc's fixture-gap note).
    #[test]
    fn eight_real_cases() {
        let c = classify_accept_response(0, false, "success");
        assert_eq!(c.reason, AcceptReason::Ok);
        assert!(c.success);

        let c = classify_accept_response(
            150399,
            false,
            "Operation failed. Your agency already accepted this request before.",
        );
        assert_eq!(c.reason, AcceptReason::AgencyDup);
        assert!(c.success); // terminal — must not retry

        assert_eq!(
            classify_accept_response(1, false, "Request already accepted by you").reason,
            AcceptReason::Ok
        );
        assert_eq!(
            classify_accept_response(1, false, "duplicate request").reason,
            AcceptReason::Ok
        );

        let c = classify_accept_response(1, false, "This booking has been taken by another agency");
        assert_eq!(c.reason, AcceptReason::Taken);
        assert!(!c.success);

        assert_eq!(
            classify_accept_response(1, false, "The booking request has expired").reason,
            AcceptReason::Taken
        );
        assert_eq!(
            classify_accept_response(1, false, "too many requests, try again").reason,
            AcceptReason::Transient
        );
        assert_eq!(
            classify_accept_response(1, false, "sesuatu yang aneh").reason,
            AcceptReason::Error
        );
    }

    // Regression: if the idempotent-ok check ran first, "already accepted" would
    // swallow an agency-dup loss as a self-win. agency_dup MUST win.
    #[test]
    fn agency_dup_checked_before_ok() {
        let c = classify_accept_response(150399, false, "Your agency already accepted this request before");
        assert_eq!(c.reason, AcceptReason::AgencyDup, "must NOT be misclassified as Ok");
        assert!(c.success);
    }
}
```

Add `pub mod accept;` to `lib.rs`.

- [x] **Step 3: Test + clippy + commit**

```bash
cd Backend
cargo test -p spx-client --lib accept
cargo clippy -p spx-client --all-targets -- -D warnings
cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): classify_accept_response — 6-category retcode classifier + 8 real cases"
```

Expected: both tests pass (all 8 cases + the ordering regression).

---

### Task 8: `SpxCookies` + cookie-string + header building (wreq / Chrome preset)

**Files:**
- Modify: `Backend/crates/spx-client/Cargo.toml` (add `wreq`; `wreq-util` per the license decision below)
- Create: `Backend/crates/spx-client/src/cookies.rs`
- Modify: `Backend/crates/spx-client/src/lib.rs` (`pub mod cookies;`)

**Interfaces:**
- Produces: `SpxCookies` (11 fields, mirror of the reference), `pub fn build_cookie_string(c: &SpxCookies) -> String`, `pub fn build_headers(c: &SpxCookies, base_url: &str) -> wreq::header::HeaderMap` (Task 9's `SpxClient` passes its `base_url`).

**License decision — READ FIRST (this is the `wreq-util` landmine from Global Constraints):**
The design doc names `rquest`, which is **no longer published** (renamed to `wreq`, Apache-2.0). Browser-impersonation presets live in `wreq-util`. **Its stable line (2.2.6) is GPL-3.0 and WILL fail `cargo deny`.** Pick ONE:
- **Path A (recommended — real JA3 preset, license-clean):** pin the Apache-2.0 pre-release line: `cargo add --package spx-client wreq --dry-run` first to see the current stable, then add `wreq` + `wreq-util` pinned to the Apache-2.0 pre-release (e.g. `wreq-util = "=3.0.0-rc.14"` with its matching `wreq` rc). Before committing, run `cargo deny check` and confirm both resolve to Apache-2.0. Highest available Chrome preset is `Emulation::Chrome137` — use it (design wants 148; 137 is the closest available; document as best-effort).
- **Path B (fallback — no pre-release deps):** add `wreq` (stable 5.3.0, Apache-2.0) ONLY, skip `wreq-util`, and spoof Chrome via manual headers (below) with wreq's default TLS. This drops the JA3 preset (matching what the reference actually did — static headers only) and is fully license-clean. Choose this if the controller forbids pre-release deps in a security-adjacent crate.

Either way: **do NOT add `wreq-util` 2.x (GPL) and do NOT add GPL-3.0 to `deny.toml`.** State which path you took in your report.

**API-confidence note:** `wreq`/`wreq-util` are large, fast-moving crates and were NOT compile-verified here (BoringSSL build + pre-release churn). The confirmed API surface (from docs.rs) is: `wreq::Client::builder()` → `ClientBuilder`; `ClientBuilder::emulation<P: EmulationProviderFactory>(self, factory)` (Path A); `ClientBuilder::default_headers(HeaderMap)`, `.cookie_store(bool)`, `.timeout(Duration)`, `.build() -> Result<Client>`; requests via `client.post(url).body(...).header(...).send().await` then `res.json().await`; `wreq_util::Emulation::Chrome137` implements `EmulationProviderFactory`. **Verify these signatures against the installed version before proceeding** — if `emulation`/`Emulation` differ, adjust to the installed API; the header/cookie logic below does not depend on wreq internals and is safe.

- [x] **Step 1: Add `wreq` (and `wreq-util` per your chosen path)**

```bash
cd Backend
cargo add --package spx-client wreq --features json   # Path A: also add the pinned Apache-2.0 wreq-util pre-release
cd ..
```

For Path A additionally add the pinned `wreq-util` (Apache-2.0 pre-release) and re-run `cargo deny check` immediately to confirm licenses. For Path B, stop after `wreq`.

- [x] **Step 2: Write `cookies.rs`**

The header names/values below are copied from the reference (`spx.ts:52-76`), with the client-hints set to match **whatever Chrome preset you actually use** (137 for Path A; keep them consistent for Path B too — do not claim 148).

```rust
// Backend/crates/spx-client/src/cookies.rs
//! SPX cookie jar + request headers. SPX auth is cookie-based (not bearer). The
//! header set mirrors the reference (spx.ts:52-76); client-hints are pinned to
//! the Chrome version actually used by the wreq emulation preset (Chrome 137 —
//! the closest available to the reference's Chrome 148; best-effort, refresh as
//! wreq-util adds newer presets).
use wreq::header::{HeaderMap, HeaderName, HeaderValue};

/// Chrome version whose UA + client-hints we emit. Keep in lockstep with the
/// wreq-util `Emulation::ChromeNNN` preset chosen in the client (Task 9).
pub const CHROME_MAJOR: u32 = 137;

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
        assert!(h.get("user-agent").unwrap().to_str().unwrap().contains("Chrome/137"));
        assert!(h.get("cookie").unwrap().to_str().unwrap().contains("fms_user_skey=SKEY"));
    }

    #[test]
    fn csrf_and_device_omitted_when_empty() {
        let h = build_headers(&SpxCookies::default(), "https://x");
        assert!(h.get("x-csrftoken").is_none());
        assert!(h.get("device-id").is_none());
    }
}
```

Add `pub mod cookies;` to `lib.rs`.

- [x] **Step 3: Test + clippy + deny + commit**

```bash
cd Backend
cargo test -p spx-client --lib cookies
cargo clippy -p spx-client --all-targets -- -D warnings
cargo deny check   # MUST stay clean — this is where a GPL wreq-util would surface
cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): SpxCookies + cookie/header building (wreq, Chrome-137 client-hints)"
```

Expected: cookie/header tests pass; `cargo deny check` clean (if it flags a GPL `wreq-util`, you are on the wrong version — switch to Path A's Apache-2.0 pin or Path B). Note in your report which license path you took and which Chrome preset is available.

---

### Task 9: `SpxClient` HTTP methods for all SPX endpoints

**Files:**
- Create: `Backend/crates/spx-client/src/client.rs`
- Modify: `Backend/crates/spx-client/src/lib.rs` (`pub mod client;` + top-level re-exports)
- Modify: `Backend/crates/spx-client/Cargo.toml` (dev-dep: `wiremock`)

**Interfaces:**
- Consumes: `SpxCookies`/`build_headers` (Task 8), `normalize_booking`/`SpxBooking` (Task 6), `classify_accept_response`/`AcceptResult` (Task 7).
- Produces: `SpxClient` with a method per confirmed endpoint. **Endpoint paths are the real reference defaults** (`env.ts`), hardcoded as consts.

**Confirmed endpoint paths (from the reference `env.ts` + spx.ts):**
| Purpose | Method | Path |
|---|---|---|
| bidding list | POST | `/api/line_haul/agency/booking/bidding/list` |
| count_v2 | POST | `/api/line_haul/agency/booking/bidding/count_v2` |
| request/list | POST | `/api/line_haul/agency/booking/bidding/request/list` |
| accept | POST | `/api/line_haul/agency/booking/bidding/accept` |
| notification count | POST | `/api/basicserver/agency/notification/pn/pending/read/count` |
| bidding log/list (acceptor) | GET | `/api/line_haul/agency/booking/bidding/log/list` |
| agency user/list | POST | `/api/basicserver/agency/account/user/list` |
| profile (primary of 6 fallbacks) | GET | `/api/basicserver/agency/account/current_user/basic_info` |
| booking_overview (fallback) | POST | `/api/line_haul/agency/booking/bidding/booking_overview` |
| booking_log (probe) | POST | `/api/line_haul/agency/booking/request/booking_log` |

**API-confidence note (repeat):** `wreq`'s request/response API was not compile-verified here. Build against the confirmed surface (`Client::builder()...build()`, `client.post(url).json(&body).headers(hmap).send().await?`, `res.status()`, `res.json::<serde_json::Value>().await?`) and **verify method names against the installed `wreq` version before proceeding.** If `wreq` exposes `headers(HeaderMap)` vs per-header `.header()`, adjust; the design does not hinge on which.

- [x] **Step 1: Write `client.rs`**

Keep the surface focused: a constructor that builds the emulating client, plus one thin async method per endpoint. The HTTP-status → `Auth`/`Transient` short-circuit for `accept` (before body classification) is ported from `acceptBooking` (spx.ts:978-984).

```rust
// Backend/crates/spx-client/src/client.rs
//! SPX HTTP client. Cookie-based auth; Chrome-impersonating transport (wreq).
//! Endpoint paths are the reference's real defaults. NOTE: wreq's exact
//! request/response method names must be verified against the installed version
//! (see Task 9 header) — the paths, bodies, and classification are the stable part.
use std::time::Duration;

use serde_json::{json, Value};

use crate::accept::{classify_accept_response, AcceptReason, AcceptResult};
use crate::booking::{normalize_booking, SpxBooking};
use crate::cookies::{build_headers, SpxCookies};

pub const PATH_BIDDING_LIST: &str = "/api/line_haul/agency/booking/bidding/list";
pub const PATH_COUNT_V2: &str = "/api/line_haul/agency/booking/bidding/count_v2";
pub const PATH_REQUEST_LIST: &str = "/api/line_haul/agency/booking/bidding/request/list";
pub const PATH_ACCEPT: &str = "/api/line_haul/agency/booking/bidding/accept";
pub const PATH_NOTIFICATION: &str =
    "/api/basicserver/agency/notification/pn/pending/read/count";
pub const PATH_BIDDING_LOG_LIST: &str = "/api/line_haul/agency/booking/bidding/log/list";
pub const PATH_USER_LIST: &str = "/api/basicserver/agency/account/user/list";
pub const PATH_PROFILE: &str = "/api/basicserver/agency/account/current_user/basic_info";
pub const PATH_BOOKING_OVERVIEW: &str =
    "/api/line_haul/agency/booking/bidding/booking_overview";
pub const PATH_BOOKING_LOG: &str = "/api/line_haul/agency/booking/request/booking_log";

#[derive(Debug, thiserror::Error)]
pub enum SpxError {
    #[error("http transport error")]
    Transport,
    #[error("http status {0}")]
    Status(u16),
    #[error("bad response body")]
    Body,
}

pub struct SpxClient {
    http: wreq::Client,
    base_url: String,
}

impl SpxClient {
    /// Build the client. `base_url` is the SPX origin. The Chrome emulation
    /// preset (Path A) must match `cookies::CHROME_MAJOR`'s client-hints
    /// (Chrome 137). For Path B (no wreq-util), drop the `.emulation(...)` call.
    pub fn new(base_url: impl Into<String>) -> Result<Self, SpxError> {
        let http = wreq::Client::builder()
            // Path A only — verify against installed wreq-util:
            .emulation(wreq_util::Emulation::Chrome137)
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| SpxError::Transport)?;
        Ok(SpxClient { http, base_url: base_url.into() })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// POST a JSON body with SPX headers; return the parsed JSON on 2xx.
    async fn post_json(&self, cookies: &SpxCookies, path: &str, body: Value) -> Result<Value, SpxError> {
        let res = self
            .http
            .post(self.url(path))
            .headers(build_headers(cookies, &self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|_| SpxError::Transport)?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SpxError::Status(status));
        }
        res.json::<Value>().await.map_err(|_| SpxError::Body)
    }

    async fn get_json(&self, cookies: &SpxCookies, path_with_query: &str) -> Result<Value, SpxError> {
        let res = self
            .http
            .get(self.url(path_with_query))
            .headers(build_headers(cookies, &self.base_url))
            .send()
            .await
            .map_err(|_| SpxError::Transport)?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(SpxError::Status(status));
        }
        res.json::<Value>().await.map_err(|_| SpxError::Body)
    }

    /// bidding/list — returns normalized bookings from `data.list`/`data.booking_list`.
    pub async fn fetch_bookings(&self, cookies: &SpxCookies, pageno: u32, count: u32) -> Result<Vec<SpxBooking>, SpxError> {
        let seven_days = (chrono::Utc::now().timestamp()) - 7 * 24 * 60 * 60;
        let body = json!({
            "pageno": pageno,
            "count": count,
            "request_tab_all": true,
            "request_ctime_start": seven_days,
        });
        let json = self.post_json(cookies, PATH_BIDDING_LIST, body).await?;
        Ok(extract_booking_list(&json).iter().map(normalize_booking).collect())
    }

    /// count_v2 — raw counts map (`data`).
    pub async fn fetch_booking_counts(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        let json = self.post_json(cookies, PATH_COUNT_V2, json!({ "request_tab_all": true })).await?;
        Ok(json.get("data").cloned().unwrap_or(Value::Null))
    }

    /// request/list — enrichment rows for one booking (`booking_id` MUST be numeric).
    pub async fn fetch_request_list(&self, cookies: &SpxCookies, booking_id: i64, count: u32) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_REQUEST_LIST, json!({ "booking_id": booking_id, "pageno": 1, "count": count })).await
    }

    /// accept — HTTP-status short-circuit (auth/transient) BEFORE body classification.
    pub async fn accept_booking(&self, cookies: &SpxCookies, booking_id: i64, agency_id: i64, request_ids: &[i64]) -> AcceptResult {
        if agency_id <= 0 {
            return AcceptResult { success: false, reason: AcceptReason::Auth, retcode: -1, message: "agency_id kosong".into() };
        }
        let mut body = json!({ "booking_id": booking_id, "agency_id": agency_id });
        if !request_ids.is_empty() {
            body["request_id_list"] = json!(request_ids);
        }
        let res = self
            .http
            .post(self.url(PATH_ACCEPT))
            .headers(build_headers(cookies, &self.base_url))
            .json(&body)
            .send()
            .await;
        let res = match res {
            Ok(r) => r,
            Err(_) => return AcceptResult { success: false, reason: AcceptReason::Transient, retcode: -1, message: "transport".into() },
        };
        let status = res.status().as_u16();
        if status == 401 || status == 403 {
            return AcceptResult { success: false, reason: AcceptReason::Auth, retcode: -1, message: format!("HTTP {status}") };
        }
        if status == 429 || status >= 500 {
            return AcceptResult { success: false, reason: AcceptReason::Transient, retcode: -1, message: format!("HTTP {status}") };
        }
        let body: Value = res.json().await.unwrap_or_else(|_| json!({}));
        let retcode = body.get("retcode").or_else(|| body.get("code")).and_then(Value::as_i64).unwrap_or(-1);
        let raw_msg = body.get("message").or_else(|| body.get("msg")).and_then(Value::as_str).unwrap_or("").to_string();
        let json_success = body.get("success").and_then(Value::as_bool).unwrap_or(false);
        classify_accept_response(retcode, json_success, &raw_msg)
    }

    /// notification pending count.
    pub async fn notification_count(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_NOTIFICATION, json!({ "use_case": "agency portal", "user_type": 4, "notification_type_list": [30] })).await
    }

    /// bidding log/list (GET) — the acceptor op-log.
    pub async fn fetch_bidding_log(&self, cookies: &SpxCookies, booking_id: i64) -> Result<Value, SpxError> {
        let path = format!("{PATH_BIDDING_LOG_LIST}?booking_id={booking_id}&pageno=1&count=30");
        self.get_json(cookies, &path).await
    }

    /// agency account user/list (requires request_source:1 + agency_id).
    pub async fn fetch_agency_users(&self, cookies: &SpxCookies, agency_id: i64) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_USER_LIST, json!({ "request_source": 1, "agency_id": agency_id, "pageno": 1, "count": 100 })).await
    }

    /// profile (GET) — primary of the reference's 6 fallbacks.
    pub async fn fetch_profile(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        self.get_json(cookies, PATH_PROFILE).await
    }

    /// booking_overview (POST) — fallback booking source.
    pub async fn fetch_booking_overview(&self, cookies: &SpxCookies) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_BOOKING_OVERVIEW, json!({ "pageno": 1, "count": 100, "request_acceptance_status": 1, "request_tab_all": true })).await
    }

    /// booking_log (POST) — acceptor probe.
    pub async fn fetch_booking_log(&self, cookies: &SpxCookies, booking_id: i64) -> Result<Value, SpxError> {
        self.post_json(cookies, PATH_BOOKING_LOG, json!({ "booking_id": booking_id, "pageno": 1, "count": 20 })).await
    }
}

/// `data.list` else `data.booking_list` else `[]` (spx.ts fetchBookings).
fn extract_booking_list(json: &Value) -> Vec<Value> {
    let data = json.get("data").unwrap_or(json);
    if let Some(list) = data.get("list").and_then(Value::as_array) {
        return list.clone();
    }
    if let Some(list) = data.get("booking_list").and_then(Value::as_array) {
        return list.clone();
    }
    Vec::new()
}

#[cfg(test)]
mod extract_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_prefers_list_then_booking_list() {
        let a = json!({ "data": { "list": [{ "booking_id": "1" }] } });
        assert_eq!(extract_booking_list(&a).len(), 1);
        let b = json!({ "data": { "booking_list": [{ "booking_id": "1" }, { "booking_id": "2" }] } });
        assert_eq!(extract_booking_list(&b).len(), 2);
        let c = json!({ "data": {} });
        assert_eq!(extract_booking_list(&c).len(), 0);
    }
}
```

Add to `lib.rs`:

```rust
pub mod accept;
pub mod booking;
pub mod client;
pub mod cookies;
pub mod crypto;
pub mod waha_settings;

pub use accept::{classify_accept_response, AcceptReason, AcceptResult};
pub use booking::{normalize_booking, to_core_booking, SpxBooking};
pub use client::SpxClient;
pub use cookies::{build_cookie_string, build_headers, SpxCookies};
```

- [x] **Step 2: Add `wiremock` dev-dep and write a request-construction test**

`wiremock` (MIT, `cargo add --dry-run` → 0.6.5 — in the allow-list) spins a real localhost HTTP server; `SpxClient` (pointed at its `http://127.0.0.1:PORT` base) hits it, letting us assert method/path/body without a real SPX server. For Path B (no emulation), this works as-is over plain HTTP; for Path A, verify wreq's emulated client will talk plain HTTP to a localhost mock (it does — emulation affects TLS/HTTP2 fingerprint, not plaintext HTTP). If wreq's emulation rejects plaintext localhost, gate the mock test behind Path B's plain client or construct the client without `.emulation()` in tests.

```bash
cd Backend && cargo add --package spx-client --dev wiremock && cargo add --package spx-client --dev tokio --features rt-multi-thread,macros && cd ..
```

```rust
// Backend/crates/spx-client/tests/client_requests.rs
//! Request-construction tests against a wiremock server (no real SPX).
use spx_client::client::{SpxClient, PATH_ACCEPT, PATH_BIDDING_LIST};
use spx_client::cookies::SpxCookies;
use wiremock::matchers::{method, path, header};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { fms_user_agency_id: "42".into(), csrftoken: "CSRF".into(), ..Default::default() }
}

#[tokio::test]
async fn bidding_list_posts_to_correct_path_with_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_BIDDING_LIST))
        .and(header("x-csrftoken", "CSRF"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": { "list": [{ "booking_id": "B1", "booking_name": "SPXID1" }] }
        })))
        .mount(&server)
        .await;

    // For Path A, construct without emulation in tests if needed (see note above).
    let client = SpxClient::new(server.uri()).expect("client");
    let bookings = client.fetch_bookings(&cookies(), 1, 50).await.expect("fetch");
    assert_eq!(bookings.len(), 1);
    assert_eq!(bookings[0].booking_id, "B1");
}

#[tokio::test]
async fn accept_classifies_agency_dup_from_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_ACCEPT))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 150399,
            "message": "Operation failed. Your agency already accepted this request before."
        })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 42, &[]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::AgencyDup);
    assert!(r.success);
}

#[tokio::test]
async fn accept_maps_401_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_ACCEPT))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 42, &[]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::Auth);
}
```

- [x] **Step 3: Test + clippy + deny + commit**

```bash
cd Backend
cargo test -p spx-client
cargo clippy -p spx-client --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/crates/spx-client Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(spx-client): SpxClient HTTP methods for all SPX endpoints + wiremock request tests"
```

Expected: the `extract_booking_list` unit test + the three wiremock request-construction tests pass. If the wreq `emulation`/request API differs from the shape above, reconcile against the installed version (this is the flagged, non-compile-verified surface) — do not change the endpoint paths, bodies, or the accept classification, which are the load-bearing, verified parts.

---

### Task 10: Final verification + Fase 3 sign-off

**Files:** None created — this task runs verification commands and checks off the plan.

**Interfaces:**
- Consumes: everything from Tasks 1-9.
- Produces: recorded evidence the Fase 3 Definition of Done (design doc) is met.

- [x] **Step 1: Full crate test suite from a clean database**

```bash
cd Docker && docker compose up -d tower-postgres && cd ..
# wait for healthy: docker compose -f Docker/docker-compose.yml ps
cd Backend && cargo test -p spx-client -- --test-threads=1 && cd ..
```

Expected: every `spx-client` test passes — the crypto unit tests, password/session tests, booking/accept tests, cookie/header tests, `extract_booking_list`, the wiremock request tests, the master-key loader tests, and the two Postgres round-trips (agency credential + WAHA `site_settings`).

- [x] **Step 2: Full workspace build/test/clippy**

```bash
cd Backend
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cd ..
```

Expected: all clean — `core-domain`'s existing tests, `store`'s suite, `spx-client`'s full suite, and the other crates' runs, all green; clippy clean workspace-wide.

- [x] **Step 3: `cargo deny check` — licenses stay clean (the `wreq-util` gate)**

```bash
cd Backend && cargo deny check && cd ..
```

Expected: `advisories ok, bans ok, licenses ok, sources ok`. If licenses fail, it is almost certainly a GPL-3.0 `wreq-util` (Task 8) — you are on the wrong version; fix per Task 8's license decision (Apache-2.0 pin or Path B), do NOT add GPL-3.0 to `deny.toml`. Record the resolved `wreq`/`wreq-util` versions + licenses in your report.

- [x] **Step 4: Confirm `spx-client`'s production dep footprint is intentional**

```bash
cd Backend && cargo tree -p spx-client --edges normal && cd ..
```

Expected: `wreq` (+ `wreq-util` if Path A), `aes-gcm`, `hkdf`, `sha2`, `argon2`, `secrecy`, `zeroize`, `getrandom`, `base64`, `thiserror`, `regex`, `serde`, `serde_json`, `chrono`, `uuid`, `core-domain`. Confirm `store`/`sqlx`/`tokio`/`wiremock` appear ONLY under dev-dependencies (`cargo tree -p spx-client --edges dev` to see them) — production `spx-client` must NOT depend on `store`. Also confirm no stray `rand` (randomness is via `getrandom`).

- [x] **Step 5: Cross-check every DoD item in the design doc**

Read `Docs/superpowers/specs/2026-07-13-fase-3-spx-client-crypto-design.md`'s "Definition of Done — Fase 3" (9 items) and cite the concrete evidence for each — do not just assert:
1. `normalize_booking` 29-field + `to_core_booking` → `booking.rs` + its tests (Task 6).
2. `classify_accept_response` 8 real cases + agency_dup-before-ok regression → `accept.rs` tests (Task 7).
3. Envelope round-trip + purpose-scoped subkeys byte-distinct → `envelope.rs` `roundtrip_with_aad` + `purpose_subkeys_are_distinct_bytes` (Task 1).
4. Three gaps closed via negative tests: (a) no pad/slice — `subkey_is_full_32_bytes` + `load_from_file` rejects wrong length (Tasks 1, 5); (b) AES subkey != HMAC subkey — `purpose_subkeys_are_distinct_bytes` (Task 1); (c) `site_settings` no plaintext — `waha_key_encrypted_in_site_settings_jsonb`'s `value::text` assertion (Task 4).
5. `cargo test`/`clippy`/`deny` clean → Steps 1-3 output.
6. `SecretString`/zeroize consistent, no plaintext-String leak path → code review: master key, subkeys, decrypted password, session token are all `SecretBox`/`SecretString`; `MasterKey` Debug redacted; file buffer zeroized. Cite the types.
7. argon2id hash/verify + salt-random → `password.rs` `hash_verify_roundtrip` + `same_password_hashes_differ` (Task 2).
8. Session token 256-bit, only SHA-256 hash to DB → `session_token.rs` tests + the `portal_sessions.token_hash` usage note (Task 2).
9. Fixture limitation documented in code → the `NOTE (DoD #9)` comment in `booking.rs` tests + the `spx-accept.test.ts`-verbatim comment in `accept.rs` (Tasks 6, 7).

- [x] **Step 6: Mark this plan complete**

Check every remaining `- [ ]` box in this file to `- [x]` by hand or with a targeted script — verify afterward (grep) that no non-checkbox prose containing the literal `- [ ]` substring got corrupted (this exact mistake happened during Fase 0's sign-off and was caught during Fase 1's; do not repeat it a third time).

- [x] **Step 7: Commit**

```bash
git add Backend Docs/superpowers/plans/2026-07-13-fase-3-spx-client-crypto.md
git commit -m "test(spx-client): Fase 3 sign-off — full verification + DoD cross-check"
```

Fase 3 is done once this commits clean. Fase 4 (executor — 3-layer dedup) is the next master-spec phase — do not start it in this same task; it gets its own spec/plan cycle.
