# Fase 7k — `/settings/spx-credentials` Design

**Status:** approved (brainstorming, 2026-07-21)
**Scope:** the fifth and final `/settings/*` sub-phase — a management page for the tenant's stored SPX agency credentials, plus the first frontend consumer of the `POST /auth/spx-login/{label}` connectivity-test endpoint (deferred since Fase 7h).

This is the **first `/settings/*` sub-phase that touches the backend** — every prior one (7g–7j) was pure frontend. The backend change is a small, single-purpose rate-limit guard on the test-connection endpoint; see §3.

---

## Why this page is different from its four siblings

All facts below were verified by reading `api-gateway` source directly (`routes/spx_credentials.rs`, `routes/spx_login.rs`, `auth/permission.rs`, `bin/reactor-core/src/main.rs`, `store/src/agency_credentials.rs`), **not** from design docs — an earlier note in this project propagated a wrong RBAC claim by trusting a plan doc, and the 6b plan doc's own opening line about these routes is self-contradictory. Every claim here is source-grounded.

1. **RBAC is edit-gated (like Branding/Locations/Sub-users), not content-gated (like Bot).**
   - `GET /auth/spx-credentials` — session auth only, **no permission gate** (`spx_credentials.rs` `list` has no `require_permission` call; router applies only `session_auth`). Any tenant member — main account or sub-user — sees the list.
   - `PUT /auth/spx-credentials/{label}` and `DELETE /auth/spx-credentials/{label}` — `require_permission(Permission::ManageSpxCredentials)` as the first statement of each handler → main-account only (403 for sub-users).
   - `POST /auth/spx-login/{label}` — also `ManageSpxCredentials`-gated (main-account only).

2. **The stored password is never returned in any form.** The only response struct is `CredentialSummary { label: String, username: String }`. No plaintext, no mask, no `has_password`/`is_set` boolean, no `id`/timestamps. **The existence of a row IS the "password is set" signal** — a row cannot exist without a non-empty ciphertext (column `NOT NULL`, and every `PUT` requires a non-empty password). This forecloses the Bot page's "leave blank to keep existing" pattern: there is nothing to keep, because `PUT` always re-encrypts from the submitted password.

3. **`PUT` is a full upsert keyed by `(tenant_id, label)`.** Both `username` and `password` are required non-empty on every call (missing key → 422; blank → 400 `"username and password are required"`). There is no partial update — you cannot change the username without also re-sending the password. This is why the chosen edit model is **delete-and-recreate**, not inline edit (see §2).

4. **Saving credentials has NO runtime effect until `reactor-core` restarts.** The poller bootstraps one task per credential exactly once, at process start (`bin/reactor-core/src/main.rs` calls `store::agency_credentials::list_all` at boot). The HTTP handlers only touch the `agency_credentials` row — there is no watch channel, no reload signal, no `accounts`-map mutation. Consequences the UI MUST communicate:
   - A new credential does **not** spawn a poller.
   - A `DELETE` does **not** stop a running poller (the spawned task keeps polling with the password it already holds in memory).
   - Editing a password does **not** re-login the poller.
   - Any copy implying "connected"/"active" after save would be a lie.

5. **Username is stored untrimmed but the poller keys accounts by `username.trim().to_lowercase()`.** At boot, two credentials whose usernames collapse to the same `account_id` cause the second to be **skipped with a warning**. So `"Agency1 "` and `"agency1"` are a silent-footgun collision. The frontend trims usernames and blocks same-username-different-label duplicates client-side (§4).

6. **`label` is an unvalidated path segment.** No length cap, no charset check, not trimmed; a label containing `/` splits the path and 404s. The frontend owns label validation entirely.

### The connectivity-test endpoint is dangerous by default

`POST /auth/spx-login/{label}` (source: `routes/spx_login.rs`, `spx-client/src/login.rs`):

- Uses the tenant's **already-stored** credentials (no body). Requires the credential to be saved first (404 otherwise).
- Performs a **real login against the live SPX upstream** (`SPX_BASE_URL`, default `https://logistics.myagencyservice.id`) using the same production `SpxClient` the poller uses.
- **One click = up to 8 real credential submissions**: 5 API-login variants tried sequentially, then a 3-request form-login fallback. On success, up to 5 more authenticated page fetches (`fetch_spx_cid`).
- **No rate limit, no cooldown, no in-flight lock, and no early-exit on an SPX 429/403** anywhere in the current stack. Per-request timeout is 10s; **worst-case wall time is 80–100s** with no server-side timeout layer.
- Returns `200 {"ok": bool, "tier": "api" | "form" | null}` for every SPX outcome. A failed login is `200 {ok:false}`, **not** a 4xx. The response **cannot distinguish** wrong-password vs. SPX-unreachable vs. SPX-rate-limited — all three are byte-identical `{ok:false, tier:null}`.
- Persists nothing (no cookie storage, no session write) — the blast radius is entirely on the SPX side, but that blast radius is real: spamming the button can trigger account lockout / captcha / OTP challenge on the tenant's SPX account, which **would also take the poller down**, since it shares the same credentials and upstream.

Two user decisions were taken during brainstorming (via AskUserQuestion):
- **Test-connection protection:** add a **server-side cooldown** (not client-only), because client throttling is trivially bypassed and the harm is real.
- **Edit model:** **delete-and-recreate** (no inline edit), matching Sub-users/Locations and honest about the backend's upsert-only reality.

---

## 1. Page shape and RBAC treatment

Multi-row management list, structurally identical to `/settings/sub-users`:
- An **add form** at the top (label, username, password with show/hide toggle).
- A **list of existing rows** below, each with a **Test** button and a **Hapus** (delete) button.
- Edit-gated: the list renders for every session; the add form's `<fieldset>` and each row's write buttons are `disabled` for non-main-account users.

Two banners:
- **Always-visible operational notice** (every session, not just read-only): explains that saved credentials only take effect after a `reactor-core` restart (fact #4). Styled as the standard boxed info banner (`bg-accent/10 text-accent border-accent/30`, `role="alert"`). Copy: *"Kredensial hanya dimuat poller saat reactor-core dijalankan. Perubahan di sini baru aktif setelah restart."*
- **Read-only banner** for non-main-account, standard family copy: *"Hanya akun utama yang dapat mengubah kredensial SPX."*

Nav entry (one-line append to `ALL_NAV_ITEMS`, no `mainAccountOnly` flag): `{ href: '/settings/spx-credentials', label: 'Akun SPX' }`.

Page `<title>`: `Akun SPX — TOWER`.

---

## 2. Edit model: delete-and-recreate

No inline edit, no per-row "change password" affordance. Rationale: the backend has no partial update and no rename; `PUT` always upserts the whole `(username, password)` for a label. To rotate a password or fix a username, the user re-submits the add form with the **same label** — the backend `PUT` overwrites it. The UI surfaces this explicitly: when the typed label matches an existing row, an inline note appears — *"Label ini sudah ada — menyimpan akan menimpa kredensial lama."* — and the save is **not** blocked (overwrite is the intended rotation path). To remove a credential entirely, use the row's Hapus button (native `confirm()`, hard-deletes the row → 204).

This is the least code, least state, and the most honest mapping to the backend.

---

## 3. Backend change — server-side test-connection cooldown

The single backend task. Adds a per-`(tenant, label)` cooldown to `POST /auth/spx-login/{label}` in `routes/spx_login.rs`. No new error type or wiring is needed — `ApiError::TooManyRequests(String)` (→ 429) and `AppState.redis: redis::aio::ConnectionManager` already exist, and `auth/otp.rs` already establishes the exact `SET key val NX EX secs` atomic-claim precedent.

**Ordering (critical):**

```
1. require_permission(ManageSpxCredentials)      // 403 — never touches Redis or SPX
2. find_by_label(...) → NotFound                 // 404 — never touches Redis or SPX
3. decrypt password (existing)                   // 500 on bad nonce/crypto
4. claim cooldown:  SET spx:spx_login_rl:{tenant_id}:{label} "1" NX EX CLAIM_TTL_SECS
     - if NOT acquired  → return 429 TooManyRequests("test koneksi sedang berjalan atau baru saja dijalankan, coba lagi sebentar")
5. run the SPX login (existing, up to ~80–100s)
6. after login finishes (success OR failure):  SET spx:spx_login_rl:{tenant_id}:{label} "1" EX TEST_COOLDOWN_SECS
     - resets the window to TEST_COOLDOWN_SECS measured from COMPLETION, not from start
7. return 200 {ok, tier}
```

Design points:
- Claiming **after** the 403/404 checks means requests that never reach SPX don't burn a cooldown — and the existing `spx_login_routes.rs` tests (which exercise 403/404/nonexistent-label paths, and a mocked-success path) are unaffected by the cooldown for the not-found/forbidden cases. The mocked-success test path WILL now write a cooldown key; it uses a per-test-unique tenant/label so this does not bleed across tests, but the test file must be checked and, if it fires two logins for the same `(tenant,label)` in one test, updated to expect the 429 or use distinct labels. (Implementer verifies against the actual test file.)
- The `NX` claim at step 4 doubles as an **in-flight lock**: a second click while the first request is still running fails the `NX` and returns 429 immediately, instead of launching a second real login storm. Its TTL is `CLAIM_TTL_SECS = 120` (not 60) so it outlives the worst-case ~80–100s login — a 60s claim TTL would lapse mid-login during a slow/degraded SPX (exactly when lockout risk is highest), reopening the storm window. The post-login refresh at step 6 re-anchors the window to `TEST_COOLDOWN_SECS = 60` from completion. (This split was raised by both the Task-1 and security reviews and applied before merge.)
- Key is per-`(tenant, label)` because each label is a distinct SPX account with its own lockout risk.
- Two module consts: `TEST_COOLDOWN_SECS = 60` (window from completion) and `CLAIM_TTL_SECS = 120` (in-flight lock TTL, ≥ worst-case login).
- Redis errors during the claim map to `Internal` (500), same as `otp.rs` — fail closed is acceptable here (a Redis outage blocking test-connection is far better than an unguarded login storm).
- A `redis::RedisError` on the final step-6 `SET` should not fail the request — the login already succeeded/failed and the result is what the user wants; log and ignore. (The NX claim at step 4 already provides the in-flight guarantee; step 6 is best-effort window-refresh.)

**Test (backend, new):** a route test asserting that two `POST /auth/spx-login/{label}` calls in quick succession for the same saved credential return `200` then `429`, and that a 403 (sub-user) or 404 (missing label) call does not consume the cooldown (a subsequent main-account call still runs). Uses the existing `spx_login_routes.rs` harness/mock SPX server.

---

## 4. `lib/spx-credentials.ts` — pure logic (TDD)

No fetch, no DOM. Functions:

- `validateLabel(label: string): string | null` — returns an Indonesian error string or `null`. Rules: non-empty after trim (`'Label wajib diisi'`); no `/` character (`'Label tidak boleh mengandung "/"'`, because it splits the URL path → 404); max 64 chars (`'Label maksimal 64 karakter'`). The backend validates none of this, so the client owns it.
- `validateUsername(username: string): string | null` — non-empty after trim (`'Username wajib diisi'`).
- `validatePassword(password: string): string | null` — non-empty (`'Password wajib diisi'`). The backend rejects only empty; there is no length floor for SPX credentials (unlike sub-user passwords), so we do not invent one.
- `duplicateUsernameLabel(username: string, existing: SpxCredential[], currentLabel: string): string | null` — returns the conflicting label if another row (a row whose `label !== currentLabel`) has the same `username.trim().toLowerCase()`, else `null`. This guards fact #5: two labels with colliding normalized usernames silently break at poller boot. The `currentLabel` exclusion lets a same-label overwrite (password rotation) pass.

`SpxCredential` type (`{ label: string; username: string }`) is defined in the api module (§5) and imported here for the array param.

Unit tests: each validator's boundary cases; `duplicateUsernameLabel` for match / no-match / case-and-whitespace-insensitive match / same-label-excluded.

---

## 5. `lib/api-spx-credentials.ts` — typed REST layer

Wire shape verified against `routes/spx_credentials.rs` (`CredentialSummary`, `UpsertCredential`) and `routes/spx_login.rs` (`SpxLoginResult`) — snake_case throughout, no `rename_all`. Here `label`/`username` are identical in both cases, so no case mapping is needed, but the module still documents the source in its header comment per convention.

- `type SpxCredential = { label: string; username: string }` (public; wire and TS are identical).
- `type SpxLoginResult = { ok: boolean; tier: 'api' | 'form' | null }`.
- `fetchSpxCredentials(): Promise<SpxCredential[]>` — `GET /auth/spx-credentials`, raw `fetch` (`credentials: 'include'`), throws `new ApiError(res.status, 'failed to fetch spx credentials')` on `!res.ok`.
- `saveSpxCredential(label: string, username: string, password: string): Promise<SpxCredential>` — `PUT /auth/spx-credentials/${encodeURIComponent(label)}`, raw `fetch` (apiPost is POST-only), body `{ username, password }` (snake_case identical), returns the parsed `CredentialSummary`. Throws `ApiError` with real `.status` on `!res.ok`.
- `deleteSpxCredential(label: string): Promise<void>` — `DELETE /auth/spx-credentials/${encodeURIComponent(label)}`, raw `fetch`, expects 204, never calls `res.json()`. Throws `ApiError` on `!res.ok`.
- `testSpxLogin(label: string): Promise<SpxLoginResult>` — `POST /auth/spx-login/${encodeURIComponent(label)}` via `apiPost<SpxLoginResult>` (real POST route, empty body). Note: `apiPost` throws `ApiError` on non-2xx already; the page branches on `.status` (429 → cooldown message).

Convention (from every sibling module): `ApiError.message` is a fixed generic English string, never parsed from the response body; pages branch on `err.status` for Indonesian copy.

Unit tests: GET maps array; each write asserts exact method + URL (including `encodeURIComponent` on a label with a space) + exact request body via `JSON.parse`; non-ok throws `ApiError` preserving `.status`; DELETE asserts no body-parse; `testSpxLogin` maps `{ok, tier}` and surfaces 429 as `ApiError` with `status: 429`.

---

## 6. Page — `routes/(app)/settings/spx-credentials/+page.svelte`

State: `credentials`, `label`, `username`, `password`, `showPassword`, `loading`, `saving`, `deletingLabel`, `errorMsg`, `successMsg`, plus a `testing` map/record keyed by label and a `cooldownUntil` record keyed by label (client-side cooldown display). `readOnly = $derived(!data.user.is_main_account)`.

Load: `onMount(load)` — SSR-safe (relative-path fetch has no origin during Node SSR; every prior phase's plan re-learned this, so it is called out here up front). `load()` sets `loading`, clears `errorMsg`, fetches, sets `loading=false` in `finally`.

Add form (inside `<fieldset disabled={readOnly}>`): label input, username input, password input + show/hide toggle (exact sub-users markup: explicit `id="new-spx-password"` + sibling `<label for>`, `Eye`/`EyeOff` size 16, `aria-pressed`, `sr-only` toggle label). Inline field errors from §4 validators. Inline overwrite note when `label` matches an existing row. Submit button disabled unless all validators pass and no duplicate-username-different-label conflict. On submit: `saveSpxCredential`, then merge/replace the row in `credentials` (upsert by label — a same-label save replaces the existing entry, not appends), clear the form, set `successMsg`.

Row list: each row shows `label` + `username`, a **Test** button, and a **Hapus** button (both `disabled={readOnly}` plus their own in-flight guards). Delete: native `confirm()`, `page.once('dialog', accept)` in tests, `deleteSpxCredential`, remove from list.

**Test button — layered client guards** (backend cooldown is the real protection, but the client must not fire needless storms):
- Only one test allowed globally at a time is NOT required (per-label cooldown suffices), but each label's button is `disabled` while `testing[label]` is true or while `cooldownUntil[label]` is in the future.
- `AbortController` with a **90s** timeout (the backend has no timeout layer; worst case ~80s).
- On completion (success, failure, 429, or abort): set `cooldownUntil[label] = now + 60_000` and show a countdown on the button (`Tunggu {n}s`). A 429 from the server (someone else, or a stale in-flight) also sets the cooldown.
- Result copy: success → `Login berhasil (tier: ${tier})` as a transient success line; `{ok:false}` → *"Tidak berhasil membuat sesi. Periksa username/password, atau SPX sedang tidak bisa dihubungi."* (honest: the backend genuinely cannot distinguish the cause); 429 → *"Test koneksi baru saja dijalankan, coba lagi sebentar."*; other errors → generic *"Gagal menguji koneksi."*
- **Never** auto-runs on mount, on an interval, or on any reactive trigger. It fires only from an explicit click.

All styling uses the established tokens (input `min-h-[40px] … rounded-md … focus-visible:ring-2 focus-visible:ring-accent`, primary button `min-h-[44px]`, small danger button `min-h-[36px] text-danger`, banners `role="alert"`/`role="status"` with `aria-live="polite"`, real `…` ellipsis). No `Date.now()` sign or numeric-format footguns apply here (no numbers).

---

## 7. Tests — E2E (Playwright), `Frontend/tests/settings-spx-credentials.spec.ts`

Real-stack, same setup as sibling specs (real `reactor-core`, real Postgres, nothing mocked). Reuses `e2e-test-user` (main) / `e2e-readonly-user` (non-main), password `correct-horse-battery-staple`. Local `login()` helper duplicated per file (project precedent — do not extract).

Tests:
- Unauthenticated visit to `/settings/spx-credentials` redirects to `/login`.
- Main-account sees the "Akun SPX" nav entry and the real list.
- Save + delete round-trip: create a credential with a `` `e2e-spx-${Date.now()}` ``-suffixed label (self-cleaning, deleted by end of test), visible after reload, then deleted.
- A duplicate-username-different-label entry shows the inline conflict error and fires **no** `PUT` (verified via `page.route('**/auth/spx-credentials/**', ...)` counting POST/PUT — here PUT; assert 0). Uses the seeded credential's username if one exists, else creates one first then attempts a second label with the same username.
- Overwrite note: typing an existing label shows the "akan menimpa" note.
- Non-main-account (`e2e-readonly-user`) sees the real list with the add-form fieldset disabled and per-row Test/Hapus buttons disabled.

**The Test button is NOT exercised in e2e** — it performs a real login against the production SPX upstream and could trigger account lockout. E2E asserts only the button's presence and its `disabled`/cooldown guard behavior via DOM state, never a click that reaches the network. This restriction is stated in the plan and the test file's header comment.

**Backend cooldown test** lives in `spx_login_routes.rs` (§3), not in Playwright — it uses the existing mock SPX server, so it can safely fire real (mocked) logins.

---

## Open Questions for the Implementer

None. RBAC shape, the no-runtime-effect reality, the delete-and-recreate edit model, the server-side cooldown design, and the honest test-result copy were all resolved during brainstorming (two via explicit user decision). Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path — in particular, the exact current contents of `spx_login_routes.rs` must be read before adding the cooldown so the new key doesn't break an existing multi-login test.

## Tracked, deliberately-deferred follow-ups from implementation + reviews

A whole-branch quality review (opus) and a dedicated security review (opus, parallel) both returned **Ready to merge: Yes**, zero Critical/Important. Summary of what was found and how it was handled:

- **Cooldown ticker (Critical, FIXED pre-merge, commit `69de197`).** The plan's Task-5 `+page.svelte` gated the 1s countdown tick on `t > Date.now()`, which skipped the falling-edge tick and froze the displayed `now` ~1s short of the deadline — the "Test Koneksi" button stuck on "Tunggu 1s" permanently until a page reload, breaking the feature after one use. Fixed by comparing against the displayed `now` (`t > now`), which preserves the idle-no-rerender optimization while letting the falling-edge tick re-enable the button. Verified with a numeric simulation and re-confirmed correct by the whole-branch review across single/multiple/idle/re-arm cases.
- **In-flight lock TTL tail (security Low + Task-1 Minor, FIXED pre-merge).** The initial `NX` claim originally used `EX 60`, shorter than the worst-case ~80–100s login, so a >60s (slow/degraded SPX) login left a window where a second click could start a second overlapping real login storm — the guard degrading exactly when lockout risk is highest. Both reviewers flagged it. Fixed by claiming with `CLAIM_TTL_SECS = 120` (≥ worst case) while keeping the completion refresh at `TEST_COOLDOWN_SECS = 60`, closing the tail with no downside.
- **E2E test-name honesty (quality Minor, FIXED pre-merge).** The non-main-account test was titled "…disabled add form and disabled row buttons" but asserted only the add-form controls (the per-row Test/Hapus buttons need a credential row to exist for the self-cleaning shared tenant, which isn't guaranteed at run time). Renamed to reflect what it asserts, with a comment noting the row-button read-only path is covered structurally (identical binding to the tested Locations/Sub-user siblings).
- **Cooldown key name (quality Minor, spec reconciled).** The implemented Redis key is `spx:spx_login_rl:{tenant_id}:{label}` (following `otp.rs`'s `spx:aa_otp_rl:` convention), not the spec's earlier placeholder `spx-login-cooldown:`. The code is the correct/consistent form; this doc's §3 was updated to match.

Nothing deferred blocks or degrades the phase; the security review confirmed the plaintext password never leaves the PUT body / is cleared after save / is never logged or persisted, RBAC is enforced server-side as the first statement of every write/test handler, and the cooldown key + all lookups are strictly scoped to the server-derived session `tenant_id` (no injection / IDOR).
