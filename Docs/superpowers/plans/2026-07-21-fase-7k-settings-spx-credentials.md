# Fase 7k — `/settings/spx-credentials` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the fifth and final `/settings/*` sub-page — a management UI for the tenant's stored SPX agency credentials (list / add / delete), plus a guarded "Test Koneksi" button wired to `POST /auth/spx-login/{label}`, and a small server-side cooldown on that endpoint.

**Architecture:** Mostly frontend (a pure-logic module, a typed REST module, a one-line nav append, a page, and e2e tests), following the exact patterns of the four sibling `/settings/*` pages. The single backend change is a per-`(tenant, label)` Redis cooldown on the test-connection route — the first backend touch in the `/settings` sub-phases, because the endpoint performs up to 8 real SPX logins per click with zero existing protection.

**Tech Stack:** Rust (axum 0.8, `redis` crate `ConnectionManager`) for the backend task; SvelteKit 5 (runes), TypeScript, Vitest, Playwright for the frontend.

## Global Constraints

- **RBAC is edit-gated:** `GET /auth/spx-credentials` is session-auth-only (any tenant member); `PUT`/`DELETE /auth/spx-credentials/{label}` and `POST /auth/spx-login/{label}` require `Permission::ManageSpxCredentials` (main account only). List renders for everyone; write controls disabled for non-main-account.
- **The stored password is never returned** by any endpoint (no plaintext, mask, or `is_set` flag). Existence of a row IS the "password set" signal.
- **`PUT` is a full upsert** keyed by `(tenant_id, label)`; both `username` and `password` are always required non-empty. No partial update, no rename → the edit model is **delete-and-recreate** (re-submitting the add form with the same label overwrites).
- **Saving has NO runtime effect until `reactor-core` restarts** — the poller bootstraps credentials once at boot. The page MUST show an always-visible notice stating this. Never imply "connected"/"active" after save.
- **Trim usernames client-side** and block same-normalized-username-on-a-different-label (poller keys accounts by `username.trim().toLowerCase()`; a collision silently drops the second account at boot).
- **The "Test Koneksi" button** performs a real login against the live SPX upstream and can take up to ~80s. It must: fire only on explicit click (never on mount/interval/reactive trigger); be disabled while in-flight and during a 60s client cooldown; use a 90s `AbortController`; and show honest result copy (the backend cannot distinguish wrong-password vs. SPX-down vs. SPX-rate-limited — all are `200 {ok:false}`).
- **Styling: tokens only** (no raw colors). Inputs `min-h-[40px]`, primary buttons `min-h-[44px]`, small danger buttons `min-h-[36px]`; `focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent` on every interactive element; banners `role="alert"`/`role="status"` with `aria-live="polite"`; real `…` ellipsis; `<svelte:head><title>…</title>`.
- **`ApiError.message` is always a fixed generic English string** — never parse the response body for a message. Pages branch on `err.status` for Indonesian copy.
- **Page data-fetch MUST be inside `onMount`** (SSR: relative-path `fetch` has no origin during Node SSR; a bare top-level call crashes on hard refresh).
- **Indonesian** for all user-facing copy.
- Reference design: `Docs/superpowers/specs/2026-07-21-fase-7k-settings-spx-credentials-design.md`.

---

### Task 1: Backend — server-side cooldown on `POST /auth/spx-login/{label}`

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/spx_login.rs`
- Test: `Backend/crates/api-gateway/tests/spx_login_routes.rs` (add one test)

**Interfaces:**
- Consumes: `AppState.redis: redis::aio::ConnectionManager` (already present), `ApiError::TooManyRequests(String)` → 429 (already present), the `SET NX EX` idiom from `src/otp.rs` (`use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};`).
- Produces: no new public interface. Same route, same success/error responses, plus a new `429 {"error":"…"}` when a test is already running or was run within 60s for the same `(tenant, label)`.

**Context:** `POST /auth/spx-login/{label}` currently has no rate limit, no in-flight lock, and no early-exit on SPX 429/403 — one click fans out to up to 8 real logins against the tenant's live SPX account. This task adds a Redis cooldown claimed **after** the 403/404/decrypt checks (so requests that never touch SPX don't burn the window) and refreshed **after** the login completes (so the 60s is measured from completion, not from the start of an ~80s call). The `NX` claim doubles as an in-flight lock. All four existing tests in `spx_login_routes.rs` use a unique `Uuid` tenant and fire exactly one login, so none is affected by the cooldown.

- [ ] **Step 1: Read the current file and its test to confirm the exact code being replaced**

Run: `sed -n '1,75p' Backend/crates/api-gateway/src/routes/spx_login.rs` and `grep -n "async fn success_via_api_tier" Backend/crates/api-gateway/tests/spx_login_routes.rs`
Expected: the handler body matches the "before" block in Step 3; the test harness helpers (`build_state`, `spawn_server`, `login`, `seed_credential`, `insert_portal_user`, `insert_tenant`, `cleanup`) exist and are reusable.

- [ ] **Step 2: Write the failing test** (append to `Backend/crates/api-gateway/tests/spx_login_routes.rs`)

```rust
/// Case 5: a second `POST /auth/spx-login/:label` for the SAME (tenant, label)
/// within the cooldown window is rejected with 429, even though the first
/// call succeeded. The cooldown is claimed only after the 403/404/decrypt
/// checks, so it guards exactly the calls that would otherwise hit SPX.
#[tokio::test]
async fn second_call_within_cooldown_is_rate_limited() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-login-rl", true).await;
    seed_credential(&pool, tenant_id, "agency1", "agency1-user", "s3cret-agency-pw").await;

    let spx_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/account/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=APITESTKEY; Path=/"),
        )
        .mount(&spx_server)
        .await;

    let state = build_state(pool.clone(), tenant_id, &spx_server.uri()).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-login-rl").await;

    let first = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("first spx-login request");
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    let second = http
        .post(format!("{base}/auth/spx-login/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("second spx-login request");
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test -p api-gateway --test spx_login_routes second_call_within_cooldown_is_rate_limited -- --nocapture`
Expected: FAIL — the second call currently also returns 200 (no cooldown exists yet).

- [ ] **Step 4: Implement the cooldown** — replace the whole handler + router region of `Backend/crates/api-gateway/src/routes/spx_login.rs`.

Before (current lines 6–74):
```rust
use axum::extract::{Extension, Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::decrypt_agency_password;
use spx_client::crypto::secret::ExposeSecret;

#[derive(Debug, Serialize)]
pub struct SpxLoginResult {
    pub ok: bool,
    pub tier: Option<&'static str>,
}

async fn test_login(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
) -> Result<Json<SpxLoginResult>, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    let cred = store::agency_credentials::find_by_label(&state.poller.pool, user.tenant_id, &label)
        .await?
        .ok_or(ApiError::NotFound)?;
    let nonce: [u8; 12] = cred
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::Internal("stored nonce is not 12 bytes".to_string()))?;
    let password = decrypt_agency_password(&state.master_key, user.tenant_id, &cred.ciphertext, &nonce)
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?;

    // Tiers 2/3 only (see this task's design note — no tier 1 in a
    // synchronous HTTP route).
    if let Some(mut jar) = state
        .poller
        .client
        .api_login(&cred.username, password.expose_secret())
        .await
    {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return Ok(Json(SpxLoginResult {
            ok: true,
            tier: Some("api"),
        }));
    }
    if let Some(mut jar) = state
        .poller
        .client
        .form_login(&cred.username, password.expose_secret())
        .await
    {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return Ok(Json(SpxLoginResult {
            ok: true,
            tier: Some("form"),
        }));
    }
    Ok(Json(SpxLoginResult { ok: false, tier: None }))
}

pub fn spx_login_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{label}", post(test_login))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

After:
```rust
use axum::extract::{Extension, Path, State};
use axum::routing::post;
use axum::{Json, Router};
use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;
use spx_client::crypto::envelope::decrypt_agency_password;
use spx_client::crypto::secret::ExposeSecret;

/// Seconds a `(tenant, label)` must wait between connectivity tests. Also the
/// TTL of the in-flight lock: a second click while the first (up to ~80s)
/// login is still running fails the `NX` claim and is rejected immediately.
const TEST_COOLDOWN_SECS: u64 = 60;

fn cooldown_key(tenant_id: Uuid, label: &str) -> String {
    format!("spx:spx_login_rl:{tenant_id}:{label}")
}

#[derive(Debug, Serialize)]
pub struct SpxLoginResult {
    pub ok: bool,
    pub tier: Option<&'static str>,
}

async fn test_login(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(label): Path<String>,
) -> Result<Json<SpxLoginResult>, ApiError> {
    require_permission(&user, Permission::ManageSpxCredentials)?;
    let cred = store::agency_credentials::find_by_label(&state.poller.pool, user.tenant_id, &label)
        .await?
        .ok_or(ApiError::NotFound)?;
    let nonce: [u8; 12] = cred
        .nonce
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::Internal("stored nonce is not 12 bytes".to_string()))?;
    let password = decrypt_agency_password(&state.master_key, user.tenant_id, &cred.ciphertext, &nonce)
        .map_err(|e| ApiError::Internal(format!("{e:?}")))?;

    // Rate-limit + in-flight guard. Claimed AFTER the 403/404/decrypt checks
    // so a request that never reaches SPX doesn't burn the window. `SET NX EX`
    // is atomic (no read-then-write race) — mirrors `otp.rs`'s cooldown idiom.
    // The `NX` failure means either a test is currently running (the key is
    // held for the whole login) or one finished within the last 60s.
    let key = cooldown_key(user.tenant_id, &label);
    let mut redis = state.redis.clone();
    let claim_opts = SetOptions::default()
        .with_expiration(SetExpiry::EX(TEST_COOLDOWN_SECS))
        .conditional_set(ExistenceCheck::NX);
    let acquired: bool = redis
        .set_options(&key, "1", claim_opts)
        .await
        .map_err(|e| ApiError::Internal(format!("redis cooldown claim: {e}")))?;
    if !acquired {
        return Err(ApiError::TooManyRequests(
            "test koneksi sedang berjalan atau baru saja dijalankan, coba lagi sebentar".to_string(),
        ));
    }

    // Tiers 2/3 only (no tier 1 in a synchronous HTTP route).
    let result = run_login(&state, &cred.username, password.expose_secret()).await;

    // Best-effort: reset the window to 60s from COMPLETION, not from the start
    // of a login that may have taken ~80s. Ignore errors — the login already
    // ran and its outcome is what the caller wants; a failed refresh at worst
    // lets the window lapse slightly early. The `NX` claim above already
    // provided the in-flight guarantee.
    let refresh_opts = SetOptions::default().with_expiration(SetExpiry::EX(TEST_COOLDOWN_SECS));
    let _: Result<(), redis::RedisError> = redis.set_options(&key, "1", refresh_opts).await;

    Ok(Json(result))
}

async fn run_login(state: &AppState, username: &str, password: &str) -> SpxLoginResult {
    if let Some(mut jar) = state.poller.client.api_login(username, password).await {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return SpxLoginResult {
            ok: true,
            tier: Some("api"),
        };
    }
    if let Some(mut jar) = state.poller.client.form_login(username, password).await {
        state.poller.client.fetch_spx_cid(&mut jar).await;
        return SpxLoginResult {
            ok: true,
            tier: Some("form"),
        };
    }
    SpxLoginResult {
        ok: false,
        tier: None,
    }
}

pub fn spx_login_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{label}", post(test_login))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [ ] **Step 5: Run the new test + all four existing tests in the file**

Run: `cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test -p api-gateway --test spx_login_routes -- --nocapture`
Expected: PASS — `success_via_api_tier`, `all_tiers_fail_reports_ok_false`, `nonexistent_label_returns_404`, `sub_user_is_forbidden`, and the new `second_call_within_cooldown_is_rate_limited` all green.

- [ ] **Step 6: Clippy on the crate**

Run: `cd Backend && cargo clippy -p api-gateway --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/spx_login.rs Backend/crates/api-gateway/tests/spx_login_routes.rs
git commit -m "feat(api-gateway): per-(tenant,label) cooldown on POST /auth/spx-login (Fase 7k)"
```

---

### Task 2: `lib/spx-credentials.ts` — pure validation logic (TDD)

**Files:**
- Create: `Frontend/src/lib/spx-credentials.ts`
- Test: `Frontend/src/lib/spx-credentials.test.ts`

**Interfaces:**
- Consumes: `type SpxCredential` from `./api-spx-credentials` (Task 3 — a `{ label: string; username: string }`; import is a type-only import so task ordering does not matter for compilation of the tests, which construct plain literals).
- Produces: `validateLabel(label: string): string | null`, `validateUsername(username: string): string | null`, `validatePassword(password: string): string | null`, `duplicateUsernameLabel(username: string, existing: SpxCredential[], currentLabel: string): string | null`.

**Context:** The backend validates essentially nothing on these fields — `label` is an unvalidated URL path segment, `username`/`password` are rejected only if empty. So the client owns all validation. `duplicateUsernameLabel` guards the silent poller-boot collision (two labels whose usernames normalize equal drop one account at boot); it excludes `currentLabel` so a same-label overwrite (password rotation) is allowed.

- [ ] **Step 1: Write the failing tests** → `Frontend/src/lib/spx-credentials.test.ts`

```ts
import { describe, it, expect } from 'vitest';
import {
	validateLabel,
	validateUsername,
	validatePassword,
	duplicateUsernameLabel
} from './spx-credentials';

describe('validateLabel', () => {
	it('rejects empty / whitespace-only', () => {
		expect(validateLabel('')).toBe('Label wajib diisi');
		expect(validateLabel('   ')).toBe('Label wajib diisi');
	});
	it('rejects a label containing a slash (would split the URL path → 404)', () => {
		expect(validateLabel('a/b')).toBe('Label tidak boleh mengandung "/"');
	});
	it('rejects a label longer than 64 chars', () => {
		expect(validateLabel('x'.repeat(65))).toBe('Label maksimal 64 karakter');
	});
	it('accepts a normal label', () => {
		expect(validateLabel('agency1')).toBeNull();
	});
});

describe('validateUsername', () => {
	it('rejects empty / whitespace-only', () => {
		expect(validateUsername('  ')).toBe('Username wajib diisi');
	});
	it('accepts non-empty', () => {
		expect(validateUsername('agency1-user')).toBeNull();
	});
});

describe('validatePassword', () => {
	it('rejects only the empty string (no length floor for SPX credentials)', () => {
		expect(validatePassword('')).toBe('Password wajib diisi');
		expect(validatePassword(' ')).toBeNull();
	});
});

describe('duplicateUsernameLabel', () => {
	const existing = [
		{ label: 'agency1', username: 'Shared-User' },
		{ label: 'agency2', username: 'other' }
	];
	it('flags a case/whitespace-insensitive username clash on a DIFFERENT label', () => {
		expect(duplicateUsernameLabel('  shared-user ', existing, 'agency3')).toBe('agency1');
	});
	it('excludes the same label (overwrite / password rotation is allowed)', () => {
		expect(duplicateUsernameLabel('shared-user', existing, 'agency1')).toBeNull();
	});
	it('returns null when there is no clash', () => {
		expect(duplicateUsernameLabel('brand-new', existing, 'agency3')).toBeNull();
	});
	it('returns null for an empty username', () => {
		expect(duplicateUsernameLabel('   ', existing, 'agency3')).toBeNull();
	});
});
```

- [ ] **Step 2: Run to verify failure**

Run: `cd Frontend && pnpm vitest run src/lib/spx-credentials.test.ts`
Expected: FAIL — module `./spx-credentials` does not exist.

- [ ] **Step 3: Implement** → `Frontend/src/lib/spx-credentials.ts`

```ts
// Frontend/src/lib/spx-credentials.ts
// Pure validation/logic for the /settings/spx-credentials page. No fetch, no DOM.
// The backend validates essentially NOTHING on these fields (label is an
// unvalidated URL path segment; username/password rejected only if empty), so
// the client owns all of it. `duplicateUsernameLabel` guards the poller-boot
// collision: reactor-core keys accounts by username.trim().toLowerCase(), so
// two labels with the same normalized username silently drop one at boot.
// See Docs/superpowers/specs/2026-07-21-fase-7k-settings-spx-credentials-design.md.
import type { SpxCredential } from './api-spx-credentials';

export function validateLabel(label: string): string | null {
	const trimmed = label.trim();
	if (trimmed === '') return 'Label wajib diisi';
	if (label.includes('/')) return 'Label tidak boleh mengandung "/"';
	if (trimmed.length > 64) return 'Label maksimal 64 karakter';
	return null;
}

export function validateUsername(username: string): string | null {
	if (username.trim() === '') return 'Username wajib diisi';
	return null;
}

export function validatePassword(password: string): string | null {
	if (password === '') return 'Password wajib diisi';
	return null;
}

export function duplicateUsernameLabel(
	username: string,
	existing: SpxCredential[],
	currentLabel: string
): string | null {
	const norm = username.trim().toLowerCase();
	if (norm === '') return null;
	const clash = existing.find(
		(c) => c.label !== currentLabel && c.username.trim().toLowerCase() === norm
	);
	return clash ? clash.label : null;
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd Frontend && pnpm vitest run src/lib/spx-credentials.test.ts`
Expected: PASS — all cases green.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/spx-credentials.ts Frontend/src/lib/spx-credentials.test.ts
git commit -m "feat(frontend): spx-credentials.ts pure validation logic (Fase 7k)"
```

---

### Task 3: `lib/api-spx-credentials.ts` — typed REST layer (TDD)

**Files:**
- Create: `Frontend/src/lib/api-spx-credentials.ts`
- Test: `Frontend/src/lib/api-spx-credentials.test.ts`

**Interfaces:**
- Consumes: `apiPost`, `ApiError` from `./api`.
- Produces: `type SpxCredential = { label: string; username: string }`; `type SpxLoginResult = { ok: boolean; tier: 'api' | 'form' | null }`; `fetchSpxCredentials(): Promise<SpxCredential[]>`; `saveSpxCredential(label, username, password): Promise<SpxCredential>`; `deleteSpxCredential(label): Promise<void>`; `testSpxLogin(label: string, signal?: AbortSignal): Promise<SpxLoginResult>`.

**Context:** Wire shape verified against `routes/spx_credentials.rs` (`CredentialSummary`/`UpsertCredential`) and `routes/spx_login.rs` (`SpxLoginResult`) — snake_case throughout, no `rename_all`; here `label`/`username`/`ok`/`tier` are identical on the wire and in TS. `apiPost` hardcodes `POST` and takes no `AbortSignal`, so `saveSpxCredential` (PUT), `deleteSpxCredential` (DELETE) and `testSpxLogin` (POST-with-signal, because the endpoint can hang ~80s and needs the 90s abort) all use raw `fetch` — the same "raw fetch when apiPost's shape doesn't fit" convention the sibling modules already use. `ApiError.message` stays a fixed generic string; pages branch on `.status`.

- [ ] **Step 1: Write the failing tests** → `Frontend/src/lib/api-spx-credentials.test.ts`

```ts
import { describe, it, expect, vi, afterEach } from 'vitest';
import {
	fetchSpxCredentials,
	saveSpxCredential,
	deleteSpxCredential,
	testSpxLogin
} from './api-spx-credentials';
import { ApiError } from './api';

afterEach(() => vi.unstubAllGlobals());

function stubFetch(response: Partial<Response> & { json?: () => Promise<unknown> }) {
	const fn = vi.fn(async () => response as Response);
	vi.stubGlobal('fetch', fn);
	return fn;
}

describe('fetchSpxCredentials', () => {
	it('GETs the list and returns it as-is', async () => {
		const fn = stubFetch({ ok: true, json: async () => [{ label: 'agency1', username: 'u1' }] });
		const result = await fetchSpxCredentials();
		expect(result).toEqual([{ label: 'agency1', username: 'u1' }]);
		expect(fn).toHaveBeenCalledWith('/auth/spx-credentials', { credentials: 'include' });
	});
	it('throws ApiError with the real status on a non-ok response', async () => {
		stubFetch({ ok: false, status: 500 });
		await expect(fetchSpxCredentials()).rejects.toMatchObject({ status: 500 });
	});
});

describe('saveSpxCredential', () => {
	it('PUTs to an encoded label with a {username,password} body and returns the summary', async () => {
		const fn = stubFetch({ ok: true, json: async () => ({ label: 'a b', username: 'u1' }) });
		const result = await saveSpxCredential('a b', 'u1', 'pw');
		expect(result).toEqual({ label: 'a b', username: 'u1' });
		const [url, init] = fn.mock.calls[0];
		expect(url).toBe('/auth/spx-credentials/a%20b');
		expect(init?.method).toBe('PUT');
		expect(JSON.parse(init?.body as string)).toEqual({ username: 'u1', password: 'pw' });
	});
	it('throws ApiError with status 409 on conflict', async () => {
		stubFetch({ ok: false, status: 409 });
		await expect(saveSpxCredential('a', 'u', 'p')).rejects.toMatchObject({ status: 409 });
	});
});

describe('deleteSpxCredential', () => {
	it('DELETEs the encoded label and never parses a body', async () => {
		const json = vi.fn();
		const fn = stubFetch({ ok: true, status: 204, json });
		await deleteSpxCredential('a b');
		const [url, init] = fn.mock.calls[0];
		expect(url).toBe('/auth/spx-credentials/a%20b');
		expect(init?.method).toBe('DELETE');
		expect(json).not.toHaveBeenCalled();
	});
});

describe('testSpxLogin', () => {
	it('POSTs to the encoded label and returns {ok,tier}', async () => {
		const fn = stubFetch({ ok: true, json: async () => ({ ok: true, tier: 'api' }) });
		const result = await testSpxLogin('a b');
		expect(result).toEqual({ ok: true, tier: 'api' });
		const [url, init] = fn.mock.calls[0];
		expect(url).toBe('/auth/spx-login/a%20b');
		expect(init?.method).toBe('POST');
	});
	it('surfaces a 429 as ApiError with status 429', async () => {
		stubFetch({ ok: false, status: 429 });
		await expect(testSpxLogin('a')).rejects.toBeInstanceOf(ApiError);
		await expect(testSpxLogin('a')).rejects.toMatchObject({ status: 429 });
	});
});
```

- [ ] **Step 2: Run to verify failure**

Run: `cd Frontend && pnpm vitest run src/lib/api-spx-credentials.test.ts`
Expected: FAIL — module `./api-spx-credentials` does not exist.

- [ ] **Step 3: Implement** → `Frontend/src/lib/api-spx-credentials.ts`

```ts
// Frontend/src/lib/api-spx-credentials.ts
// Typed REST layer for the tenant's stored SPX agency credentials.
// Wire shape verified against Backend/crates/api-gateway/src/routes/spx_credentials.rs
// (CredentialSummary { label, username }, UpsertCredential { username, password }) and
// routes/spx_login.rs (SpxLoginResult { ok, tier }) — snake_case throughout, no rename_all;
// label/username/ok/tier are identical on the wire and in TS (no case mapping needed).
// The stored password is NEVER returned by the backend in any form — the existence of a
// row IS the "password is set" signal. apiPost hardcodes POST and takes no AbortSignal, so
// PUT/DELETE and the abortable test-login all use raw fetch (same convention as the sibling
// api-*.ts modules). ApiError carries a fixed generic message; pages branch on .status.
import { ApiError } from './api';

export type SpxCredential = { label: string; username: string };
export type SpxLoginResult = { ok: boolean; tier: 'api' | 'form' | null };

export async function fetchSpxCredentials(): Promise<SpxCredential[]> {
	const res = await fetch('/auth/spx-credentials', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch spx credentials');
	return res.json();
}

export async function saveSpxCredential(
	label: string,
	username: string,
	password: string
): Promise<SpxCredential> {
	const res = await fetch(`/auth/spx-credentials/${encodeURIComponent(label)}`, {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ username, password })
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save spx credential');
	return res.json();
}

export async function deleteSpxCredential(label: string): Promise<void> {
	const res = await fetch(`/auth/spx-credentials/${encodeURIComponent(label)}`, {
		method: 'DELETE',
		credentials: 'include'
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to delete spx credential');
	// 204 No Content — deliberately never call res.json().
}

export async function testSpxLogin(label: string, signal?: AbortSignal): Promise<SpxLoginResult> {
	const res = await fetch(`/auth/spx-login/${encodeURIComponent(label)}`, {
		method: 'POST',
		credentials: 'include',
		signal
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to test spx login');
	return res.json();
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cd Frontend && pnpm vitest run src/lib/api-spx-credentials.test.ts`
Expected: PASS — all cases green.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/api-spx-credentials.ts Frontend/src/lib/api-spx-credentials.test.ts
git commit -m "feat(frontend): api-spx-credentials.ts typed REST layer (Fase 7k)"
```

---

### Task 4: `/settings` shell — append the "Akun SPX" nav entry

**Files:**
- Modify: `Frontend/src/routes/(app)/settings/+layout.svelte`

**Interfaces:**
- Consumes: the existing `ALL_NAV_ITEMS: NavItem[]` array and its `NAV_ITEMS = $derived(...)` filter.
- Produces: a new nav entry `{ href: '/settings/spx-credentials', label: 'Akun SPX' }` (no `mainAccountOnly` flag — the page is edit-gated, list visible to all sessions).

**Context:** This is the fifth and final append to the flag-filtered nav array introduced in Fase 7i. No `mainAccountOnly` flag because `GET /auth/spx-credentials` is open to any session. The layout's `{#each NAV_ITEMS …}` loop is fully data-driven, so nothing else changes.

- [ ] **Step 1: Confirm the current array** — Read `Frontend/src/routes/(app)/settings/+layout.svelte` and locate `ALL_NAV_ITEMS`.

Expected: exactly
```ts
const ALL_NAV_ITEMS: NavItem[] = [
	{ href: '/settings/branding', label: 'Branding' },
	{ href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
	{ href: '/settings/locations', label: 'Lokasi' },
	{ href: '/settings/sub-users', label: 'Sub-user' }
];
```

- [ ] **Step 2: Append the entry** — replace that array with:

```ts
const ALL_NAV_ITEMS: NavItem[] = [
	{ href: '/settings/branding', label: 'Branding' },
	{ href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
	{ href: '/settings/locations', label: 'Lokasi' },
	{ href: '/settings/sub-users', label: 'Sub-user' },
	{ href: '/settings/spx-credentials', label: 'Akun SPX' }
];
```

If the layout's header comment enumerates which entries are main-account-only vs open, update it to note that `spx-credentials` is open (edit-gated), consistent with Branding/Locations/Sub-user.

- [ ] **Step 3: Type-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/+layout.svelte"
git commit -m "feat(frontend): append 'Akun SPX' nav entry to /settings shell (Fase 7k)"
```

---

### Task 5: `/settings/spx-credentials` page assembly

**Files:**
- Create: `Frontend/src/routes/(app)/settings/spx-credentials/+page.svelte`

**Interfaces:**
- Consumes: `fetchSpxCredentials`, `saveSpxCredential`, `deleteSpxCredential`, `testSpxLogin`, `type SpxCredential` from `$lib/api-spx-credentials`; `validateLabel`, `validateUsername`, `validatePassword`, `duplicateUsernameLabel` from `$lib/spx-credentials`; `ApiError` from `$lib/api`; `Eye`, `EyeOff` from `@lucide/svelte`; `PageProps` from `./$types` (provides `data.user.is_main_account`).
- Produces: the page. No new exports.

**Context:** Structurally the sub-users page: an add form inside a `<fieldset disabled={readOnly}>`, then a row list with per-row Test + Hapus buttons. Two banners (always-visible restart notice + read-only banner). The delete-and-recreate edit model means a same-label save overwrites the existing row in place. The Test button is layered-guarded per the Global Constraints. `data.user` comes from the settings layout's load — no `+page.server.ts` needed.

- [ ] **Step 1: Create the page** → `Frontend/src/routes/(app)/settings/spx-credentials/+page.svelte`

```svelte
<!-- Frontend/src/routes/(app)/settings/spx-credentials/+page.svelte -->
<!-- Manage the tenant's stored SPX agency credentials. GET /auth/spx-credentials has no
     permission gate (any session sees the list), only PUT/DELETE and POST /auth/spx-login
     are main-account-gated — so this page is edit-gated like /settings/branding and
     /settings/locations. There is no partial update or rename on the backend (PUT always
     upserts the whole username+password for a label), so editing is delete-and-recreate:
     re-submitting the add form with an existing label overwrites it. Saved credentials only
     take effect after reactor-core restarts (the poller bootstraps them once at boot) —
     hence the always-visible notice. The "Test Koneksi" button runs a REAL login against
     the live SPX upstream (up to ~80s), so it is click-only, in-flight-locked, 60s-cooldown
     guarded (client + server), and 90s-aborted; its result copy is deliberately honest,
     because the backend cannot distinguish wrong-password from SPX-down. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import { Eye, EyeOff } from '@lucide/svelte';
	import type { PageProps } from './$types';
	import {
		fetchSpxCredentials,
		saveSpxCredential,
		deleteSpxCredential,
		testSpxLogin,
		type SpxCredential
	} from '$lib/api-spx-credentials';
	import {
		validateLabel,
		validateUsername,
		validatePassword,
		duplicateUsernameLabel
	} from '$lib/spx-credentials';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let credentials = $state<SpxCredential[]>([]);
	let label = $state('');
	let username = $state('');
	let password = $state('');
	let showPassword = $state(false);
	let loading = $state(true);
	let saving = $state(false);
	let deletingLabel = $state<string | null>(null);
	let errorMsg = $state('');
	let successMsg = $state('');

	// Per-label transient UI state for the Test button.
	let testing = $state<Record<string, boolean>>({});
	let testResult = $state<Record<string, string>>({});
	let cooldownUntil = $state<Record<string, number>>({});
	let now = $state(0);

	const labelError = $derived(label === '' ? null : validateLabel(label));
	const usernameError = $derived(username === '' ? null : validateUsername(username));
	const passwordError = $derived(password === '' ? null : validatePassword(password));
	const dupLabel = $derived(duplicateUsernameLabel(username, credentials, label.trim()));
	const overwriteLabel = $derived(
		label.trim() !== '' && credentials.some((c) => c.label === label.trim()) ? label.trim() : null
	);
	const canSubmit = $derived(
		validateLabel(label) === null &&
			validateUsername(username) === null &&
			validatePassword(password) === null &&
			dupLabel === null
	);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			credentials = await fetchSpxCredentials();
		} catch {
			errorMsg = 'Gagal memuat kredensial SPX.';
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		load();
		// 1s ticker for the cooldown countdown. Reassigns `now` only while a
		// cooldown is actually pending, so an idle page never re-renders.
		const timer = setInterval(() => {
			const active = Object.values(cooldownUntil).some((t) => t > Date.now());
			if (active) now = Date.now();
		}, 1000);
		return () => clearInterval(timer);
	});

	function cooldownRemaining(l: string): number {
		const until = cooldownUntil[l] ?? 0;
		return until > now ? Math.ceil((until - now) / 1000) : 0;
	}

	async function handleCreate() {
		if (!canSubmit) return;
		saving = true;
		errorMsg = '';
		successMsg = '';
		try {
			const saved = await saveSpxCredential(label.trim(), username.trim(), password);
			const idx = credentials.findIndex((c) => c.label === saved.label);
			if (idx >= 0) credentials[idx] = saved;
			else credentials = [...credentials, saved];
			label = '';
			username = '';
			password = '';
			showPassword = false;
			successMsg = 'Kredensial tersimpan. Aktif setelah reactor-core direstart.';
		} catch (err) {
			errorMsg =
				err instanceof ApiError && err.status === 409
					? 'Label ini sedang dipakai, coba lagi.'
					: 'Gagal menyimpan kredensial.';
		} finally {
			saving = false;
		}
	}

	async function handleDelete(l: string) {
		if (!confirm(`Hapus kredensial "${l}"?`)) return;
		deletingLabel = l;
		errorMsg = '';
		try {
			await deleteSpxCredential(l);
			credentials = credentials.filter((c) => c.label !== l);
		} catch {
			errorMsg = 'Gagal menghapus kredensial.';
		} finally {
			deletingLabel = null;
		}
	}

	async function handleTest(l: string) {
		if (testing[l] || cooldownRemaining(l) > 0) return;
		testing = { ...testing, [l]: true };
		testResult = { ...testResult, [l]: '' };
		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), 90_000);
		try {
			const result = await testSpxLogin(l, controller.signal);
			testResult = {
				...testResult,
				[l]: result.ok
					? `Login berhasil (tier: ${result.tier}).`
					: 'Tidak berhasil membuat sesi. Periksa username/password, atau SPX sedang tidak bisa dihubungi.'
			};
		} catch (err) {
			let msg = 'Gagal menguji koneksi.';
			if (err instanceof DOMException && err.name === 'AbortError')
				msg = 'Test koneksi melebihi batas waktu (90 detik).';
			else if (err instanceof ApiError && err.status === 429)
				msg = 'Test koneksi baru saja dijalankan, coba lagi sebentar.';
			testResult = { ...testResult, [l]: msg };
		} finally {
			clearTimeout(timeout);
			testing = { ...testing, [l]: false };
			cooldownUntil = { ...cooldownUntil, [l]: Date.now() + 60_000 };
			now = Date.now();
		}
	}
</script>

<svelte:head>
	<title>Akun SPX — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	<div
		role="alert"
		class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
	>
		Kredensial hanya dimuat poller saat reactor-core dijalankan. Perubahan di sini baru aktif setelah
		restart.
	</div>

	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else}
		{#if readOnly}
			<div
				role="alert"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
			>
				Hanya akun utama yang dapat mengubah kredensial SPX.
			</div>
		{/if}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}
		{#if successMsg}
			<p role="status" aria-live="polite" class="text-[13px] text-accent">{successMsg}</p>
		{/if}

		<fieldset disabled={readOnly} class="flex flex-col gap-3 border-0 p-0">
			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Label</span>
				<input
					type="text"
					bind:value={label}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			{#if labelError}<span class="text-[11px] text-danger">{labelError}</span>{/if}
			{#if overwriteLabel}
				<span class="text-[11px] text-text-muted"
					>Label ini sudah ada — menyimpan akan menimpa kredensial lama.</span
				>
			{/if}

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Username</span>
				<input
					type="text"
					bind:value={username}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			{#if usernameError}<span class="text-[11px] text-danger">{usernameError}</span>{/if}
			{#if dupLabel}
				<span class="text-[11px] text-danger"
					>Username ini sudah dipakai label "{dupLabel}". Dua label dengan username sama akan
					bentrok saat poller start.</span
				>
			{/if}

			<div class="flex flex-col gap-1">
				<label
					for="new-spx-password"
					class="text-[10px] font-body text-text-muted uppercase tracking-wide">Password</label
				>
				<div class="relative">
					<input
						id="new-spx-password"
						type={showPassword ? 'text' : 'password'}
						bind:value={password}
						autocomplete="new-password"
						class="w-full min-h-[40px] px-2.5 pr-12 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
					<button
						type="button"
						onclick={() => (showPassword = !showPassword)}
						aria-pressed={showPassword}
						class="absolute inset-y-0 right-0 flex items-center px-3 min-w-[44px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-md"
					>
						<span class="sr-only">{showPassword ? 'Sembunyikan password' : 'Tampilkan password'}</span>
						{#if showPassword}
							<EyeOff size={16} aria-hidden="true" />
						{:else}
							<Eye size={16} aria-hidden="true" />
						{/if}
					</button>
				</div>
				{#if passwordError}<span class="text-[11px] text-danger">{passwordError}</span>{/if}
			</div>

			<button
				type="button"
				onclick={handleCreate}
				disabled={saving || !canSubmit}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{saving ? 'Menyimpan…' : 'Simpan Kredensial'}
			</button>
		</fieldset>

		{#if credentials.length === 0}
			<p class="text-[13px] text-text-muted">Belum ada kredensial SPX.</p>
		{:else}
			<ul class="flex flex-col gap-2">
				{#each credentials as cred (cred.label)}
					{@const remaining = cooldownRemaining(cred.label)}
					<li class="flex flex-col gap-1 rounded-lg border border-border bg-bg-surface p-3">
						<div class="flex items-center justify-between gap-2">
							<span class="text-[13px] font-body text-text-primary">
								{cred.label}
								<span class="text-text-muted">({cred.username})</span>
							</span>
							<div class="flex items-center gap-2">
								<button
									type="button"
									disabled={readOnly || testing[cred.label] || remaining > 0}
									onclick={() => handleTest(cred.label)}
									class="min-h-[36px] px-2 text-[11px] text-accent disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
								>
									{#if testing[cred.label]}
										Menguji…
									{:else if remaining > 0}
										Tunggu {remaining}s
									{:else}
										Test
									{/if}
								</button>
								<button
									type="button"
									disabled={readOnly || deletingLabel === cred.label}
									onclick={() => handleDelete(cred.label)}
									class="min-h-[36px] px-2 text-[11px] text-danger disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
								>
									{deletingLabel === cred.label ? 'Menghapus…' : 'Hapus'}
								</button>
							</div>
						</div>
						{#if testResult[cred.label]}
							<span role="status" aria-live="polite" class="text-[11px] text-text-muted"
								>{testResult[cred.label]}</span
							>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
```

- [ ] **Step 2: Type-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 3: Build (catches SSR-time crashes the type-checker misses)**

Run: `cd Frontend && pnpm build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/spx-credentials/+page.svelte"
git commit -m "feat(frontend): /settings/spx-credentials page assembly (Fase 7k)"
```

---

### Task 6: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/settings-spx-credentials.spec.ts`

**Interfaces:**
- Consumes: the running dev stack (real `reactor-core` + Postgres + Redis behind Vite's proxy), the seeded `e2e-test-user` (main) / `e2e-readonly-user` (non-main) accounts (password `correct-horse-battery-staple`), and the page from Task 5.
- Produces: the e2e suite. Final verification runs the full backend + frontend gates.

**Context:** Real-stack, nothing mocked — same setup as `settings-sub-users.spec.ts`. **The Test button is NOT clicked in e2e** — it performs a real login against the production SPX upstream and could trigger an account lockout; we assert only its presence and its `disabled` guard behavior via DOM state. Self-cleaning: every credential this suite creates uses a `Date.now()`-suffixed label and is deleted within its own test. The backend cooldown itself is covered by `spx_login_routes.rs` (Task 1), which uses a mock SPX server.

- [ ] **Step 1: Write the e2e spec** → `Frontend/tests/settings-spx-credentials.spec.ts`

```ts
// Frontend/tests/settings-spx-credentials.spec.ts
//
// REAL end-to-end proof of Fase 7k's /settings/spx-credentials page. Same real-stack setup
// as tests/settings-sub-users.spec.ts — real reactor-core behind Vite's proxy, real Postgres,
// real Redis. Nothing is mocked.
//
// The "Test Koneksi" button is DELIBERATELY never clicked here: it performs a real login
// against the production SPX upstream and could lock the tenant's SPX account. We assert only
// that the button renders and is disabled for non-main-account users. The backend cooldown is
// covered by Backend/crates/api-gateway/tests/spx_login_routes.rs against a mock SPX server.
//
// Every credential this suite creates uses a Date.now()-suffixed label and is deleted within
// its own test; it never touches shared fixture rows.

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /settings/spx-credentials redirects to /login', async ({ page }) => {
	await page.goto('/settings/spx-credentials');
	await expect(page).toHaveURL(/\/login/);
});

test('main account sees the Akun SPX nav entry and the restart notice', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByRole('link', { name: 'Akun SPX' })).toBeVisible();

	await page.goto('/settings/spx-credentials');
	await expect(page.getByText('Perubahan di sini baru aktif setelah restart.')).toBeVisible({
		timeout: 10_000
	});
});

test('creating a credential persists it, exposes a Test button, then it can be deleted', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');
	await expect(page.getByLabel('Label')).toBeVisible({ timeout: 10_000 });

	const uniqueLabel = `e2e-spx-${Date.now()}`;
	await page.getByLabel('Label').fill(uniqueLabel);
	await page.getByLabel('Username').fill(`${uniqueLabel}-user`);
	await page.getByLabel('Password').fill('a-valid-password');
	await page.getByRole('button', { name: 'Simpan Kredensial' }).click();
	await expect(page.getByText(uniqueLabel)).toBeVisible({ timeout: 10_000 });

	await page.reload();
	const row = page.locator('li', { hasText: uniqueLabel });
	await expect(row).toBeVisible({ timeout: 10_000 });
	// The Test button renders and is enabled — but we never click it (real SPX login).
	await expect(row.getByRole('button', { name: 'Test' })).toBeEnabled();

	page.once('dialog', (dialog) => dialog.accept());
	await row.getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(uniqueLabel)).toBeHidden({ timeout: 10_000 });
});

test('a duplicate-username-different-label entry is blocked and fires no PUT', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');
	await expect(page.getByLabel('Label')).toBeVisible({ timeout: 10_000 });

	// First, create a real credential to clash against.
	const baseLabel = `e2e-dup-${Date.now()}`;
	const sharedUser = `${baseLabel}-user`;
	await page.getByLabel('Label').fill(baseLabel);
	await page.getByLabel('Username').fill(sharedUser);
	await page.getByLabel('Password').fill('a-valid-password');
	await page.getByRole('button', { name: 'Simpan Kredensial' }).click();
	await expect(page.getByText(baseLabel)).toBeVisible({ timeout: 10_000 });

	// Now attempt a DIFFERENT label with the SAME username — must be blocked client-side.
	let putCount = 0;
	await page.route('**/auth/spx-credentials/**', (route) => {
		if (route.request().method() === 'PUT') putCount++;
		route.continue();
	});
	await page.getByLabel('Label').fill(`${baseLabel}-two`);
	await page.getByLabel('Username').fill(sharedUser.toUpperCase()); // case-insensitive clash
	await page.getByLabel('Password').fill('another-password');
	await expect(page.getByText(`Username ini sudah dipakai label "${baseLabel}"`)).toBeVisible();
	await expect(page.getByRole('button', { name: 'Simpan Kredensial' })).toBeDisabled();
	expect(putCount).toBe(0);

	// Clean up the one real row created.
	await page.unroute('**/auth/spx-credentials/**');
	await page.reload();
	page.once('dialog', (dialog) => dialog.accept());
	await page.locator('li', { hasText: baseLabel }).getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(baseLabel)).toBeHidden({ timeout: 10_000 });
});

test('typing an existing label shows the overwrite note', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');
	await expect(page.getByLabel('Label')).toBeVisible({ timeout: 10_000 });

	const uniqueLabel = `e2e-ovr-${Date.now()}`;
	await page.getByLabel('Label').fill(uniqueLabel);
	await page.getByLabel('Username').fill(`${uniqueLabel}-user`);
	await page.getByLabel('Password').fill('a-valid-password');
	await page.getByRole('button', { name: 'Simpan Kredensial' }).click();
	await expect(page.getByText(uniqueLabel)).toBeVisible({ timeout: 10_000 });

	await page.getByLabel('Label').fill(uniqueLabel);
	await expect(page.getByText('menyimpan akan menimpa kredensial lama')).toBeVisible();

	// Clean up.
	page.once('dialog', (dialog) => dialog.accept());
	await page.locator('li', { hasText: uniqueLabel }).getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(uniqueLabel)).toBeHidden({ timeout: 10_000 });
});

test('non-main-account sees the list with a disabled add form and disabled row buttons', async ({
	page
}) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');

	const labelInput = page.getByLabel('Label');
	await expect(labelInput).toBeVisible({ timeout: 10_000 });
	await expect(labelInput).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Simpan Kredensial' })).toBeDisabled();
});
```

- [ ] **Step 2: Ensure the dev stack is up, then run the new e2e file standalone**

Run: `cd Frontend && pnpm exec playwright test tests/settings-spx-credentials.spec.ts --workers=1`
Expected: 6/6 pass. (If a test lands on `/login` unexpectedly, it is the known shared-`reactor-core` login-rate-limiter flake — restart `reactor-core` and rerun this file standalone to confirm; it is not a regression.)

- [ ] **Step 3: Full frontend gate**

Run: `cd Frontend && pnpm check && pnpm vitest run && pnpm build`
Expected: `pnpm check` 0/0; all vitest suites pass (including the two new lib suites); build succeeds.

- [ ] **Step 4: Full backend gate**

Run (foreground, do NOT background):
```
cd Backend && DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 cargo test --workspace
cd Backend && cargo clippy --workspace --all-targets -- -D warnings
cd Backend && cargo deny check
```
Expected: all green. The only acceptable failure is a pre-existing flake unrelated to this branch's diff (confirm via a standalone rerun of the named test); the new `second_call_within_cooldown_is_rate_limited` and all four prior `spx_login_routes` tests must pass.

- [ ] **Step 5: Commit**

```bash
git add Frontend/tests/settings-spx-credentials.spec.ts
git commit -m "test(fase-7k): /settings/spx-credentials e2e — full workspace + frontend verification"
```

---

## Self-Review Notes

- **Spec coverage:** RBAC/edit-gating (Tasks 4,5,6), password-never-returned + existence-as-signal (Task 3 comment, Task 5 has no prefill), delete-and-recreate + overwrite note (Task 5, Task 6 overwrite test), always-visible restart notice (Task 5, Task 6 asserts it), trim + duplicate-username guard (Task 2, Task 5, Task 6 dup test), Test-button layered guards (Task 5, Global Constraints), server-side cooldown (Task 1), honest test copy (Task 5), tokens/a11y/onMount/Indonesian (Global Constraints, applied throughout) — all mapped to a task.
- **Placeholder scan:** every code step contains complete code; every run step names an exact command and expected result. No TBD/TODO/"similar to".
- **Type consistency:** `SpxCredential { label, username }` and `SpxLoginResult { ok, tier }` are defined once in Task 3 and consumed identically in Tasks 2 and 5. Function names (`fetchSpxCredentials`/`saveSpxCredential`/`deleteSpxCredential`/`testSpxLogin`, `validateLabel`/`validateUsername`/`validatePassword`/`duplicateUsernameLabel`) match across their definition and every call site. `testSpxLogin`'s `signal?: AbortSignal` param (Task 3) matches the Task 5 call `testSpxLogin(l, controller.signal)`.
- **Known environmental notes:** the shared-`reactor-core` login-rate-limiter flake can surface on full-suite e2e runs (mitigation: restart `reactor-core`, rerun the affected file standalone); the new Redis cooldown key uses a per-test-unique tenant so it never bleeds across `spx_login_routes` tests.
- **Backend-test caution (carried from the spec):** the implementer of Task 1 must read the current `spx_login_routes.rs` before adding the test (done in the plan against the real file) — the four existing tests each fire one login to a unique tenant, so the cooldown does not break them.
