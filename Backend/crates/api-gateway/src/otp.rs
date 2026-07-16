// Backend/crates/api-gateway/src/otp.rs
//! Redis-backed OTP state for the auto-accept arm gate (`request-aa-otp` /
//! `verify-aa-otp`, Task 5). Pure Redis logic — no HTTP, no WAHA delivery
//! (the caller sends the code; this module only manages its lifecycle).
//! Keyed by `portal_user_id` (globally unique) with a `tenant_id` prefix
//! for operational grep-ability, not collision-safety.
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};
use uuid::Uuid;

const CODE_TTL_SECS: u64 = 180;
const RESEND_COOLDOWN_SECS: u64 = 60;
const MAX_ATTEMPTS: u64 = 5;
const ATTEMPT_WINDOW_SECS: i64 = 180;
const PWVERIFY_TTL_SECS: u64 = 120;

fn code_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:aa_otp:{tenant_id}:{user_id}")
}
fn cooldown_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:aa_otp_rl:{tenant_id}:{user_id}")
}
fn attempts_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:aa_otp_att:{tenant_id}:{user_id}")
}
pub(crate) fn pwverify_key(tenant_id: Uuid, user_id: Uuid) -> String {
    format!("spx:pwverify:{tenant_id}:{user_id}")
}

#[derive(Debug)]
pub enum OtpRequestError {
    TooSoon,
    Redis(redis::RedisError),
}
impl From<redis::RedisError> for OtpRequestError {
    fn from(e: redis::RedisError) -> Self {
        OtpRequestError::Redis(e)
    }
}

#[derive(Debug)]
pub enum OtpVerifyError {
    NoActiveCode,
    WrongCode,
    TooManyAttempts,
    Redis(redis::RedisError),
}
impl From<redis::RedisError> for OtpVerifyError {
    fn from(e: redis::RedisError) -> Self {
        OtpVerifyError::Redis(e)
    }
}

// Route handlers (Task 5) map these onto `ApiError` manually rather than via
// a `From` impl here: `OtpRequestError::TooSoon` is a 429, `OtpVerifyError`'s
// `WrongCode`/`NoActiveCode` are 401s, `TooManyAttempts` is a 429 with a
// different message than `TooSoon`, etc — a blanket `From` impl can only
// produce ONE `ApiError` variant per source type, which would collapse this
// necessary per-variant status-code distinction into a single branch. Only
// the `redis::RedisError` sub-case is uniform (always an unexpected 500), so
// that alone gets the small `From<redis::RedisError>` impls above; the
// route handler's own `match` on the full `OtpRequestError`/`OtpVerifyError`
// is the cleaner place for the rest.

/// Generates a 6-digit numeric OTP code (uniform over `000000..=999999`).
/// `getrandom` (not the `rand` crate) matches this workspace's established
/// randomness precedent — see `spx_client::crypto::{password,envelope,
/// session_token}` and `spx_client::waha_settings`, all of which call
/// `getrandom::fill` directly rather than pulling in a second RNG
/// dependency. A 6-digit code needs a value in `0..=999_999` — reducing a
/// random `u32` modulo `1_000_000` has a tiny modulo bias (the top ~294
/// million of `u32::MAX`'s ~4.29 billion values are ever so slightly
/// over-represented), immaterial for a 180s-lived, 5-attempt,
/// non-cryptographic-secret OTP code — do not over-engineer this with
/// rejection sampling.
fn generate_code() -> String {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("getrandom for OTP code");
    let n = u32::from_le_bytes(buf) % 1_000_000;
    format!("{n:06}")
}

/// Requests a new OTP code for `(tenant_id, user_id)`. Fails closed with
/// `TooSoon` if a resend was already issued within `RESEND_COOLDOWN_SECS`
/// (the cooldown key is claimed with `SET NX EX` — atomic acquire, no
/// read-then-write race between concurrent requests for the same user).
/// Stores the fresh code with `EX CODE_TTL_SECS` and resets the attempt
/// counter (a fresh code gets a fresh attempt budget). Returns the code —
/// the CALLER (Task 5's route handler) sends it via WAHA; this module never
/// touches HTTP.
pub async fn request(
    redis: &mut redis::aio::ConnectionManager,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<String, OtpRequestError> {
    let cooldown = cooldown_key(tenant_id, user_id);
    let opts = SetOptions::default()
        .with_expiration(SetExpiry::EX(RESEND_COOLDOWN_SECS))
        .conditional_set(ExistenceCheck::NX);
    // `SET key val NX EX secs` returns "OK" (-> `Value::Okay`, `bool` true)
    // when the key was set, or Nil (-> `bool` false) when NX blocked it
    // because the key already exists — verified against `redis` 1.3.0's own
    // `FromRedisValue for bool` impl (`~/.cargo/registry/.../redis-1.3.0/
    // src/types.rs`), not assumed.
    let acquired: bool = redis.set_options(&cooldown, "1", opts).await?;
    if !acquired {
        return Err(OtpRequestError::TooSoon);
    }
    let code = generate_code();
    let _: () = redis
        .set_ex(code_key(tenant_id, user_id), &code, CODE_TTL_SECS)
        .await?;
    let _: () = redis.del(attempts_key(tenant_id, user_id)).await?; // fresh code, fresh attempt budget
    Ok(code)
}

/// Verifies `submitted_code` against the stored code for `(tenant_id,
/// user_id)`. Increments the attempt counter FIRST (before comparing) and
/// rejects with `TooManyAttempts` once the counter exceeds `MAX_ATTEMPTS` —
/// this order means a 6th attempt is rejected on attempt-count alone, even
/// if it happens to submit the right code. On success, the code and attempt
/// counter are deleted (single-use) and a short-lived
/// `spx:pwverify:<tenant>:<user>` proof key is written for the caller's next
/// step (Task 5's arm-with-password flow) to consume.
pub async fn verify(
    redis: &mut redis::aio::ConnectionManager,
    tenant_id: Uuid,
    user_id: Uuid,
    submitted_code: &str,
) -> Result<(), OtpVerifyError> {
    let attempts_k = attempts_key(tenant_id, user_id);
    let attempts: u64 = redis.incr(&attempts_k, 1).await?;
    if attempts == 1 {
        let _: () = redis.expire(&attempts_k, ATTEMPT_WINDOW_SECS).await?;
    }
    if attempts > MAX_ATTEMPTS {
        return Err(OtpVerifyError::TooManyAttempts);
    }

    let stored: Option<String> = redis.get(code_key(tenant_id, user_id)).await?;
    let Some(stored) = stored else {
        return Err(OtpVerifyError::NoActiveCode);
    };
    // A 6-digit numeric OTP has vastly lower entropy than a password
    // (1,000,000 possibilities vs. argon2id's much-higher-entropy,
    // much-longer-lived secret), and the 5-attempt cap enforced above —
    // not comparison timing — is THIS code's primary defense: even a
    // perfectly-informative timing side-channel only saves an attacker
    // attempts they don't have (max 5 total, ever, per code). Unlike
    // `spx_client::crypto::password::verify_password` (argon2's own
    // constant-time comparator, defending a secret with no attempt cap and
    // a much longer effective lifetime), a plain same-length string `==`
    // here is an accepted, disclosed choice — deliberately NOT
    // "upgraded" to a constant-time comparison, which would be
    // over-engineering against an already-dominant defense.
    if stored != submitted_code {
        return Err(OtpVerifyError::WrongCode);
    }

    let _: () = redis.del(code_key(tenant_id, user_id)).await?;
    let _: () = redis.del(&attempts_k).await?;
    let _: () = redis
        .set_ex(pwverify_key(tenant_id, user_id), "1", PWVERIFY_TTL_SECS)
        .await?;
    Ok(())
}
