# Fase 7d: `/rules` (Rule Builder) — Design

**Status:** Approved for implementation.

## Context

Fase 7a-7c shipped login, the nav shell + `/command` live dashboard, and `/tickets` full management. The master spec (`Docs/tower-master-spec.md:183`) names `/rules` as one of Fase 7's 7 surfaces, with exactly one content-level hint: *"Rule Builder (chip + route lane enum-bound)"*. Everything else below was worked out in this design conversation.

Unlike 7b/7c, **the backend for this sub-fase is already fully built** — Fase 6c shipped `GET/PUT /bookings/settings` (the rule editor + `auto_accept_enabled` kill switch, OTP-gated on its false→true transition) and Fase 6d shipped `GET/POST/DELETE /locations`. The `core-domain` rule engine (`AcceptRule`, `RuleConditions`, `sanitize_accept_rules`, `dedupe_rules`, matching/ranking) is complete and tested. **This sub-fase is a pure frontend build against existing, working backend surfaces — no backend changes.**

## Scope

**In scope:**
- A single page, `/rules`, showing the auto-accept kill switch and the full list of `AcceptRule`s for the tenant.
- Full CRUD editing of rules in all 3 engine modes (`booking_id`, `route`, `filter`) — the engine already supports all 3, so a partial builder would just mean users can't manage rule types they may already have saved.
- Local-edit + single-Save workflow: all add/edit/delete/reorder happens against local state; one "Simpan Perubahan" PUTs the whole set (matches how the backend already works — it replaces the entire rule set on every save, there is no per-rule persistence).
- The `auto_accept_enabled` kill switch, including the OTP arm flow (`POST /auth/request-aa-otp` → `POST /auth/verify-aa-otp` → PUT within the 120s proof window).
- Inline "add new location" in the origin/destination picker (reuses `POST /locations`), since there's no `/settings` page yet to manage locations and a fresh tenant could otherwise have zero usable route-mode rules.
- Read-only mode for non-main-account users (view is ungated; edit requires `ManageRules`).

**Out of scope (deliberately deferred, not silently dropped):**
- **Rule-testing/preview tool** ("try this booking ID against current rules"). Not requested, no evidence of need yet, and it would require either a live booking search or a synthetic-booking input form — meaningfully more surface than this sub-fase's core CRUD. Follow-up candidate once the builder itself is in daily use.
- **Rule templates / duplication shortcut, bulk enable-disable.** Not requested.
- **Rule-change audit trail** (who edited which rule, when). The existing `accept_events` table is for booking-accept events, not rule edits — there is no rule-history schema. Would need its own design pass if wanted.
- **Drag-to-reorder rules.** The real ranking algorithm is mode-first, then `priority`, then specificity (destination count, strict-vs-flexible, service-type count) — list position has no effect on matching. A drag-reorder UI would misrepresent how rules actually rank; `priority` is a plain numeric field instead (see below).
- **Full `/settings`-style location management** (rename, bulk delete). This sub-fase only adds the minimum "create a location inline while building a route rule" affordance; a dedicated locations manager is Fase 7e's job.

## Backend

No changes. Existing surfaces this sub-fase depends on:
- `GET/PUT /bookings/settings` (`Backend/crates/api-gateway/src/routes/rules.rs`) — `SettingsResponse { auto_accept_enabled, rules: Vec<RuleOutput>, warnings: Vec<String> }`, all snake_case wire format (no `rename_all` anywhere in this crate, consistent with the rest of the REST layer). `GET` is `session_auth`-only (any tenant member). `PUT` requires `Permission::ManageRules`; additionally `Permission::ArmAutoAccept` + a valid, single-use `pwverify` Redis proof when `auto_accept_enabled` flips `false→true`. Both permissions are main-account-only today (`auth/permission.rs`).
  - **Important client contract:** client-supplied rule `id`s (the `Uuid` each `RuleOutput` carries) do **not** round-trip — `PUT` always deletes-and-reinserts, server-assigning fresh ids via `sanitize_accept_rules`'s own `rule_N` scheme internally before persistence. The client must key its local list on an ephemeral client-generated id, never on the server `id`, and must treat the PUT response's `rules` array as the new source of truth (replacing local state wholesale) rather than merging by id.
- `POST /auth/request-aa-otp` (no body, 200/429/400), `POST /auth/verify-aa-otp` (`{code}`, 200/401/429) — `Backend/crates/api-gateway/src/routes/otp.rs`. Code TTL 180s, resend cooldown 60s, max 5 attempts/window. Delivered via WhatsApp to the tenant's configured number; if unconfigured, `request-aa-otp` 400s with `"OTP delivery is not configured for this tenant"` — the frontend must surface this distinctly from a transient failure (it means arming is impossible until WA is set up elsewhere, not "try again").
- Successful `verify-aa-otp` writes a 120s-TTL single-use proof; `PUT /bookings/settings` consumes (and deletes) it implicitly via the session's `tenant_id`+`portal_user_id` — no token is echoed by the client. If the 120s window lapses before Save, the PUT 401s.
- `GET/POST/DELETE /locations` (`Backend/crates/api-gateway/src/routes/locations.rs`) — `LocationItem { id: Uuid, name: String }`. `GET` is `session_auth`-only, `POST`/`DELETE` require `ManageLocations` (main-account-only). No `PUT` — add/delete only. `list_route_locations` returns the full unpaginated list, name-sorted; fine at expected scale (tens, not thousands, of named locations).

**Known field vocabularies** (needed for chip/select labels, none of this is guessable from schema alone — captured here so the implementer doesn't have to re-derive it):
- `service_types` (route/filter modes): 8 canonical vehicle classes — `TRONTON`, `FUSO`, `CDD LONG`, `CDE LONG`, `BLINDVAN`, `WINGBOX`, `ENGKEL`, `40FCL` (`core-domain/src/vehicle.rs`'s `vehicle_rule_label`). Free text is canonicalized server-side on save regardless, but the UI should offer exactly these 8 as chip options rather than free text, per the master spec's "chip"-bound hint.
- `shift_types`: `1` = Pagi, `2` = Siang, `3` = Malam (confirmed by the user; not in code/schema).
- `trip_types`: `1` = Berangkat, `2` = Pulang (confirmed by the user; not in code/schema).
- `booking_type`: `all` / `spxid` / `reguler`.
- `match_mode` (route mode only): `strict` (every destination required, in order) / `flexible` (only the last destination required).

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/rules/+page.svelte` — page assembly: kill-switch card, rule list, add/save controls, OTP modal wiring. Session-gated by the existing `(app)/+layout.server.ts`, same as every other Fase-7 page.
- `Frontend/src/lib/rules.ts` — pure logic: the client-side rule/condition types, mode-conditional field defaults, condition-summary text generation (for the collapsed row), and a dirty-state diff check (local vs. last-loaded/saved). Unit-tested, following the `ticker.ts`/`tickets.ts` "$lib teruji" convention.
- `Frontend/src/lib/api-rules.ts` — typed REST helpers: `fetchSettings()`, `saveSettings(payload)`, `fetchLocations()`, `createLocation(name)`, `requestAaOtp()`, `verifyAaOtp(code)`.
- `Frontend/src/lib/components/RuleRow.svelte` — one rule: collapsed summary + expand-in-place editing, mode selector, all condition fields, delete.
- `Frontend/src/lib/components/ChipInput.svelte` — generic reusable chip list: free-text entry (booking IDs — type + Enter to add) when given no `options` prop, or a closed-vocabulary multi-select (service types, shift/trip types) when given one — both render/remove chips identically, differing only in whether arbitrary text is accepted, so one small prop covers both rather than two near-duplicate components.
- `Frontend/src/lib/components/LocationCombobox.svelte` — separate from `ChipInput`: async-searches `GET /locations`, supports single-select (origin) and multi-select-capped-at-5-with-reorder (destinations), and the inline "add new location" action. Kept separate because its interaction (remote search, create-new, ordering) is materially different from a plain chip list, not a variant of it.
- `Frontend/src/lib/components/AutoAcceptSwitch.svelte` — the kill-switch card + OTP modal (request → code entry with live countdown → verify), reporting the armed/disarmed intent up to the page for inclusion in the next Save.

**Data flow:** page mounts → `fetchSettings()` + `fetchLocations()` in parallel → local `$state` rule list (each row keyed by a client-generated id) + local `autoAcceptEnabled` boolean, both seeded from the response. All edits mutate local state only. "Simpan Perubahan" calls `saveSettings({auto_accept_enabled, rules})`; on success, **replace** local state with the response's `rules`/`auto_accept_enabled` (not a merge — this is how the user sees server-side dedupe/collapse and warning-driven adjustments reflected), and surface `warnings[]` as a dismissible `role="alert" aria-live="polite"` list, matching the established banner pattern from 7a-7c. A dirty-state guard (`beforeNavigate` + `beforeunload`) warns before leaving with unsaved local edits.

**Rule row UX:** collapsed row shows name, mode badge, enabled toggle, priority number, and an auto-generated one-line condition summary; click/Enter/Space expands in place (no dialog/drawer — see rationale below). Expanded view shows a mode selector (segmented control) that swaps the visible field set:
- `booking_id`: `ChipInput` (free-text mode) for `booking_ids`.
- `route`: `LocationCombobox` for origin (single) and destinations (multi, capped 5, ordered — reorder via explicit up/down buttons, not drag, for keyboard accessibility), `match_mode` radio with a one-line Indonesian explainer.
- `filter`: no origin/destinations.
- Shared (all modes): `service_types` (`ChipInput`, closed-vocabulary mode, the 8 options above), `max_weight`, `max_cod_amount`, `booking_type` (radio), `shift_types`/`trip_types` (`ChipInput`, closed-vocabulary mode, labeled per the vocabularies above), `coc_only`/`non_coc_only` (mutually exclusive in the UI itself — no point letting a user pick a combination the server will silently override), `min_deadline_min`, `max_accept_count` (explicit "0 = tanpa batas" label), `accepted_count` shown read-only (server-maintained counter).

**Layout choice — inline expanding rows, not cards + drawer:** considered a compact-card-list + slide-in edit drawer (like `TicketDetailDrawer`), but rejected: a drawer with ~15 editable fields needs a real multi-element focus trap (heavier than 7c's single-close-button drawer), and adds a second UI paradigm (list + modal) for state that's still just local edits with no independent "open"/"closed" server concept. Inline expand keeps one page, one scroll, and the simplest keyboard flow.

**Priority is a plain numeric field**, not derived from list position — see Scope's out-of-scope note on drag-reorder.

**Permissions:** the page loads and displays fully for any authenticated user (`GET` is ungated). `is_main_account` (already returned by `/auth/me` and already consumed by `TopNav`/`+layout.server.ts`) gates all edit affordances client-side: non-main-account sees every field read-only, no add/delete/toggle/Save controls, and a banner: "Hanya akun utama yang dapat mengubah rule." The server remains the real enforcement (`ManageRules`/`ArmAutoAccept` on `PUT`) — this is a UX courtesy, not the security boundary.

**Accessibility:** consistent with 7a-7c's bar — tokens-only styling (`app.css` `@theme`), 44px tap targets, focus-visible rings, `aria-live="polite"` for warnings and the OTP countdown, all chip/combobox interactions keyboard-operable (type + Enter to add, explicit remove buttons, explicit reorder buttons — no drag-only affordance anywhere on this page), expand/collapse rows operable via Enter/Space, glyph+text (not color-only) for the kill-switch status.

## OTP Arm Flow (detail)

1. User toggles the kill switch ON (a UI-only intent at this point — nothing is armed yet).
2. `AutoAcceptSwitch` opens a modal: "Kirim kode" → `requestAaOtp()`. A 400 with the "not configured" message is shown as a blocking explanation (arming is impossible until WA is configured elsewhere), distinct from a retryable error. A 429 is shown as "kode sudah dikirim, tunggu sebentar" with the remaining cooldown.
3. On successful request, show a 6-digit code input and a live countdown from 180s (matching the WA message's own "berlaku 3 menit"). A "Kirim ulang" option respects the 60s cooldown client-side (not just relying on the server's own 429).
4. `verifyAaOtp(code)` — 401 shows "Kode salah atau kedaluwarsa, coba lagi" (server deliberately doesn't distinguish wrong-code from expired/absent, so neither does the UI); 429 shows "Terlalu banyak percobaan, minta kode baru".
5. On success, close the modal, the kill switch shows ON locally, and a countdown/notice indicates the 120s window in which Save must happen to actually take effect.
6. If the user clicks "Simpan Perubahan" within the window, the PUT proceeds normally. If the window has lapsed, the PUT 401s — the frontend catches this specific case (auto_accept_enabled was being set true) and reopens the OTP modal with "Kode kedaluwarsa, verifikasi ulang" rather than a generic save-failure message.

## Testing

- **Unit (Vitest):** `rules.ts` — condition-summary generation per mode, dirty-diff logic, any client-side mirror of `coc_only`/`non_coc_only` mutual exclusivity.
- **E2E (Playwright), `Frontend/tests/rules.spec.ts`:** unauthenticated redirect; load and display existing seeded rules; add a route-mode rule end-to-end (including inline "add new location") and save, confirming the saved state renders after reload; edit and delete a rule; a non-main-account session sees the read-only banner and no edit controls (the e2e seed data does not currently include a non-main-account test user under tenant `tower-dev` — the implementer must add one, mirroring the existing `e2e-test-user` seed pattern from `login.spec.ts`); the OTP arm flow end-to-end (needs a way to read the delivered code in test — check whether `notifier::waha` has a test/dev-mode bypass or log-visible code path before assuming a mock is required at the HTTP layer).

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming. Any genuinely new ambiguity found during implementation (e.g. the WAHA test-code-visibility question above) should be raised through the normal task-brief escalation path, not silently decided.
