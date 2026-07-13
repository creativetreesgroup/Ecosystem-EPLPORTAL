pub mod envelope;
pub mod password;
pub mod secret;
pub mod session_token;

// NB: `derive_subkey` is `pub(crate)` (used only by envelope.rs's own tests) —
// do NOT add it to this `pub use`, re-exporting a pub(crate) item as pub is E0365.
pub use envelope::{
    aad_for, decrypt, encrypt, Ciphertext, CryptoError, MasterKey, KEY_VERSION,
    LABEL_AGENCY_CREDENTIAL, LABEL_QUICK_ACCEPT_HMAC, LABEL_WAHA_KEY,
};
pub use password::{hash_password, verify_password};
pub use secret::{ExposeSecret, SecretBox, SecretKeyBytes, SecretString};
pub use session_token::{generate_session_token, hash_session_token};
