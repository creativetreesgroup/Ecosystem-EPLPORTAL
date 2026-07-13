//! Re-exports of the `secrecy` primitives used across the crypto module.
//!
//! Every plaintext secret in memory MUST be carried in one of these so that
//! `Debug`/`Display` are redacted and the memory is zeroized on drop (secrecy
//! depends on `zeroize`; `SecretBox<S: Zeroize>` implements `ZeroizeOnDrop`).
pub use secrecy::{ExposeSecret, SecretBox, SecretString};

/// A 32-byte key (master key or HKDF subkey) held in zeroize-on-drop memory.
pub type SecretKeyBytes = SecretBox<[u8; 32]>;
