# Fase 7j: `/settings/sub-users` — Design

**Status:** Approved for implementation.

## Context

Fourth of five `/settings/*` sub-phases (see Fase 7g's design doc for the full decomposition decision). Fase 7g/7h/7i built the shell + `/settings/branding`, `/settings/bot`, `/settings/locations`. This phase adds `/settings/sub-users` — a management page over the already-existing, never-consumed `GET/POST/DELETE /auth/portal-users` endpoint (Fase 6b).

**RBAC matches Locations'/Branding's pattern, not Bot's — a correction from an earlier, wrong assumption.** Re-reading `Backend/crates/api-gateway/src/routes/portal_users.rs` directly (not relying on the resource's name/sensitivity "feeling" like it should be content-gated) confirms: `GET /auth/portal-users` is `session_auth`-only — ANY authenticated tenant member may list sub-users, matching this crate's established "a read within one's own tenant" posture (the same as `GET /auth/spx-credentials`). Only `POST`/`DELETE` require `Permission::ManageSubUsers` (main-account only). So this page is edit-gated: the list is always visible with real data, only the create-form and delete buttons are disabled for non-main-account.

**This is the first phase to build a genuinely new-password-creation UI.** Every prior `/settings/*` phase either had no password field (Branding, Locations) or a masked/write-only rotate-existing-secret field (Bot's WAHA API key, blank-means-keep). Creating a sub-user means typing a brand-new password that has never existed before — the show/hide toggle pattern already established on `/login` (`Frontend/src/routes/login/+page.svelte`) is the direct precedent to reuse, not invent fresh.

**A real backend self-lockout guard exists and must be surfaced in the UI**: `remove()` in `portal_users.rs` explicitly rejects (`400`) a main-account attempting to delete their OWN row (`id == user.portal_user_id`), before even calling the store layer — this prevents a tenant from locking itself out of sub-user management entirely. The frontend has no direct way to compare ids (`/auth/me`'s response, `{username, display_name, is_main_account}`, carries no portal_user id — confirmed by reading `Frontend/src/routes/(app)/+layout.server.ts`'s `SessionUser` type) — so "is this my own row" must be detected by matching `username` against `data.user.username`, which is reliable since usernames are tenant-unique.

**No edit/toggle capability exists in the backend** — this resource is create+delete only, like Locations (no `PUT`, no way to change `display_name`/`password`/`is_main_account`/`enabled` after creation). This phase does not invent one.

**`is_main_account` is settable at creation time** (`CreatePortalUser.is_main_account: bool`, default `false`) — a main-account user creating a new sub-user can choose to grant that new account main-account status too. This is already bounded by the existing RBAC (only a main account can create anything here at all) — the create form exposes this as a checkbox, no extra confirmation step invented beyond what every other destructive/privileged action in this codebase already uses (native `confirm()` is reserved for delete, per established convention; a grant-at-creation checkbox needs no separate confirmation).

## Scope

**In scope:**
- `/settings/sub-users` page: a list of the tenant's sub-users (username, display name, an "Akun Utama" badge when `is_main_account` is true), in the order the backend returns them (`created_at` ascending — no re-sort needed, unlike Locations' alphabetical case).
- A create form: username, password (with show/hide toggle, matching `/login`'s established pattern), display name, and an "is main account" checkbox.
- Delete button per row (native `confirm()` guard, matching established precedent), **disabled with an inline explanatory note for the row matching the current session's own username** (self-lockout, backend-enforced — this is presenting an existing 400 cleanly, not inventing new client-side policy).
- The "Sub-users" entry in the `/settings` shell's nav array, visible to ALL authenticated sessions (edit-gated like Branding/Locations — one more append to the flag-filtered array Fase 7i built).
- Read-only view for non-main-account sessions (disabled create-form and disabled delete buttons), matching Branding's/Locations' pattern.

**Out of scope (deliberately deferred, not silently dropped):**
- **No edit/rename/password-reset/enable-disable capability** — the backend has none (create+delete only, by design). Not something this phase invents.
- **No password-strength meter beyond the backend's own >= 8 character minimum.** Mirrored client-side for immediate feedback (same pattern as every other phase's validation mirroring); no additional complexity-scoring UI invented.
- **`enabled` is not displayed.** `PortalUserSummary` carries it, but no path in this codebase can ever set it to anything but its DB default (`true`) — no `CreatePortalUser` field, no update endpoint. Surfacing a value that can never vary would be UI for a feature that doesn't exist yet; the wire type still carries it (for forward-compat / honesty about the real API shape) but the page doesn't render it.
- **No safeguard beyond the backend's own self-lockout against a tenant ending up with zero main accounts.** A main account COULD still delete every OTHER main account (just not themselves) — this is an existing, already-shipped backend design choice from Fase 6b, not something this pure-frontend phase re-litigates or patches around.

## Backend

No changes. Existing surface (`Backend/crates/api-gateway/src/routes/portal_users.rs`):

- **`GET /auth/portal-users`** — `session_auth` only (any authenticated tenant member). Returns `PortalUserSummary[]`: `{id: uuid, username: string, display_name: string, is_main_account: bool, enabled: bool}`, ordered by `created_at` ascending. `password_hash` is never included in any response.
- **`POST /auth/portal-users`** — `Permission::ManageSubUsers` (main-account only). Body: `{username: string, password: string, display_name: string, is_main_account?: bool}` (`is_main_account` defaults `false` if omitted). `400` if `username` is blank or `password` is under 8 characters. A duplicate `(tenant_id, username)` returns `409 {"error": "already exists"}` (generic `sqlx::Error`→`ApiError` conversion, same pattern as Locations/`/price`). Returns the created `PortalUserSummary` (with its real server-assigned `id`, never the password).
- **`DELETE /auth/portal-users/:id`** — `Permission::ManageSubUsers` (main-account only). **Self-lockout guard**: `400 "cannot delete your own account"` if `id` matches the CALLER's own portal_user id (checked before the store call). Returns `204` on success, `404` if the id doesn't exist for this tenant.

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/settings/+layout.svelte` — **modified**: append `{ href: '/settings/sub-users', label: 'Sub-user' }` to `ALL_NAV_ITEMS` (no `mainAccountOnly` flag — always visible, per the corrected RBAC understanding above).
- `Frontend/src/routes/(app)/settings/sub-users/+page.svelte` — page assembly: create form (with password show/hide), sub-user list, delete-with-confirm (self-lockout-aware), read-only gating.
- `Frontend/src/lib/sub-users.ts` — pure logic, unit-tested (TDD): `validatePassword(password: string): string | null` (mirrors the backend's own >= 8 char rule, returns an error message or `null`), and `isSelf(username: string, sessionUsername: string): boolean` (the self-lockout row-detection helper — small enough to be worth extracting and unit-testing on its own, since getting it wrong either hides the wrong row or fails to protect the right one).
- `Frontend/src/lib/api-sub-users.ts` — typed REST helpers: `fetchSubUsers(): Promise<PortalUser[]>`, `createSubUser(input: CreateSubUserInput): Promise<PortalUser>`, `deleteSubUser(id: string): Promise<void>`. New module (not a reuse of anything — no prior phase has consumed this endpoint at all, unlike Locations which had a partial existing consumer in `api-rules.ts`).

**Data flow:**
- On mount (`onMount`, per the established SSR-safety convention), fetch the full sub-user list.
- Create form: username, password (type toggles between `password`/`text` via a show/hide button, mirroring `/login`'s exact markup/aria pattern — `aria-pressed`, `sr-only` label text), display name, and an "is main account" checkbox. Client-side: `validatePassword` blocks submission with an inline error before any network call for a too-short password (mirrors the backend's 8-char minimum exactly). A `409` on submit shows a client-hardcoded "Username ini sudah dipakai." message (same status-code-keyed convention as Locations' `/price`-derived pattern — never parses the response body).
- On successful create, the new user is appended to the local list (matching backend's `created_at`-ascending order — a fresh append is already correctly ordered, no re-sort needed).
- Delete: `confirm()` guard, then `deleteSubUser(id)`; on success, remove from local list. **Self-row detection**: `isSelf(row.username, data.user.username)` determines whether that row's delete button is `disabled`, with a small inline note ("Tidak bisa menghapus akun sendiri.") shown next to the disabled button for that row only — chosen over hiding the button entirely so the reason is legible, not just an apparently-missing feature (explicit design decision, confirmed with the user during brainstorming).
- **RBAC:** `data.user.is_main_account` disables the create-form fields and every delete button (except self-row, which is disabled regardless via the self-lockout check) for non-main-account sessions. The list itself always shows real data — matching Branding's/Locations' edit-gated pattern, including the same read-only explanatory banner ("Hanya akun utama yang dapat mengelola sub-user.") both those phases ended up needing (built in from the start this time, per the tracked lesson from Fase 7i's whole-branch review).

**The nav-array append** (mechanical, using the flag-filtered structure Fase 7i already built):
```ts
const ALL_NAV_ITEMS: NavItem[] = [
  { href: '/settings/branding', label: 'Branding' },
  { href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
  { href: '/settings/locations', label: 'Lokasi' },
  { href: '/settings/sub-users', label: 'Sub-user' }
];
```

**Accessibility:** consistent with 7a-7i's bar — tokens-only styling, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error/read-only banners, `<svelte:head><title>Sub-user — TOWER</title></svelte:head>`, native `confirm()` for delete, the password show/hide toggle button gets the same `aria-pressed`/`sr-only` treatment `/login` already established (not a fresh a11y pattern to invent).

## Testing

- **Unit (Vitest):** `sub-users.ts` — `validatePassword` (accepts exactly 8 chars, rejects 7, accepts longer, empty string rejected); `isSelf` (matches on exact username equality, case-sensitivity matches whatever the backend's own username comparison does — usernames are stored/compared as-is, no normalization found in `portal_users.rs`/migration 0002, so `isSelf` does a plain `===`). `api-sub-users.ts` — wire mapping, `POST`/`DELETE` verb+URL correctness, 409/404 status propagation without body-parsing.
- **E2E (Playwright), `Frontend/tests/settings-sub-users.spec.ts`:** unauthenticated visit redirects to `/login`; main-account session sees the "Sub-user" nav entry and the real existing list (including the seeded `e2e-test-user`/`e2e-readonly-user` rows already used across this project's e2e suites); creating a new sub-user with a valid password persists it (visible after reload); a too-short password shows an inline error and fires no create request (verified via `page.route` interception, matching Fase 7g's/7i's established file/duplicate-validation test pattern); a duplicate username shows the "sudah dipakai" message; deleting a just-created sub-user (confirm accepted) removes it; **the row matching the logged-in `e2e-test-user` itself has a disabled delete button with the self-lockout note visible**; non-main-account (`e2e-readonly-user`) session sees the real list with disabled create-form and disabled delete buttons (matching Locations' non-main-account test pattern). **This suite must not delete or modify the actual `e2e-test-user`/`e2e-readonly-user` rows other suites depend on for login** — every sub-user this suite creates gets a `Date.now()`-suffixed unique username and is deleted by the end of its own test, same self-cleaning discipline as Fase 7i's Locations suite (this resource has no cross-suite shared-fixture risk beyond "don't delete the seeded login accounts," which the self-lockout guard and this suite's own restraint both help ensure).

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming (RBAC shape corrected via direct source read, the self-lockout UI treatment — disabled-with-note, confirmed with the user — and the password-creation UX precedent). Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path.
