# Fase 6e (quick-accept HMAC + hardening) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the fifth and final api-gateway sub-phase of Fase 6: the login-free "quick accept" flow reached from a WhatsApp notification link (`sign_quick_token`/`verify_quick_token` HMAC tokens + the Redis short-code flow), both mounted OUTSIDE `session_auth` — then close out Fase 6 entirely with a workspace-wide Definition-of-Done sign-off covering all five sub-phases (6a-6e).

**Architecture:** A new `spx_client::crypto::quick_token` module (HMAC-SHA256 over a base64url JSON payload, using the already-reserved `LABEL_QUICK_ACCEPT_HMAC` subkey) provides sign/verify. A new `store::bookings::get_by_spx_id` lookup resolves a booking by its raw SPX platform ID (not the internal UUID row id) — the shape both the HMAC token and the short code carry. `api-gateway::routes::bookings::accept`'s existing manual-accept core (Fase 6c Task 10: `try_claim_manual` → dispatch → DB update) is extracted into a tenant_id-parameterized helper shared by the existing session-gated route AND the two new public routes, so the accept mechanism is written once. A new `routes/quick_accept.rs` serves `GET/POST /q/:token` (HMAC flow) and `GET/POST /accept/:code` (Redis short-code flow, read-only — code *generation* is a disclosed, deliberately out-of-scope Fase 5 gap, see Global Constraints). Both route groups return `Html<String>` on GET (a genuine first for this crate — every other route is JSON) and JSON on POST, and are rate-limited via two new `tower_governor` configs (60/min view, 12/min accept) distinct from the existing 120/min public-read and ~20/min login budgets.

**Tech Stack:** Same as every prior sub-phase (`axum` 0.8, `sqlx` 0.9/Postgres, `redis` 1.3, `tower_governor` 0.8), plus one new direct dependency: `hmac = "0.13"` on `spx-client` (already resolved workspace-wide at this exact version as a transitive dependency of the existing crypto stack — see Task 1's own verification step; adds a dependency-graph edge, not a new `Cargo.lock` version).

## Global Constraints

- Every tenant-scoped Postgres query MUST run inside `store::begin_tenant_tx(pool, tenant_id)`.
- `ApiError` variants unchanged: `Unauthorized | Forbidden | NotFound | Conflict(String) | BadRequest(String) | Internal(String) | TooManyRequests(String)`. The two new GET (page) handlers do NOT go through `ApiError` — they return `(StatusCode, Html<String>)` directly, a disclosed first for this crate (every other handler is JSON), because the reference itself renders an HTML confirmation page here, not a JSON error body, and Fase 7's UI does not exist yet to replace it.
- **Public routes with no session use `state.tenant_id`, matching the established convention** (`routes/prices.rs`'s `GET /prices`, `routes/branding.rs`'s `GET /branding`) — coherent with TOWER's single-tenant-per-instance deployment model, not a new pattern.
- **Quick-accept HMAC tokens are tenant-scoped** (a deliberate TOWER-specific hardening beyond the reference, which is single-tenant and has no such concept): the signed payload embeds `tenant_id`, and `verify_quick_token` requires the caller to supply the SAME `tenant_id` it was signed for. Without this, a token minted under one tenant's `AppState.tenant_id` could theoretically be replayed against a different tenant's deployment if the same `spx_id` existed there — closed by construction, not by convention.
- **Short-code *generation* (the reference's `genAcceptCode`/`webhook.ts`, and wiring `notifier::notify_new_tickets` — built in Fase 5, never called anywhere) is explicitly OUT OF SCOPE for this plan.** This task builds the READ side only (`GET/POST /accept/:code` against whatever the reference's `spx:qa:<code>` key shape would produce), mirroring how `sign_quick_token` itself has no live caller in the reference either — both are parity infrastructure, wiring a real caller is tracked as a Fase-5-owned gap (see Task 7's DoD notes), not silently absorbed into this plan's scope.
- **The `docker compose up` / `app_role` provisioning gap (tracked since Fase 6a) is explicitly OWNED BY FASE 8, not this plan** — confirmed with the user this session. Task 7's DoD cross-check must NOT claim item #4 closed.
- No `regex` dependency for input validation (this crate has none; token/code shape validation uses plain char-class checks, matching `branding.rs`'s `validate_data_uri` precedent).
- Every new file needs a top doc comment; `cargo fmt`/`cargo clippy --workspace --all-targets -- -D warnings`/`cargo test --workspace -- --test-threads=1` must stay clean after every task.
- Apply `ponytail` (YAGNI, reuse before new code, shortest correct diff) while implementing every task — Task 3's whole point is reuse-not-reimplement, and the HTML pages in Tasks 4-5 must stay functionally minimal, not attempt reference CSS-parity (Fase 7 replaces this page's styling entirely; a plain, accessible, correctly-stated confirmation page is the actual requirement).

---

## Task 1: `spx_client::crypto::quick_token` — sign/verify HMAC token

**Files:**
- Create: `Backend/crates/spx-client/src/crypto/quick_token.rs`
- Modify: `Backend/crates/spx-client/src/crypto/mod.rs` (re-export)
- Modify: `Backend/crates/spx-client/Cargo.toml` (`hmac = "0.13"`)

**Interfaces:**
- Consumes: `crate::crypto::envelope::{derive_subkey, MasterKey, LABEL_QUICK_ACCEPT_HMAC}` (`derive_subkey` is `pub(crate)` — this module lives inside `crate::crypto`, same visibility scope `envelope.rs` itself uses).
- Produces (for Task 4): `quick_token::{sign_quick_token, verify_quick_token, QuickTokenClaims}`.

- [ ] **Step 1: Add the `hmac` dependency**

```toml
# Backend/crates/spx-client/Cargo.toml — add to [dependencies], alphabetical position
hmac = "0.13"
```

- [ ] **Step 2: Verify it resolves to the already-present version**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo tree -p spx-client -i hmac 2>&1`
Expected: shows `hmac v0.13.0` (matching the version already present in `Cargo.lock` as a transitive dependency — confirm via `grep -A1 'name = "hmac"' Cargo.lock` shows `version = "0.13.0"` is one of the entries, and that this task's `cargo build` doesn't add a THIRD hmac version). If cargo resolves a different 0.13.x patch or fails to compile against `sha2 = "0.11"`'s `digest` trait version, STOP and report BLOCKED — do not pin a version that silently duplicates the dependency graph.

- [ ] **Step 3: Write the module**

```rust
// Backend/crates/spx-client/src/crypto/quick_token.rs
//! Signed, expiring, single-purpose token embedded in the WhatsApp "Terima cepat" link (mirrors
//! the reference's `lib/quicktoken.ts`). Lets an operator accept ONE specific booking straight
//! from the notification — no portal login. Forgery-proof (HMAC-SHA256 over a purpose-scoped
//! subkey derived from the master key via `LABEL_QUICK_ACCEPT_HMAC`), time-boxed, and — a
//! TOWER-specific hardening beyond the reference (which is single-tenant) — scoped to a single
//! `tenant_id` as well as a single `spx_id`, so a token can never be replayed against a
//! different tenant's booking even if the same `spx_id` string existed there.
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
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
```

- [ ] **Step 4: Re-export from the crypto module**

```rust
// Backend/crates/spx-client/src/crypto/mod.rs — add alongside the existing `pub mod envelope;`
pub mod quick_token;
```

Check the existing `mod.rs` re-export style first (Task 3 of Fase 3 established the pattern: `envelope`'s public items are re-exported at `crate::crypto::{...}` — mirror it exactly for `quick_token`'s `sign_quick_token`/`verify_quick_token`/`QuickTokenClaims`, not just the bare `pub mod`).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spx-client quick_token:: -- --test-threads=1`
Expected: 6 tests passing (`valid_token_round_trips`, `expired_token_is_rejected`, `wrong_tenant_is_rejected`, `tampered_payload_is_rejected`, `malformed_tokens_are_rejected_not_panicking`, `different_master_key_rejects_the_token`).

Run: `cargo test -p spx-client -- --test-threads=1 && cargo clippy -p spx-client --all-targets -- -D warnings`
Expected: 0 failures, clean.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/spx-client/Cargo.toml Backend/crates/spx-client/src/crypto/quick_token.rs \
        Backend/crates/spx-client/src/crypto/mod.rs Backend/Cargo.lock
git commit -m "feat(spx-client): sign_quick_token/verify_quick_token — tenant-scoped HMAC quick-accept tokens"
```

---

## Task 2: `store::bookings::get_by_spx_id`

**Files:**
- Modify: `Backend/crates/store/src/bookings.rs`

**Interfaces:**
- Consumes: `crate::begin_tenant_tx`, existing `models::Booking` / whatever row type `get_detail` (line 316) already returns — reuse that exact type, do not define a new one.
- Produces (for Task 4/5): `bookings::get_by_spx_id(pool, tenant_id, spx_id) -> Result<Option<Booking>, sqlx::Error>`.

- [ ] **Step 1: Read `get_detail` first**

Read `Backend/crates/store/src/bookings.rs`'s existing `get_detail` fn (line 316) in full — this task's new fn is a near-identical sibling querying by `spx_id` instead of `id`, returning the SAME row type. Copy its exact `SELECT` column list and tenant-scoping pattern; do not invent a different shape.

- [ ] **Step 2: Write the function**

```rust
// Backend/crates/store/src/bookings.rs — add near get_detail
/// Looks up a booking by its SPX platform id (`spx_id`) rather than the internal UUID row id —
/// what a quick-accept HMAC token or short code carries (Fase 6e), since neither the WhatsApp
/// notification nor the reference's own token format ever carries TOWER's internal row id.
/// Tenant-scoped like every other lookup in this module; `(tenant_id, account_id, spx_id)` is
/// UNIQUE per migration 0020 but NOT `(tenant_id, spx_id)` alone (the same spx_id can exist
/// under two different accounts within one tenant) — `LIMIT 1` with no `ORDER BY` is
/// intentional: whichever row Postgres returns first is a genuinely correct answer for THIS
/// use case (a WhatsApp link fired for one specific account's ticket at send-time; if a
/// same-spx_id collision across accounts is a real ambiguity for the caller, that's a
/// documented, accepted simplification of the reference's OWN behavior, which has no per-account
/// disambiguation concept at all — see this task's plan Global Constraints).
pub async fn get_by_spx_id(
    pool: &PgPool,
    tenant_id: Uuid,
    spx_id: &str,
) -> Result<Option<BookingDetail>, sqlx::Error> {
    let mut tx = begin_tenant_tx(pool, tenant_id).await?;
    let row = sqlx::query_as::<_, BookingDetail>(
        // SAME column list as get_detail — copy it verbatim from that fn when implementing,
        // do not re-derive; the two queries must return byte-identical shapes.
        "SELECT id, tenant_id, account_id, spx_id, status, is_coc, raw_data, created_at, updated_at \
         FROM bookings WHERE tenant_id = $1 AND spx_id = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}
```

**Note for the implementer:** the exact column list above is a best-effort reconstruction — `get_detail`'s real `SELECT` list (read in Step 1) is the source of truth. Match it exactly, including whatever the real return type is actually named (`BookingDetail` is this plan's guess; use the real name from `get_detail`'s signature).

- [ ] **Step 3: Write the failing test**

```rust
// Backend/crates/store/src/lib.rs — inside #[cfg(test)] mod tests, alongside the existing
// bookings_get_detail_returns_none_for_wrong_tenant test — follow that test's exact setup
// pattern (insert_test_tenant, a direct INSERT into bookings) rather than re-deriving one.
#[tokio::test]
async fn bookings_get_by_spx_id_finds_row_and_isolates_by_tenant() {
    let pool = connect(&test_database_url()).await.expect("connect");
    let tenant_a = insert_test_tenant(&pool).await;
    let tenant_b = insert_test_tenant(&pool).await;

    sqlx::query(
        "INSERT INTO bookings (tenant_id, account_id, spx_id, status, raw_data) \
         VALUES ($1, 'acct-1', 'SPX-QUICK-1', 'pending', '{}'::jsonb)",
    )
    .bind(tenant_a)
    .execute(&pool)
    .await
    .expect("insert booking");

    let found = bookings::get_by_spx_id(&pool, tenant_a, "SPX-QUICK-1")
        .await
        .expect("query ok")
        .expect("row found");
    assert_eq!(found.spx_id, "SPX-QUICK-1");

    // Cross-tenant: same spx_id string, wrong tenant, must return None (tenant_b never wrote it).
    let not_found = bookings::get_by_spx_id(&pool, tenant_b, "SPX-QUICK-1")
        .await
        .expect("query ok");
    assert!(not_found.is_none());

    let missing = bookings::get_by_spx_id(&pool, tenant_a, "SPX-DOES-NOT-EXIST")
        .await
        .expect("query ok");
    assert!(missing.is_none());

    sqlx::query("DELETE FROM tenants WHERE id = $1 OR id = $2")
        .bind(tenant_a)
        .bind(tenant_b)
        .execute(&pool)
        .await
        .ok();
}
```

Adjust the exact `INSERT` columns to match `bookings` table's real required columns (check migration 0003/0020 or an existing test's own `INSERT INTO bookings` statement in this same file for the exact required-column list — several already exist, copy one).

- [ ] **Step 4: Run it, then full store verification**

Run: `cargo test -p store bookings_get_by_spx_id -- --test-threads=1` — PASS.
Run: `cargo test -p store -- --test-threads=1 && cargo clippy -p store --all-targets -- -D warnings` — 0 failures, clean.

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/store/src/bookings.rs Backend/crates/store/src/lib.rs
git commit -m "feat(store): bookings::get_by_spx_id — tenant-scoped lookup by SPX platform id"
```

---

## Task 3: Extract the shared manual-accept core (refactor, no behavior change)

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs`

**Interfaces:**
- Produces (for Task 4/5): a private (or `pub(crate)`) helper `async fn execute_manual_accept(state: &AppState, tenant_id: Uuid, booking: &BookingDetail) -> ManualAcceptResponse` that Task 4/5's handlers call directly — no new `ApiError` conversion needed at this layer, `ManualAcceptResponse` (already `Serialize`) is returned as plain data; the caller decides the HTTP status.

This task is a pure refactor: read the CURRENT `accept()` handler (`routes/bookings.rs`, already summarized in this plan's own research — the `try_claim_manual` → `manual_tx` dispatch → `outcome_for` → DB update sequence) in full, then extract everything from "resolve `handle`/`dedup`/`manual_tx` from `state.poller.accounts`" through "the final DB status update" into a new private async fn that takes `&AppState`, `tenant_id: Uuid`, and `&BookingDetail` (the type `get_detail`/`get_by_spx_id` both return) and returns `ManualAcceptResponse` — no `Result`, no `ApiError`: every failure mode the existing code currently maps to an early `Err(ApiError::...)` return (account not connected, already claimed) must become an OK-typed `ManualAcceptResponse { ok: false, reason: ..., message: ... }` value instead, since Tasks 4/5's callers need uniform response handling regardless of which failure occurred (unlike the existing session-gated route, which is fine using `ApiError`'s automatic status-code mapping).

- [ ] **Step 1: Extract the helper**

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — new private fn, placed above `accept`
/// The manual-accept core shared by the session-gated `POST /bookings/:id/accept` (below) and
/// Fase 6e's public quick-accept routes (`routes/quick_accept.rs`): resolve the owning account's
/// poller handle, claim via `try_claim_manual`, dispatch through the manual-accept channel, map
/// the outcome, and persist the DB status update. Returns `ManualAcceptResponse` directly (never
/// `ApiError`) so every caller — whichever HTTP status convention it uses — gets uniform data to
/// render, not a status code baked in at this layer.
async fn execute_manual_accept(
    state: &AppState,
    tenant_id: Uuid,
    booking: &BookingDetail,
) -> ManualAcceptResponse {
    if booking.status != "pending" {
        return ManualAcceptResponse {
            ok: false,
            reason: "not_pending".to_string(),
            message: format!("booking is not pending (status: {})", booking.status),
        };
    }

    let handle = match state.poller.accounts.get(&booking.account_id) {
        Some(h) => h,
        None => {
            return ManualAcceptResponse {
                ok: false,
                reason: "account_offline".to_string(),
                message: "the account this booking belongs to is not currently connected"
                    .to_string(),
            };
        }
    };
    let (dedup, manual_tx) = (handle.dedup.clone(), handle.manual_accept.clone());
    drop(handle);

    match state
        .poller
        .executor
        .try_claim_manual(&booking.account_id, &booking.spx_id, &dedup)
        .await
    {
        executor::ManualClaimOutcome::AlreadyAccepted => {
            return ManualAcceptResponse {
                ok: false,
                reason: "already_claimed".to_string(),
                message: "booking is already claimed or accepted".to_string(),
            };
        }
        executor::ManualClaimOutcome::Ok => {}
    }

    let spx_booking = spx_client::normalize_booking(&booking.raw_data);
    let booking_id_i64 = spx_booking.booking_id.parse::<i64>().unwrap_or(0);
    let request_ids: Vec<i64> = spx_booking
        .request_id
        .parse::<i64>()
        .ok()
        .into_iter()
        .collect();

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if manual_tx
        .send(poller::ManualAcceptRequest {
            booking_id: booking_id_i64,
            request_ids,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return ManualAcceptResponse {
            ok: false,
            reason: "dispatch_failed".to_string(),
            message: "account task is not accepting manual requests".to_string(),
        };
    }

    let result = match tokio::time::timeout(std::time::Duration::from_secs(15), reply_rx).await {
        Ok(Ok(r)) => r,
        _ => {
            return ManualAcceptResponse {
                ok: false,
                reason: "timeout".to_string(),
                message: "manual accept dispatch timed out".to_string(),
            };
        }
    };

    let outcome = outcome_for(result.reason);

    if matches!(result.reason, spx_client::AcceptReason::Ok) {
        dedup.commit_accept(&booking.spx_id);
        let _ = state
            .poller
            .executor
            .record_durable_accept(&booking.account_id, &booking.spx_id)
            .await;
        let _ = store::update_booking_status(
            &state.poller.pool,
            tenant_id,
            &booking.spx_id,
            store::BookingStatusUpdate {
                status: "accepted",
                latency_ms: None,
                auto_accepted: false,
                rule_matched: None,
                accept_reason: None,
            },
        )
        .await;
        ManualAcceptResponse {
            ok: true,
            reason: outcome.to_string(),
            message: "accepted".to_string(),
        }
    } else {
        state
            .poller
            .executor
            .release_claim_auto(&booking.account_id, &booking.spx_id, None)
            .await;
        dedup.abort_accept(&booking.spx_id);
        let _ = store::update_booking_status(
            &state.poller.pool,
            tenant_id,
            &booking.spx_id,
            store::BookingStatusUpdate {
                status: "failed",
                latency_ms: None,
                auto_accepted: false,
                rule_matched: None,
                accept_reason: Some(outcome),
            },
        )
        .await;
        ManualAcceptResponse {
            ok: false,
            reason: outcome.to_string(),
            message: "accept failed".to_string(),
        }
    }
}
```

Read the CURRENT `accept()` function's exact `store::update_booking_status` calls (both branches) before writing this — the `accept_reason` field on the failure branch, and the exact `dedup`/`executor` method names/argument order, must match byte-for-byte what's already there. This plan's reconstruction above is from this session's own research; verify every line against the real file.

- [ ] **Step 2: Rewrite `accept()` as a thin wrapper**

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — replace the body of `accept`
async fn accept(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<ManualAcceptResponse>, ApiError> {
    let booking = store::bookings::get_detail(&state.poller.pool, user.tenant_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let response = execute_manual_accept(&state, user.tenant_id, &booking).await;

    if !response.ok && response.reason == "not_pending" {
        return Err(ApiError::Conflict(response.message));
    }
    if !response.ok && response.reason == "already_claimed" {
        return Err(ApiError::Conflict(response.message));
    }
    if !response.ok && response.reason == "account_offline" {
        return Err(ApiError::Conflict(response.message));
    }
    if !response.ok && matches!(response.reason.as_str(), "dispatch_failed" | "timeout") {
        return Err(ApiError::Internal(response.message));
    }

    Ok(Json(response))
}
```

This preserves the EXACT existing external behavior (same status codes for the same failure modes: `Conflict`/409 for not-pending/already-claimed/offline, `Internal`/500 for dispatch failures) — the refactor must be invisible to every existing test in `bookings_routes.rs`. If any existing test fails after this change, the mapping above has a mismatch against the pre-refactor behavior; fix the mapping, not the test.

- [ ] **Step 3: Run the existing test suite unchanged — this IS the regression proof**

Run: `cargo test -p api-gateway --test bookings_routes -- --test-threads=1`
Expected: every test that existed BEFORE this task still passes, with ZERO test-file changes (if this task needs to touch `bookings_routes.rs` to make tests pass, the refactor changed observable behavior — stop and reconcile against Step 2's mapping before proceeding).

- [ ] **Step 4: Full crate + workspace verification**

Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings` — 0 failures, clean.
Run: `cargo test --workspace -- --test-threads=1` — 0 failures (this touches a route every other sub-phase's tests don't call, but a full run is cheap insurance for a refactor of shared logic).

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs
git commit -m "refactor(api-gateway): extract execute_manual_accept — shared core for Fase 6e's quick-accept routes"
```

---

## Task 4: `GET/POST /q/:token` — HMAC quick-accept flow

**Files:**
- Create: `Backend/crates/api-gateway/src/routes/quick_accept.rs`
- Modify: `Backend/crates/api-gateway/src/routes/mod.rs`

**Interfaces:**
- Consumes: `spx_client::crypto::quick_token::{verify_quick_token}`, `store::bookings::get_by_spx_id` (Task 2), `crate::routes::bookings::execute_manual_accept` (Task 3 — widen its visibility to `pub(crate)` if it was left private; this is the ONE cross-file interface this task needs from Task 3, note it explicitly if not already `pub(crate)`).
- Produces (for Task 6): `quick_accept::hmac_router(state: AppState) -> Router<AppState>`.

- [ ] **Step 1: Write the module — token validation, page rendering, and the two HMAC handlers**

```rust
// Backend/crates/api-gateway/src/routes/quick_accept.rs
//! `GET/POST /q/:token` (HMAC flow) + `GET/POST /accept/:code` (Task 5, short-code flow) —
//! the login-free "quick accept" links embedded in a WhatsApp notification. Both mounted
//! OUTSIDE `session_auth` (the token/code itself IS the authorization, matching the reference).
//! GET returns a minimal HTML confirmation page (this crate's first non-JSON response — Fase 7's
//! Command Center replaces this page's styling entirely, so intentionally undecorated: correct
//! state, no CSS-parity attempt with the reference). POST returns JSON `{ok, reason, message}`
//! with REAL HTTP status codes on failure (400/410/429) — a disclosed deviation from the
//! reference's blanket 200 (this crate's established convention everywhere else uses accurate
//! status codes; the JSON body shape still lets a client render a message either way).
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::state::AppState;
use spx_client::crypto::quick_token::verify_quick_token;

use super::bookings::{execute_manual_accept, ManualAcceptResponse};

/// Printable token/code shape — reject anything malformed BEFORE touching crypto/Redis/DB.
/// Mirrors the reference's `VALID_CODE` regex intent (`^[A-Za-z0-9_.\-]{4,512}$`) without a
/// `regex` dependency (this crate has none).
fn is_valid_token_shape(s: &str) -> bool {
    let len = s.len();
    (4..=512).contains(&len)
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Serialize)]
struct QuickAcceptJsonResponse {
    ok: bool,
    reason: String,
    message: String,
}
impl From<ManualAcceptResponse> for QuickAcceptJsonResponse {
    fn from(r: ManualAcceptResponse) -> Self {
        Self {
            ok: r.ok,
            reason: r.reason,
            message: r.message,
        }
    }
}

fn error_page(status: StatusCode, text: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Terima Tiket</title></head>\
         <body style=\"font-family:sans-serif;max-width:420px;margin:60px auto;padding:0 20px;text-align:center\">\
         <p>{text}</p></body></html>"
    );
    (status, Html(body)).into_response()
}

/// Booking-state confirmation page. `postUrl` is the exact endpoint the page's own fetch()
/// posts back to (`/q/accept` or `/accept/:code`, matching Task 5) with `postBody` as the JSON
/// body — kept generic on purpose since Task 5 reuses this exact fn with a different post target.
fn confirmation_page(spx_id: &str, status: &str, post_url: &str, post_body: &str) -> Response {
    let (label, disabled) = match status {
        "accepted" => ("Tiket sudah diterima", true),
        "gone" => ("Tiket tidak tersedia lagi", true),
        _ => ("Terima tiket ini", false),
    };
    let btn_attr = if disabled { "disabled" } else { "" };
    let body = format!(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Terima Tiket</title></head>\
         <body style=\"font-family:sans-serif;max-width:420px;margin:40px auto;padding:0 20px\">\
         <p>Booking ID: <b>{spx_id}</b></p>\
         <p>{label}</p>\
         <button id=\"go\" {btn_attr} style=\"width:100%;padding:14px;font-size:16px\">Terima Tiket Sekarang</button>\
         <p id=\"msg\"></p>\
         <script>\
         var POST={post_url:?};var BODY={post_body};\
         document.getElementById('go').onclick=async function(){{\
           var b=this,m=document.getElementById('msg');b.disabled=true;\
           try{{var r=await fetch(POST,{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify(BODY)}});\
           var d=await r.json();m.textContent=d.message||(d.ok?'Berhasil diterima.':'Gagal.');\
           if(!d.ok)b.disabled=false;}}catch(e){{m.textContent='Koneksi gagal.';b.disabled=false;}}\
         }};\
         </script></body></html>"
    );
    Html(body).into_response()
}

async fn get_quick_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Response {
    if !is_valid_token_shape(&token) {
        return error_page(StatusCode::BAD_REQUEST, "Tautan tidak valid.");
    }
    let Some(claims) = verify_quick_token(&state.master_key, state.tenant_id, &token, now_ms())
    else {
        return error_page(StatusCode::GONE, "Tautan sudah kedaluwarsa atau tidak valid.");
    };
    let booking = match store::bookings::get_by_spx_id(&state.poller.pool, state.tenant_id, &claims.spx_id).await {
        Ok(Some(b)) => b,
        Ok(None) => return error_page(StatusCode::NOT_FOUND, "Tiket tidak ditemukan."),
        Err(_) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Terjadi kesalahan."),
    };
    let page_status = match booking.status.as_str() {
        "accepted" => "accepted",
        "pending" => "available",
        _ => "gone",
    };
    confirmation_page(
        &claims.spx_id,
        page_status,
        "/q/accept",
        &format!("{{\"token\":{token:?}}}"),
    )
}

#[derive(Debug, Deserialize)]
struct QuickAcceptBody {
    token: String,
}

async fn post_quick_accept(
    State(state): State<AppState>,
    Json(body): Json<QuickAcceptBody>,
) -> (StatusCode, Json<QuickAcceptJsonResponse>) {
    if !is_valid_token_shape(&body.token) {
        return (
            StatusCode::BAD_REQUEST,
            Json(QuickAcceptJsonResponse {
                ok: false,
                reason: "bad_request".to_string(),
                message: "Permintaan tidak valid".to_string(),
            }),
        );
    }
    let Some(claims) = verify_quick_token(&state.master_key, state.tenant_id, &body.token, now_ms())
    else {
        return (
            StatusCode::GONE,
            Json(QuickAcceptJsonResponse {
                ok: false,
                reason: "expired_or_invalid".to_string(),
                message: "Tautan tidak valid atau kedaluwarsa".to_string(),
            }),
        );
    };

    let booking = match store::bookings::get_by_spx_id(&state.poller.pool, state.tenant_id, &claims.spx_id).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(QuickAcceptJsonResponse {
                    ok: false,
                    reason: "not_found".to_string(),
                    message: "Tiket tidak ditemukan".to_string(),
                }),
            );
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(QuickAcceptJsonResponse {
                    ok: false,
                    reason: "internal".to_string(),
                    message: "Terjadi kesalahan".to_string(),
                }),
            );
        }
    };

    let result = execute_manual_accept(&state, claims.tenant_id, &booking).await;
    let status = if result.ok {
        StatusCode::OK
    } else {
        match result.reason.as_str() {
            "not_pending" | "already_claimed" => StatusCode::CONFLICT,
            "account_offline" => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    };
    (status, Json(result.into()))
}

/// Mounted at `/q` in `build_router` (Task 6). Rate limits applied there, not here — see Task 6's
/// own doc comment for why (keeps this module's router focused on routing, not cross-cutting
/// layers, matching every other `*_router` fn in this crate).
pub fn hmac_router(_state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{token}", get(get_quick_token))
        .route("/accept", post(post_quick_accept))
}
```

- [ ] **Step 2: Wire the module**

```rust
// Backend/crates/api-gateway/src/routes/mod.rs
pub mod quick_accept;
```

Also widen `execute_manual_accept` and `ManualAcceptResponse` to `pub(crate)` in `routes/bookings.rs` if Task 3 left them private (module-private items aren't visible from `routes/quick_accept.rs`).

- [ ] **Step 3: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/quick_accept_routes.rs (new file)
//! `GET/POST /q/:token` — the HMAC quick-accept flow, reachable with NO session cookie at all.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use api_gateway::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes([7u8; 32]))
}
async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url()).expect("open redis client").get_connection_manager().await.expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id).bind("Quick Accept Test Tenant").bind(format!("qa-test-{id}"))
        .execute(pool).await.expect("insert tenant");
    id
}
async fn insert_booking(pool: &sqlx::PgPool, tenant_id: Uuid, spx_id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO bookings (tenant_id, account_id, spx_id, status, raw_data) \
         VALUES ($1, 'acct-qa', $2, $3, '{}'::jsonb)",
    )
    .bind(tenant_id).bind(spx_id).bind(status)
    .execute(pool).await.expect("insert booking");
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor), client: Arc::new(client), pool: pool.clone(),
        config: poller::PollerConfig::default(), accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar), notifier: None, redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared, ws_hub: ws_hub::Hub::new(), tenant_id,
        cors_origins: Arc::new(vec![]), session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false, master_key: test_master_key(), redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

#[tokio::test]
async fn get_quick_token_is_reachable_with_no_session_at_all() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPX-QA-1", "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key, tenant_id, "SPX-QA-1",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS, now,
    ).unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "must be reachable with zero Cookie header");
    let body = resp.text().await.unwrap();
    assert!(body.contains("SPX-QA-1"));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn expired_token_returns_410() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let long_ago = chrono::Utc::now().timestamp_millis() - 999_999_999;
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key, tenant_id, "SPX-QA-2", 1, long_ago,
    ).unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 410);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn malformed_token_returns_400() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/q/a")).send().await.unwrap(); // too short
    assert_eq!(resp.status(), 400);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn post_quick_accept_on_a_nonexistent_booking_returns_404() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key, tenant_id, "SPX-DOES-NOT-EXIST",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS, now,
    ).unwrap();

    let resp = http.post(format!("{base}/q/accept"))
        .json(&serde_json::json!({"token": token}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 404);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn wrong_tenant_token_is_rejected_by_this_deployments_state_tenant_id() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // Signed for a DIFFERENT tenant than this deployment's own state.tenant_id.
    let other_tenant = Uuid::new_v4();
    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key, other_tenant, "SPX-QA-3",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS, now,
    ).unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 410, "a token signed for a different tenant must verify as invalid here");

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p api-gateway --test quick_accept_routes -- --test-threads=1`
Expected: 5 tests passing.

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/quick_accept.rs Backend/crates/api-gateway/src/routes/mod.rs \
        Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/api-gateway/tests/quick_accept_routes.rs
git commit -m "feat(api-gateway): GET/POST /q/:token — HMAC quick-accept flow (login-free)"
```

---

## Task 5: `GET/POST /accept/:code` — Redis short-code flow

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/quick_accept.rs`
- Modify: `Backend/crates/api-gateway/tests/quick_accept_routes.rs`

**Interfaces:**
- Consumes: this task's own file's `confirmation_page`/`error_page`/`is_valid_token_shape` (Task 4, same file — reuse, do not duplicate).
- Produces (for Task 6): `quick_accept::short_code_router(state: AppState) -> Router<AppState>`.

- [ ] **Step 1: Write the route module — short-code handlers**

```rust
// Backend/crates/api-gateway/src/routes/quick_accept.rs — append
/// `spx:qa:<code>` Redis key (30-min TTL) → JSON `{"b": "<spx_id>"}` (this task's minimal shape
/// — the reference's own key ALSO carries display fields `n/r/v/s/st/pe/ba` for a richer page,
/// but nothing in TOWER writes those yet since code *generation* is out of this plan's scope
/// — see Global Constraints; a future generator can extend this shape additively, `serde`'s
/// `#[serde(default)]` on any new optional field keeps this reader forward-compatible).
#[derive(Debug, Deserialize)]
struct ShortCodeEntry {
    b: String,
}

fn short_code_redis_key(code: &str) -> String {
    format!("spx:qa:{code}")
}

async fn get_short_code(
    State(mut state): State<AppState>,
    Path(code): Path<String>,
) -> Response {
    if !is_valid_token_shape(&code) {
        return error_page(StatusCode::BAD_REQUEST, "Tautan tidak valid.");
    }
    let raw: Option<String> = redis::AsyncCommands::get(&mut state.redis, short_code_redis_key(&code))
        .await
        .unwrap_or(None);
    let Some(raw) = raw else {
        return error_page(StatusCode::GONE, "Tautan sudah kedaluwarsa.");
    };
    let Ok(entry) = serde_json::from_str::<ShortCodeEntry>(&raw) else {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Terjadi kesalahan.");
    };
    let booking = match store::bookings::get_by_spx_id(&state.poller.pool, state.tenant_id, &entry.b).await {
        Ok(Some(b)) => b,
        Ok(None) => return error_page(StatusCode::NOT_FOUND, "Tiket tidak ditemukan."),
        Err(_) => return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Terjadi kesalahan."),
    };
    let page_status = match booking.status.as_str() {
        "accepted" => "accepted",
        "pending" => "available",
        _ => "gone",
    };
    confirmation_page(&entry.b, page_status, &format!("/accept/{code}"), "{}")
}

async fn post_short_code(
    State(mut state): State<AppState>,
    Path(code): Path<String>,
) -> (StatusCode, Json<QuickAcceptJsonResponse>) {
    if !is_valid_token_shape(&code) {
        return (
            StatusCode::BAD_REQUEST,
            Json(QuickAcceptJsonResponse {
                ok: false,
                reason: "bad_request".to_string(),
                message: "Permintaan tidak valid".to_string(),
            }),
        );
    }
    let key = short_code_redis_key(&code);
    let raw: Option<String> = redis::AsyncCommands::get(&mut state.redis, key.clone())
        .await
        .unwrap_or(None);
    let Some(raw) = raw else {
        return (
            StatusCode::GONE,
            Json(QuickAcceptJsonResponse {
                ok: false,
                reason: "expired_or_invalid".to_string(),
                message: "Tautan tidak valid atau kedaluwarsa".to_string(),
            }),
        );
    };
    let Ok(entry) = serde_json::from_str::<ShortCodeEntry>(&raw) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(QuickAcceptJsonResponse {
                ok: false,
                reason: "internal".to_string(),
                message: "Terjadi kesalahan".to_string(),
            }),
        );
    };
    let booking = match store::bookings::get_by_spx_id(&state.poller.pool, state.tenant_id, &entry.b).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(QuickAcceptJsonResponse {
                    ok: false,
                    reason: "not_found".to_string(),
                    message: "Tiket tidak ditemukan".to_string(),
                }),
            );
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(QuickAcceptJsonResponse {
                    ok: false,
                    reason: "internal".to_string(),
                    message: "Terjadi kesalahan".to_string(),
                }),
            );
        }
    };

    let result = execute_manual_accept(&state, state.tenant_id, &booking).await;
    // Single-use on success — matches the reference's `redis.del(...)` on the winning path only,
    // so a failed/retriable attempt leaves the code intact for a genuine retry.
    if result.ok {
        let _: Result<i64, redis::RedisError> =
            redis::AsyncCommands::del(&mut state.redis, key).await;
    }
    let status = if result.ok {
        StatusCode::OK
    } else {
        match result.reason.as_str() {
            "not_pending" | "already_claimed" | "account_offline" => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    };
    (status, Json(result.into()))
}

/// Mounted at `/accept` in `build_router` (Task 6).
pub fn short_code_router(_state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{code}", get(get_short_code).post(post_short_code))
}
```

- [ ] **Step 2: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/quick_accept_routes.rs — append
#[tokio::test]
async fn short_code_flow_round_trips_and_is_single_use() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPX-QA-CODE-1", "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let mut redis_conn = test_redis_manager().await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let code = "testCode123";
    let _: () = redis::AsyncCommands::set_ex(
        &mut redis_conn, format!("spx:qa:{code}"), r#"{"b":"SPX-QA-CODE-1"}"#, 1800,
    ).await.unwrap();

    let get_resp = http.get(format!("{base}/accept/{code}")).send().await.unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.text().await.unwrap();
    assert!(body.contains("SPX-QA-CODE-1"));

    // Account not connected in this test harness -> accept fails with a real, non-2xx status,
    // proving the route genuinely reached execute_manual_accept (not a silent no-op).
    let post_resp = http.post(format!("{base}/accept/{code}")).send().await.unwrap();
    assert_eq!(post_resp.status(), 409);
    let post_body: serde_json::Value = post_resp.json().await.unwrap();
    assert_eq!(post_body["ok"], false);
    assert_eq!(post_body["reason"], "account_offline");

    // Failure path must NOT delete the code (only a successful accept does) — confirm it's
    // still readable.
    let still_there: Option<String> = redis::AsyncCommands::get(&mut redis_conn, format!("spx:qa:{code}")).await.unwrap();
    assert!(still_there.is_some(), "a failed accept attempt must not consume the code");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn expired_short_code_returns_410() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/accept/never-existed-code")).send().await.unwrap();
    assert_eq!(resp.status(), 410);

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p api-gateway --test quick_accept_routes -- --test-threads=1`
Expected: all 7 tests passing (5 from Task 4 + 2 new).

- [ ] **Step 4: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/quick_accept.rs Backend/crates/api-gateway/tests/quick_accept_routes.rs
git commit -m "feat(api-gateway): GET/POST /accept/:code — Redis short-code quick-accept flow (read side)"
```

---

## Task 6: Rate limiting + mount into `build_router`

**Files:**
- Modify: `Backend/crates/api-gateway/src/middleware/rate_limit.rs`
- Modify: `Backend/crates/api-gateway/src/lib.rs`

**Interfaces:**
- Consumes: `middleware::rate_limit::{quick_accept_view_rate_limit_layer, quick_accept_action_rate_limit_layer}` (new, this task), `routes::quick_accept::{hmac_router, short_code_router}` (Tasks 4/5).

- [ ] **Step 1: Add the two new rate-limit configs**

Read `middleware/rate_limit.rs`'s existing `login_rate_limit_layer`/`public_rate_limit_layer` fully first (already summarized in this plan's research) — the two new fns below follow the IDENTICAL structure (`GovernorConfigBuilder` + `SmartIpKeyExtractor` + `.expect(...)` with the same non-panic justification), only the burst/period constants and the route-scope doc comment differ.

```rust
// Backend/crates/api-gateway/src/middleware/rate_limit.rs — append

/// ~60 requests/minute/IP for quick-accept PAGE views (`GET /q/:token`, `GET /accept/:code`) —
/// matches the reference's own `rlView` budget exactly (60/60s), a lenient budget since this is
/// a page render, not a state-changing action. A burst of 60 immediately, replenishing one
/// element every 1000ms thereafter.
const QUICK_ACCEPT_VIEW_BURST_SIZE: u32 = 60;
const QUICK_ACCEPT_VIEW_REPLENISH_PERIOD_MS: u64 = 1000;

pub fn quick_accept_view_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(QUICK_ACCEPT_VIEW_REPLENISH_PERIOD_MS)
        .burst_size(QUICK_ACCEPT_VIEW_BURST_SIZE)
        .finish()
        .expect("QUICK_ACCEPT_VIEW_BURST_SIZE and _REPLENISH_PERIOD_MS are both non-zero");
    GovernorLayer::new(config)
}

/// ~12 requests/minute/IP for quick-accept ACTIONS (`POST /q/accept`, `POST /accept/:code`) —
/// matches the reference's own `rlAccept` budget exactly (12/60s), a stricter budget than the
/// view limiter since this fires a real accept attempt against SPX (a state-changing, external
/// side effect — anti-brute-force AND anti-DoS-against-SPX, not just anti-scraping).
const QUICK_ACCEPT_ACTION_BURST_SIZE: u32 = 12;
const QUICK_ACCEPT_ACTION_REPLENISH_PERIOD_MS: u64 = 5000;

pub fn quick_accept_action_rate_limit_layer(
) -> GovernorLayer<SmartIpKeyExtractor, NoOpMiddleware, axum::body::Body> {
    let config = GovernorConfigBuilder::default()
        .key_extractor(SmartIpKeyExtractor)
        .per_millisecond(QUICK_ACCEPT_ACTION_REPLENISH_PERIOD_MS)
        .burst_size(QUICK_ACCEPT_ACTION_BURST_SIZE)
        .finish()
        .expect("QUICK_ACCEPT_ACTION_BURST_SIZE and _REPLENISH_PERIOD_MS are both non-zero");
    GovernorLayer::new(config)
}
```

- [ ] **Step 2: Apply the rate limits inside each router fn**

```rust
// Backend/crates/api-gateway/src/routes/quick_accept.rs — modify hmac_router/short_code_router
pub fn hmac_router(_state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{token}", get(get_quick_token))
        .route_layer(crate::middleware::rate_limit::quick_accept_view_rate_limit_layer())
        .route("/accept", post(post_quick_accept))
        // NOTE: route_layer above only wraps routes registered BEFORE it in axum 0.8 —
        // verify against this crate's existing multi-route-with-different-layers precedent
        // (`routes/prices.rs`'s public.merge(protected) shape) whether `/accept`'s stricter
        // limiter needs a SEPARATE sub-router merged in, same pattern as that file, rather than
        // a second `.route_layer` call on this same builder (axum only allows ONE route_layer
        // per router value in the fluent chain the way it's used elsewhere in this crate).
}
```

**Read `routes/prices.rs::prices_router` (already-shipped Fase 6d code) before writing this step** — it already solves "two different rate limits on two different routes in the same mount point" via `Router::new()...route_layer(A)` for one half `.merge()`d with a second `Router::new()...route_layer(B)` for the other half. Mirror that EXACT shape for `hmac_router` (view limiter on the `/{token}` half, action limiter on the `/accept` half) and `short_code_router` (view limiter on GET, action limiter on POST — note GET and POST share the same path `/{code}` here, unlike `hmac_router`'s two different paths; check whether `tower_governor`'s `route_layer` can be scoped to one HTTP method on a shared path, or whether `short_code_router` needs `.route("/{code}", get(...))` and `.route("/{code}", post(...))` as two separately-layered single-method routers merged together instead of one `MethodRouter` — read `tower_governor`'s actual behavior here rather than assuming, this is exactly the class of layering subtlety Fase 6d Task 8 got wrong on the first pass).

- [ ] **Step 3: Mount into `build_router`**

```rust
// Backend/crates/api-gateway/src/lib.rs — inside build_router's `rest` chain, add:
.nest("/q", routes::quick_accept::hmac_router(state.clone()))
.nest("/accept", routes::quick_accept::short_code_router(state.clone()))
```

Add these `.nest(...)` calls to the SAME `rest` tree every other route (prices/locations/bot) already joins — quick-accept doesn't need branding's 15MB carve-out, the existing 1.5MB global limit is more than sufficient for a token/code + small JSON body. Do NOT wrap either router in `session_auth` — these are the two genuinely public route groups this task adds, alongside `GET /prices`/`GET /branding`'s already-established public precedent.

- [ ] **Step 4: Write the failing tests proving BOTH rate limits fire correctly-scoped**

```rust
// Backend/crates/api-gateway/tests/quick_accept_routes.rs — append
#[tokio::test]
async fn action_rate_limit_is_stricter_than_view_rate_limit() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // 13 rapid POSTs to /accept/:code (an invalid code each time, doesn't matter — the
    // rate limiter fires before the handler's own 410 logic) — the 13th must be 429, proving
    // the 12/window action budget, not the 60/window view budget, gates this path.
    let mut saw_429 = false;
    for _ in 0..13 {
        let resp = http.post(format!("{base}/accept/rl-test-code")).send().await.unwrap();
        if resp.status() == 429 {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "the 12/min action limiter must eventually reject rapid POSTs");

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 5: Run the tests, then full crate + workspace verification**

Run: `cargo test -p api-gateway --test quick_accept_routes -- --test-threads=1` — all 8 tests PASS.
Run: `cargo test -p api-gateway -- --test-threads=1 && cargo clippy -p api-gateway --all-targets -- -D warnings` — 0 failures, clean.
Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — this task restructures `build_router` again (a load-bearing shared fn); full workspace scope is the right check here, same reasoning as Fase 6d Task 8.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/api-gateway/src/middleware/rate_limit.rs Backend/crates/api-gateway/src/lib.rs \
        Backend/crates/api-gateway/src/routes/quick_accept.rs Backend/crates/api-gateway/tests/quick_accept_routes.rs
git commit -m "feat(api-gateway): rate-limit + mount /q and /accept (quick-accept flows now reachable)"
```

---

## Task 7: Final Fase-6-WIDE workspace verification + DoD sign-off

**Files:** none (verification-only task, no new code) plus the shared design doc (tracked notes).

- [ ] **Step 1: Full workspace test suite**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && unset DATABASE_URL && export REDIS_URL="redis://127.0.0.1:16379" && cargo test --workspace -- --test-threads=1 2>&1 | tail -100`
Expected: `0 failed` across every crate.

- [ ] **Step 2: Clippy, workspace-wide, warnings as errors**

Run: `cargo clippy --workspace --all-targets -- -D warnings` — clean.

- [ ] **Step 3: `cargo deny check`**

Run: `cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok`, exit 0. Confirm the only new `Cargo.lock` entries this whole sub-phase introduced are `hmac 0.13.0`'s promotion to a direct edge (Task 1) — no new crate VERSIONS anywhere.

- [ ] **Step 4: `cargo tree` cross-dependency check (DoD #8)**

Run: `cargo tree -p api-gateway --depth 1 | grep -E "(store|executor|spx-client|poller|ws-hub|notifier|core-domain) v"` — confirm all 7 present.
Run: `for c in store executor spx-client poller ws-hub notifier core-domain; do cargo tree -p "$c" -i api-gateway 2>&1; done` — every invocation must fail with "did not match any packages".

- [ ] **Step 5: Checkbox guard**

```bash
grep -c '^\- \[ \]' Docs/superpowers/plans/2026-07-17-fase-6e-quick-accept-hardening.md
```
Expected: `0` after converting every real step checkbox to `[x]`. Verify via `diff` against a pre-conversion copy that only checkbox markers changed.

- [ ] **Step 6: Fase-6-WIDE Definition of Done cross-check (all 5 sub-phases, not just 6e)**

Re-read `Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md`'s DoD list (8 items) and the progress ledger's 6a-6e history. For each item, state which sub-phase closed it and whether it is GENUINELY closeable now:

- **#1** (route parity): closed — every route in the design doc's research brief now has a TOWER equivalent across 6a-6e, tested.
- **#2** (`require_permission` on every mutating route): closed for every route EXCEPT the two deliberate, disclosed exceptions (`POST /bookings/:id/accept` — Fase 6c Task 10's own disclosed decision; the new `/q/accept`, `/accept/:code` — this task's own by-design public exception, the token/code itself IS the authorization). Both exceptions must be named explicitly, not silently passed.
- **#3** (OTP gate): closed since Fase 6b/6c.
- **#4** (`docker compose up` end-to-end): **NOT closed by this sub-phase — explicitly re-confirmed as Fase 8's responsibility this session (human decision).** Do not claim this item closed; state its owner plainly.
- **#5** (security headers/CORS/body-limit/rate-limit): closed — this task adds the two remaining rate-limit budgets (12/min, 60/min) the design doc named for quick-accept specifically.
- **#6** (quick-accept HMAC round-trip + tamper/expiry rejection, constant-time compare): closed by Task 1's tests + `hmac::Mac::verify_slice`'s built-in constant-time guarantee.
- **#7** (clean workspace): re-confirmed by Steps 1-3 above.
- **#8** (cross-dependency uniqueness): re-confirmed by Step 4 above.

- [ ] **Step 7: Add the disclosed Fase-5 gap as a tracked design-doc note**

Append to `Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md` (matching the existing tracked-notes' style/section — read at least 2 first):

> **Quick-accept short-code *generation* is unbuilt (Fase 6e review scope note).** `notifier::notify_new_tickets` (built Fase 5) has no caller anywhere in the codebase, and its link-building still uses a raw `spx_id` rather than a real `spx:qa:<code>` short code — Fase 6e (this sub-phase) built the READ side of both the HMAC-token and short-code quick-accept flows (parity with the reference's `verifyQuickToken`/`GET /accept/:code`, both of which the reference itself also has as effectively-dead-until-linked infrastructure), but generating and sending a real link from a real new-ticket detection event is unbuilt. Tracked for whichever future phase wires `poller`'s ticket-detection loop to `notifier::notify_new_tickets` with a real short-code generator (mirrors the reference's `webhook.ts::genAcceptCode`) — until then, `GET/POST /q/:token` and `/accept/:code` are reachable and correct, but nothing in TOWER currently produces a link to them.

- [ ] **Step 8: Update the progress ledger**

Append one line per task plus a closing summary: `Fase 6e (quick-accept HMAC + hardening): all 7 tasks complete. Fase 6 (6a-6e) Definition of Done fully cross-checked — item #4 explicitly owned by Fase 8, not closed here. Proceeding to final whole-branch review.`

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "test(fase-6e): quick-accept + Fase-6-wide DoD sign-off — full workspace verification"
```

---

## Self-Review Notes (writing-plans skill, run by the plan author before handoff)

**Spec coverage:** every clause of the master spec's 6e bullet has a task — `sign_quick_token`/`verify_quick_token` (Task 1), `GET /q/:token` + `POST /q/accept` (Task 4), the short-code Redis flow `GET/POST /accept/:code` (Task 5), both mounted outside `session_auth` (Task 6), final Fase-6 workspace verification + DoD sign-off (Task 7). Two genuine, previously-undiscovered gaps surfaced during planning and are resolved with disclosed, reasoned scope decisions rather than silently invented or silently absorbed: short-code *generation*/`notify_new_tickets` wiring (Fase 5's gap, explicitly deferred, Task 7 Step 7 tracks it) and the `docker compose`/`app_role` provisioning gap (explicitly confirmed as Fase 8's ownership with the user this session, not re-litigated here).

**Placeholder scan:** no "TBD"/"handle appropriately"/"similar to Task N" patterns. Two steps (Task 2 Step 2, Task 3 Step 1) explicitly flag themselves as best-effort reconstructions needing verification against the real current file rather than blind transcription — this is a disclosed research-confidence gap (this plan's author could not read `get_detail`'s exact column list and `accept()`'s exact current body byte-for-byte while authoring in the time available), not a placeholder: the actual REQUIREMENT (match the real `get_detail` shape; preserve the real `accept()` behavior exactly) is stated precisely, only the verbatim source text is deferred to the implementer's own read of the file — the established, repeatedly-used convention every prior sub-phase's plans also relied on when full source visibility wasn't available at plan-writing time.

**Type consistency:** `ManualAcceptResponse` (Task 3) is constructed identically by `execute_manual_accept`'s every branch and consumed identically by `accept()` (Task 3), `post_quick_accept` (Task 4), and `post_short_code` (Task 5) via the same `QuickAcceptJsonResponse::from` conversion. `BookingDetail` (whatever `get_detail`'s real return type is named) is used identically by `get_by_spx_id` (Task 2), `execute_manual_accept` (Task 3), and both new route handlers (Tasks 4/5) — the plan explicitly instructs matching the real type name rather than inventing a parallel one.

**Cross-task dependency order:** Task 1 (crypto) and Task 2 (store) are independent of each other and of Task 3. Task 3 (refactor) depends on nothing new — it's a pure extraction from already-shipped Fase 6c code — but must land BEFORE Tasks 4/5 so they have `execute_manual_accept` to call. Task 4 depends on Tasks 1-3. Task 5 depends on Tasks 2-4 (reuses Task 4's page-rendering helpers in the same file). Task 6 depends on Tasks 4-5 (mounts their routers). Task 7 depends on everything. This ordering (1,2 parallel-safe → 3 → 4 → 5 → 6 → 7) is a valid topological sort; subagent-driven-development should still run them serially (never parallel implementers per that skill's own Red Flags), but Tasks 1 and 2 have no ordering constraint between them if a controller ever wants to reorder.
