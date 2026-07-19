# Fase 7f: `/activity` (Activity Log) — Design

**Status:** Approved for implementation.

## Context

The master spec (`Docs/tower-master-spec.md:183`) names `/activity` as one of Fase 7's 7 surfaces, with no content-level detail beyond that. Unlike every other Fase 7 page, `/activity` has **no reference-app equivalent** — the reference app's own `activity_log` table (16 event types) was deliberately NOT ported during Fase 2 (YAGNI: its accept/reject-decision scope already overlaps `accept_events`, and broader activity like logins/settings-changes was explicitly deferred to "whichever fase actually needs it"). Fase 7d/7e's design docs both flagged `/activity` as a known, named, still-open gap. This doc closes it, scoped to what's actually buildable today.

**The backend already has two real, queryable data sources for this page** — `accept_events` (via the existing but never-consumed `GET /bookings/spx-log`) and `bot_log` (via `GET/DELETE /bot/logs`) — chosen over introducing new backend event tracking (logins, settings changes, etc.), which would be a materially larger, riskier phase better left for if/when actually needed.

## Scope

**In scope:**
- A single page, `/activity`, with two tabs: "Riwayat Keputusan" (accept_events) and "Log Bot" (bot_log).
- Riwayat Keputusan: genuine server-side pagination over the tenant's full accept/reject decision history, with an expand-in-place disclosure per row showing the raw `detail` JSON.
- Log Bot: the tenant's last-200 bot notification log (OTP sends, dispatch outcomes), with a clear-all action. Visible only to main-account users — this tab is content-gated, not merely edit-gated, since the underlying `GET /bot/logs` itself requires `Permission::ManageBotSettings`.

**Out of scope (deliberately deferred, not silently dropped):**
- **A filter bar on Riwayat Keputusan.** `GET /bookings/spx-log`'s handler ignores the `status`/`spx_id`/`from`/`to` fields on its shared `ListParams` struct entirely — it only consumes `limit`/`offset`. Building a filter UI against a backend that silently ignores the filter would be actively misleading. A follow-up could add real filter support to this endpoint (mirroring Fase 7c's `sqlx::QueryBuilder` pattern for `/bookings/live`/`/history`) if this becomes a real need.
- **Rule-name / booking-summary enrichment.** `AcceptEventItem` returns raw `rule_id`/`booking_id` UUIDs with no join. Resolving these to human names would require additional fetches (a full rules list, a booking lookup) beyond this phase's scope — raw IDs are shown as-is, monospaced.
- **New backend activity tracking** (logins, settings changes, etc.) — see Context; a real scope fork consciously declined this round.
- **`archive_runs` surfacing.** Not tenant-scoped, no RLS, no REST endpoint, no producer yet (Fase 8's retention job doesn't exist) — not real tenant activity today.

## Backend

No changes. Existing surfaces this sub-phase depends on:
- `GET /bookings/spx-log` (`Backend/crates/api-gateway/src/routes/bookings.rs::spx_log`) — `session_auth`-only (any authenticated tenant member, same gate as `/bookings/live`/`/history`/audit-trail). Query params: `limit` (default 50, clamped server-side to a sane range — same `clamp_limit`/`clamp_offset` helpers `/bookings/live` uses), `offset`. Returns `AcceptEventItem[]`: `{id: uuid, booking_id: uuid|null, rule_id: uuid|null, outcome: string, local_dispatch_us: number|null, accept_e2e_ms: number|null, detail: object, created_at: ISO8601}`. `outcome` is one of exactly 6 values (DB CHECK constraint, migration 0008): `accepted`, `rejected`, `skipped`, `taken_by_agency`, `failed`, `agency_dup_unverified`.
- `GET /bot/logs` / `DELETE /bot/logs` (`Backend/crates/api-gateway/src/routes/bot.rs::get_logs`/`delete_logs`) — both require `Permission::ManageBotSettings` (main-account only, confirmed against `permission.rs`'s uniform `is_main_account` gate). `GET` takes no query params, always returns up to 200 entries (`notifier::bot_log::list(..., 200)`), newest-first. Entry shape (`notifier::bot_log::BotLogEntry`): `{ts: number (unix ms), log_type: "success"|"error", kind: "accept"|"agency_loss"|"otp"|null, booking_id: string|null, latency_ms: number|null, rule: string|null, error: string|null}`. `DELETE` clears the whole log, no partial-delete capability.

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/activity/+page.svelte` — page assembly: tab switcher, per-tab data fetching/pagination, permission gating for the Log Bot tab's very existence.
- `Frontend/src/lib/activity.ts` — pure logic: outcome/log-type/kind label mappings (Indonesian), timestamp formatting, latency formatting. Unit-tested.
- `Frontend/src/lib/api-activity.ts` — typed REST helpers: `fetchAcceptEvents(limit, offset)`, `fetchBotLogs()`, `clearBotLogs()`.
- `Frontend/src/lib/components/AcceptEventRow.svelte` — one accept_events row, collapsed summary + expand-in-place JSON detail disclosure (read-only, no edit — simpler than `RuleRow`/`PriceRow`, just a show/hide toggle).
- `Frontend/src/lib/components/BotLogRow.svelte` — one bot_log entry, single-line row (no expand needed — the entry itself is already flat and small).
- Reuses as-is, no changes: `Frontend/src/lib/components/Pagination.svelte`.

**Data flow — two genuinely different pagination models, matching what each backend endpoint actually supports:**
- Riwayat Keputusan: `page` state drives a real `fetchAcceptEvents(...)` call on every page change (server-side pagination — this list can grow unboundedly, unlike `/price`'s bounded config list, so client-side "fetch everything once" is wrong here). `hasMore` uses the exact overfetch-by-one technique already established in `Frontend/src/lib/api-tickets.ts::fetchTickets` (request `limit: PAGE_SIZE + 1`, check `items.length > PAGE_SIZE` for `hasMore`, then slice back to `PAGE_SIZE` before returning) — NOT a naive "did this page come back full" check, which would be wrong exactly when the total count is a multiple of `PAGE_SIZE` (it would falsely report `hasMore: true` and the last page would appear to have a phantom next page).
- Log Bot: one `fetchBotLogs()` call on mount (the endpoint has no pagination — it always returns everything, up to 200), then client-side pagination over that fixed array via `Pagination.svelte`, same pattern as `/price`.

**Tab gating:** `data.user.is_main_account` (same pattern as `/rules`/`/price`) controls whether the "Log Bot" tab button even renders — not just whether its contents are editable. If a non-main-account user were to reach the tab's content some other way, the underlying `fetchBotLogs()` call would itself 403 (server-enforced), so this is UX-only, matching the security posture established every prior phase: the frontend gate is a courtesy, the backend gate is the real boundary.

**Rendering:** `detail: object` (accept_events) is rendered as pretty-printed JSON text (`JSON.stringify(detail, null, 2)` inside a `<pre>`), never via `{@html}` — this is backend-originated data that could theoretically contain arbitrary strings (e.g. upstream SPX error text), so it gets the same auto-escaping treatment as any other rendered value, no special-casing. `outcome`/`log_type`/`kind` render as small labeled badges (glyph/color + text, never color-only, matching the established accessibility bar) via `activity.ts`'s label-mapping functions. Timestamps: `created_at` (accept_events) is ISO8601; `ts` (bot_log) is Unix-ms — `activity.ts` normalizes both to the same display format.

**Permissions:** page itself requires only being authenticated (session-gated by the existing `(app)/+layout.server.ts`). No mutation affordances exist on the Riwayat Keputusan tab at all (it's read-only by nature — you can't edit or delete an accept-event record). The Log Bot tab's one mutation (clear) is main-account-only, matching its view-gate (not a separate, finer-grained permission — `ManageBotSettings` covers both).

**Accessibility:** consistent with 7a-7e's bar — tokens-only styling, 44px primary tap targets, focus-visible rings, keyboard-operable tab switching and row expand/collapse, `role="alert" aria-live="polite"` error banners, native `confirm()` for the destructive clear-log action (matching `/price`'s delete precedent).

## Testing

- **Unit (Vitest):** `activity.ts` — outcome/log-type/kind label mapping, timestamp formatting, latency formatting.
- **E2E (Playwright), `Frontend/tests/activity.spec.ts`:** unauthenticated redirect; Riwayat Keputusan tab loads and shows seeded accept_events (this task must seed at least one `accept_events` row directly via `psql`, verified against the real migration schema — no UI path creates these); pagination advances to a genuinely different server-fetched page; non-main-account session does not see the "Log Bot" tab button at all; main-account session sees the Log Bot tab, and can clear it (with the native confirm dialog handled, matching the established Playwright `page.once('dialog', ...)` precedent from `/price`'s e2e suite).

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming. Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path.
