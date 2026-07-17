# Fase 7c: `/tickets` (Full Ticket Management) — Design

**Status:** Approved for implementation.

## Context

Fase 7b shipped `/command` — a live-only dashboard with a compact Ticket Ticker (pending bookings, optimistic manual accept) and the Latency Tape. Both 7a's and 7b's design docs name `/tickets` as the next sub-fase, explicitly scoped as "versi manajemen penuh, filter/histori" (full management version, filter/history) — see `Docs/superpowers/specs/2026-07-17-fase-7b-nav-shell-command-design.md` line 185. This doc covers that sub-fase.

The master spec (`Docs/tower-master-spec.md:183`) names `/tickets` as one of Fase 7's 7 surfaces but gives no content-level requirements beyond "this surface exists" plus the project-wide realtime/accessibility rules that already apply everywhere (WCAG 2.2 AA, optimistic accept, connection-lost handling). All concrete scope below was worked out in this design conversation, not dictated top-down.

## Scope

**In scope:**
- A full data-table view of bookings (not the compact ticker style) covering BOTH live (pending) and historical (accepted/failed) bookings in one place.
- Server-side filtering: status, SPX ID (exact/prefix), created-at date range.
- Server-side pagination (numbered pages, reusing the existing `LIMIT`/`OFFSET` convention — no cursor-pagination rewrite, no virtualization needed since page size is capped).
- Row detail drill-down: click a row → panel showing the full `BookingDetail` plus that booking's `accept_events` audit trail (rule matched, outcome, timing) — data that already exists in the DB but has never been surfaced in any UI.
- Manual accept action on `pending` rows, reusing the same optimistic-UI pattern as `/command`'s Ticket Ticker (but with a new, richer row type — see "Status vocabulary" below).
- Responsive: desktop shows a real table; narrow viewports collapse each row into a stacked card (never horizontal-scroll-only).
- Icons via `@lucide/svelte` (established in the icon-adoption commit) for filter/pagination/action affordances.

**Out of scope (deliberately deferred, not silently dropped):**
- **Free-text search over `route`** (the SPX route-stop names). `route` is not a real DB column — it's computed from `raw_data` JSONB at read time, has no index, and no dynamic-filter precedent exists anywhere in the `store` crate yet. Searching it today means an unindexed sequential scan on every request. Per the master spec's own "Parity dulu, optimasi kedua" principle, this ships deferred; SPX ID search (which IS indexed) covers the common "find this one booking" case. Follow-up path if this becomes a real need: either a generated/stored `route_search` column with a GIN trigram index, or promoting `route_stops` into its own indexed column.
- **`new_tickets` WS event** — still not wired (a Fase-5 gap tracked since 7b). `/tickets`'s live-status rows use the same 20s-poll-as-fallback pattern `/command` already established, not a new mechanism.
- Bulk actions (multi-select accept/export) — not requested, no evidence of need yet.
- Editing a booking's own fields — bookings are read/accept-only from this UI, matching the existing `/bookings` REST surface's own scope (there is no `PUT /bookings/:id`).

## Backend Changes

### 1. Filter query params on `GET /bookings/live` and `GET /bookings/history`

New optional `ListParams` fields (`Backend/crates/api-gateway/src/routes/bookings.rs`):
- `status: Option<String>` — when present, overrides each endpoint's current hardcoded status set (`live` defaults to `pending`, `history` defaults to `accepted,failed`), still validated against the real 3-value vocabulary (`pending`/`accepted`/`failed`) server-side — reject anything else with `400 Bad Request`, never pass an arbitrary client string into SQL.
- `spx_id: Option<String>` — exact or prefix match (`spx_id LIKE $1 || '%'`), uses the existing `(tenant_id, spx_id)` unique index.
- `from: Option<DateTime<Utc>>`, `to: Option<DateTime<Utc>>` — inclusive range on `created_at`, uses the existing BRIN index.

### 2. Dynamic query building — first precedent in the `store` crate

`store::bookings::list_live`/`list_history` currently build fully static SQL strings (confirmed: no `store::*` module has an optional-filter pattern today). This sub-fase introduces `sqlx::QueryBuilder` for these two functions specifically — parameterized throughout (`QueryBuilder::push_bind`, never string concatenation of user input), matching this project's non-negotiable SQL-injection-safety standard. This pattern is scoped to `bookings.rs`; it is not a project-wide refactor of every `list_*` function.

### 3. `BookingDetail` gains `route: Vec<String>`

Currently `BookingListItem` has `route` (added Fase 7b Task 1) but `BookingDetail` (the `/bookings/:id/detail` response) does not — an oversight found during this design's research. Add it the same way: `spx_client::normalize_booking(&raw_data).route_stops`.

### 4. New endpoint: `GET /bookings/{id}/audit-trail`

Returns `Vec<AcceptEventItem>` filtered by `booking_id = $1` (reuses the existing `AcceptEventItem` shape and `accept_events` table — no new schema). Tenant-scoped like every other `/bookings/*` route, `session_auth`-gated (same as the rest of this router, not the stricter `ManageBotSettings` permission `bot_log` uses — this is per-booking data any logged-in tenant member should see, matching `/bookings/spx-log`'s existing gate).

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/tickets/+page.svelte` — page assembly: filter bar + table/cards + pagination + detail drawer, wired to REST fetch (not primarily WS-driven, since this is a browse/search surface, not a live feed — WS is used only to keep `pending` rows' status fresh via the same `ticket_accepted`/`ticket_rejected` events `/command` already subscribes to, through Svelte context).
- `Frontend/src/lib/tickets.ts` — pure logic: the richer `TicketRow` type for this view (see below), and any pure filter-state → query-string mapping. Unit-tested, following the same "$lib teruji" convention as `ticker.ts`.
- `Frontend/src/lib/api-tickets.ts` — typed REST helpers: `fetchTickets(filters, page)`, `fetchBookingDetail(id)`, `fetchAuditTrail(id)`.
- `Frontend/src/lib/components/TicketsTable.svelte` — the table (desktop) / card-list (mobile) renderer. Responsive via a CSS breakpoint, not two separate components — one component, `hidden md:table`/`md:hidden` style toggling, so there is exactly one source of truth for row data and one for each row's markup variant.
- `Frontend/src/lib/components/TicketFilterBar.svelte` — status/SPX-ID/date-range controls.
- `Frontend/src/lib/components/Pagination.svelte` — Prev/Next + page indicator, generic enough to be reused by future sub-fases (Rules/Price/Activity all likely need paginated lists too) — this is the one deliberate, justified abstraction in this sub-fase, since the need is already visible across ≥2 upcoming surfaces, not speculative.
- `Frontend/src/lib/components/TicketDetailDrawer.svelte` — slide-in panel showing `BookingDetail` + audit trail.

### Status vocabulary — NOT a reuse of `ticker.ts`'s `TicketRow`

`/command`'s `ticker.ts` intentionally simplifies status to `'pending' | 'accepted' | 'taken_by_agency'` — correct for its scope (a live-only view where only those 3 outcomes are ever visible). `/tickets` needs the REAL backend vocabulary plus sub-detail:

```ts
// Frontend/src/lib/tickets.ts
export type TicketStatus = 'pending' | 'accepted' | 'failed';
export type FailureReason = 'expired' | 'taken_by_other' | 'manual_accept_failed' | null;
```

`FailureReason` is read from `raw_data.drift_reason`/`raw_data.accept_reason` (server-side, folded into the REST response — the frontend never parses `raw_data` itself) and rendered as a secondary badge next to the primary `failed` status dot, so a user can distinguish "expired before we could act" from "another agency took it" from "we tried and the dispatch failed" at a glance — this is the actual value of a "full management" view over the compact ticker.

### Data flow

1. Filter bar state → `fetchTickets({status, spxId, from, to}, page)` → `GET /bookings/live` or `/history` depending on the status filter (a `pending`-only filter hits `/live`; anything including `accepted`/`failed` hits `/history`; "all statuses" — the default — needs both, fetched in parallel and merged client-side by `created_at DESC`, capped at the page size).
2. Row click → `fetchBookingDetail(id)` + `fetchAuditTrail(id)` in parallel → `TicketDetailDrawer`.
3. "Terima" click on a `pending` row → same optimistic pattern as `/command` (mark accepting → `POST /bookings/:id/accept` → reconcile or revert), implemented as its own small set of pure functions in `tickets.ts` (not imported from `ticker.ts`, since the row shape differs) — some duplication with `ticker.ts`'s `markAccepting`/`revertAccepting` is accepted here: the two functions are ~3 lines each, and forcing a shared abstraction across two different row types for that little logic would cost more in indirection than it saves (YAGNI/ponytail call).
4. WS subscription (via `getContext('ws')`, shared connection from Fase 7b's layout) updates any currently-visible `pending` row in place on `ticket_accepted`/`ticket_rejected` — same events `/command` already consumes, no new WS event types needed.

## Testing

- `tickets.ts` — unit tests (vitest) for the pure functions (accept optimistic markers, filter-state→query-string mapping), same rigor as `ticker.test.ts`.
- Backend — new tests for the filter query params (status validation rejects invalid values with 400; `spx_id`/date-range filters return the right subset; the new `audit-trail` endpoint returns tenant-scoped, booking-scoped results only) and for `BookingDetail`'s new `route` field.
- E2E (Playwright) — real stack: load `/tickets`, apply a filter, see filtered results; click a row, see the detail drawer with real audit-trail data; click "Terima" on a pending row, see optimistic-then-confirmed state; resize to a narrow viewport, confirm the table collapses to cards.

## Accessibility & Responsive

- Table semantics: real `<table>`/`<th scope="col">` on desktop (not div-grid pretending to be a table) — screen readers get real table navigation.
- Mobile card collapse: each card carries the same information as its table row, with visible field labels (not relying on column position to convey meaning, since that's lost once collapsed).
- Filter controls: all keyboard-operable, labeled, focus-visible rings per the existing design system.
- Status badges: dot + text (never color-only), matching `HealthPill`'s established pattern.
- Pagination: `aria-current="page"` on the active page indicator, `aria-label` on Prev/Next.
- Icons (`@lucide/svelte`): decorative icons `aria-hidden`, icon-only buttons get a real `aria-label` or visually-hidden text.
