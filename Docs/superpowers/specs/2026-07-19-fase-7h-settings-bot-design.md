# Fase 7h: `/settings/bot` — Design

**Status:** Approved for implementation.

## Context

Second of five `/settings/*` sub-phases (see [[project-tower-workflow]] / Fase 7g's design doc for the full decomposition decision). Fase 7g built the shared `/settings` shell (secondary nav) plus `/settings/branding`. This phase adds `/settings/bot` — a form over the already-existing, never-consumed `GET/PUT /bot/settings` endpoint (Fase 6d), which configures the tenant's WAHA (WhatsApp HTTP API) bridge and n8n webhook used for OTP delivery and accept/reject notifications.

**This resource has a genuinely different RBAC shape from Branding**: `GET /bot/settings` itself requires `Permission::ManageBotSettings` (main-account only) — Branding/Locations/Sub-users/SPX-Credentials all allow `GET` for any authenticated session. This is a deliberate backend choice (WAHA connection info, even with the API key masked, is treated as sensitive enough that even reading it requires main-account) that this phase's frontend must respect: the page cannot be merely edit-gated like Branding, it must be **content-gated**, matching `/activity`'s "Log Bot" tab precedent from Fase 7f.

**The dev tenant's `waha_settings` row is shared, real state other e2e suites depend on**: `Frontend/tests/rules.spec.ts`'s OTP arm-flow test requires a genuinely-decryptable `api_key_ciphertext_b64` to exist (a prior Fase 7d finding: `otp.rs::load_bot_settings` unconditionally decrypts it, so a malformed placeholder 500s that test). Fase 7h's own e2e suite will exercise the real save path (including key rotation) against this same shared row — safe as long as every test that mutates it restores the pre-test values afterward, so it never leaves the row in a state that could break `rules.spec.ts` or any other suite reading it later.

## Scope

**In scope:**
- `/settings/bot` page: view/edit `enabled`, `webhook_url`, `wa_number`, `wa_group`, `waha_url`, `waha_session`, and the write-only `waha_api_key` (masked — never round-tripped, blank-on-load, blank-on-save-means-keep-existing) via `GET/PUT /bot/settings`.
- The "Bot" entry in the `/settings` shell's secondary nav, shown ONLY for main-account sessions (extending `+layout.svelte`'s nav array to be conditional — it was a static list in 7g since Branding needed no such gating).
- A direct-navigation guard: a non-main-account session that navigates straight to `/settings/bot` (bypassing the hidden nav link) sees a clear "tidak punya akses" message, not a raw error or an empty form — the real enforcement is the backend's 403, this is just presenting that outcome cleanly.

**Out of scope (deliberately deferred, not silently dropped):**
- **A "test WAHA connection" button.** No backend endpoint exists for this (unlike SPX Credentials, which will get `POST /auth/spx-login` in Fase 7k) — inventing new backend behavior is out of scope for what's meant to be a pure-frontend phase. If this becomes a real need, it's a backend-first addition for a future phase.
- **Client-side SSRF validation.** The backend's `is_safe_outbound_url` (`Backend/crates/api-gateway/src/routes/bot.rs`) is genuinely complex security logic (rejects localhost/private-IP/link-local/cloud-metadata hostnames, embedded credentials, non-http(s) schemes) — duplicating any part of it client-side would risk drift between the two checks and create a false sense of client-side safety for what is, and must remain, an exclusively backend-enforced boundary. The frontend does only a basic well-formed-URL syntax check (via the `URL` constructor) as a completeness nicety, nothing more.
- **Bot log viewing/clearing** — already fully built in Fase 7f's `/activity` page (`GET/DELETE /bot/logs`). This phase touches only `/bot/settings`.

## Backend

No changes. Existing surface (`Backend/crates/api-gateway/src/routes/bot.rs`):

- **`GET /bot/settings`** — `session_auth` + `Permission::ManageBotSettings` (main-account only, on BOTH verbs — the one deliberate exception to this crate's usual "GET = any session" convention). Returns `BotSettingsResponse`: `{enabled: bool, webhook_url: string, wa_number: string, wa_group: string, waha_url: string, waha_session: string, waha_api_key_set: bool}`. The real API key is NEVER included in any response — only whether one is currently configured.
- **`PUT /bot/settings`** — same gate. Body (`BotSettingsRequest`): `{enabled, webhook_url, wa_number, wa_group, waha_url, waha_session, waha_api_key}`, all `#[serde(default)]` (blank-string-safe). **`waha_api_key` semantics**: a blank/whitespace-only value means "keep the previously configured key" (existing ciphertext carried forward untouched) — but if NO key has ever been configured for this tenant (first setup), a blank key is rejected with `400 "waha_api_key is required on first setup"`. A non-blank value always triggers a full key rotation (fresh envelope-encryption via `WahaSettings::encrypt_new`).
- **SSRF guard**: both `waha_url` and `webhook_url` are validated by `is_safe_outbound_url` before any write; a rejected URL returns `400` with a specific message per field ("waha_url points to a disallowed host" / "webhook_url points to a disallowed host").
- Returns the same `BotSettingsResponse` shape on success (so the form can re-populate from the response, matching Branding's established pattern).

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/settings/+layout.svelte` — **modified**, not created: `NAV_ITEMS` becomes a `$derived` (or equivalent) that includes the new "Bot" entry only when `data.user.is_main_account` is true, instead of a static const array.
- `Frontend/src/routes/(app)/settings/bot/+page.svelte` — page assembly: form fields, masked API-key input, save, forbidden-state handling for a direct-navigation non-main-account visit.
- `Frontend/src/lib/bot-settings.ts` — pure logic, unit-tested (TDD): a basic well-formed-URL check (`isValidUrlFormat(value: string): boolean`, empty string treated as valid since both URL fields can be blank) for `waha_url`/`webhook_url`, and a small helper for the API-key-required-on-first-setup client check (`apiKeyRequired(hasExistingKey: boolean, enteredKey: string): boolean`).
- `Frontend/src/lib/api-bot-settings.ts` — typed REST helpers: `fetchBotSettings(): Promise<BotSettings>`, `saveBotSettings(input: BotSettingsInput): Promise<BotSettings>`. Distinct module name from Fase 7f's existing `api-activity.ts`'s `fetchBotLogs`/`clearBotLogs` (different resource, `/bot/settings` vs `/bot/logs` — no naming collision, but keep the distinction clear in code comments since both live under the same backend `/bot` prefix).

**Data flow:**
- On mount (`onMount`, learning directly from Fase 7g Task 4's SSR-crash finding — this app runs `adapter-node` with SSR on, so a bare top-level `load()` call would crash on direct navigation), `+page.svelte` calls `fetchBotSettings()`.
- **A `403` from `fetchBotSettings()` is a distinct, expected UI state** (not a generic error banner): show a clear "Anda tidak memiliki akses ke halaman ini" message instead of the form. This is the direct-navigation guard for a non-main-account session that reached this URL despite the hidden nav link.
- The API key field: `type="password"`, always empty on load (the backend never sends it), with dynamic placeholder text — `"Biarkan kosong untuk tidak mengubah"` when `waha_api_key_set` is true, `"Wajib diisi (setup pertama)"` when false. Client-side: if `waha_api_key_set` is false and the field is submitted blank, block Save with an inline error (mirrors the backend's own 400 case, giving immediate feedback instead of a round-trip).
- `waha_url`/`webhook_url`: on blur or on Save, run `isValidUrlFormat` — a syntax-only check, explicitly not a security boundary (see Scope). An invalid format shows an inline error; the real host-safety check remains entirely server-side, and a `400` from a SSRF rejection surfaces as a save-time error banner with the backend's own message.
- **Save re-populates from the response**, same established pattern as Branding — this matters doubly here since the response also reveals the POST-save `waha_api_key_set` state (e.g., confirming a rotation actually took effect).
- **RBAC:** unlike Branding, this page has NO read-only view for non-main-account — the whole page is content-gated, either via the hidden nav entry (normal path) or the 403-driven "no access" message (direct-navigation path). No `<fieldset disabled>` needed since a non-main-account session never sees the form at all.

**Accessibility:** consistent with 7a-7g's bar — tokens-only styling, every input labeled, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `aria-live="polite"` for save success/error and the forbidden-state message, a `<svelte:head><title>Bot — TOWER</title></svelte:head>` (an item Fase 7g's own whole-branch review flagged as missing and is now this phase's job to get right from the start).

## Testing

- **Unit (Vitest):** `bot-settings.ts` — `isValidUrlFormat` (valid http/https, invalid schemes, malformed strings, blank-is-valid); `apiKeyRequired` (true only when no existing key AND entered key is blank/whitespace). `api-bot-settings.ts` — wire mapping (snake_case round-trip), `PUT` (not `POST`) verb, and specifically a test that a blank `waha_api_key` in the request body serializes as an actual blank string (not omitted), matching what the backend's "keep existing" branch expects to see.
- **E2E (Playwright), `Frontend/tests/settings-bot.spec.ts`:** unauthenticated visit redirects to `/login`; non-main-account session does not see the "Bot" nav entry at all, AND a direct navigation to `/settings/bot` shows the forbidden message (both content-gating layers, matching `/activity`'s Log Bot tab test pattern); main-account session loads the real existing config and confirms the API key field is blank with the "keep existing" placeholder (since the dev tenant's row already has a key configured); editing a non-key field (e.g. `wa_group`) and saving without touching the key field persists that edit while leaving the key functionally unchanged (verified indirectly: `waha_api_key_set` stays `true` in the response); entering a new value in the API key field and saving rotates it (`waha_api_key_set` still `true`, and the save succeeds — proving the rotation path itself works) — **this test MUST fetch the tenant's original settings first and restore every field to its original value in a cleanup step (afterEach or an explicit final restore call within the test) so the shared dev-tenant row is left exactly as `rules.spec.ts`'s OTP test expects it**, the same self-restoring discipline already used for other shared-state e2e tests in this project; an invalid URL format shows an inline client-side error before any save request fires (verified via `page.route` interception, matching Fase 7g's file-validation test pattern).

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming (nav-hiding vs. read-only-view, the API-key input UX, and the shared-row e2e safety concern). Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path.
