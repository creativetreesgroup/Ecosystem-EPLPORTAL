# Command/Tickets UI Revamp — Design

**Status:** Approved for implementation.

## Context

Fase 7b (`/command`) and Fase 7c (`/tickets`) are already merged and reviewed. This is a **revision pass** on that shipped UI, triggered by three reference screenshots the user provided (a travel-agency dashboard template for `/command`'s stat-card layout, and the real production SPX portal's ticket table + advanced filter drawer for `/tickets`). This is not a new Fase-7 sub-phase per the master spec's list — it's a visual/data-depth upgrade of two already-built pages, done as one combined package per the user's explicit choice.

Investigation before this doc was written established the key constraint that shapes everything below: `spx_client::normalize_booking` (Fase 3) already parses far more from a booking's raw SPX payload than the API currently exposes — `request_id`, `onsite_id`, `vehicle_type`, `vehicle_capacity`, `deadline_at`, `booking_type` (COC/REG), `trip_type`. None of this is a stored column today; it's re-derived from `bookings.raw_data` (JSONB) at read time in Rust. That's fine for display, but the reference filter drawer (Image #3) needs these as real SQL `WHERE`/`ORDER BY` targets with correct pagination — which is not possible against Rust-side-only derivation. So this revision necessarily includes a migration, not just Svelte changes (confirmed with the user before proceeding).

## Scope

**In scope:**
1. New migration adding generated columns to `bookings` for the SPX-derived fields the new table/filter need.
2. `BookingListItem` expansion (more fields from `normalize_booking`) and `ListParams`/`BookingFilter` expansion (new filter dimensions) in `api-gateway`/`store`.
3. New `GET /bookings/summary` endpoint (today's KPI counts + latency baseline).
4. `/command` page: 5-widget KPI row (1 latency + 4 clickable count widgets that drive the list below) replacing the current bare `LatencyTape` + `TicketTicker` pair with a restyled, denser version in the same visual family.
5. `/tickets` page: full `TicketsTable` column rewrite to match the reference (ID/BK-REQ-OID, Booking Number, Route & Vehicle, Jadwal Booking, Deadline Bidding, Tags, Status, Accept By, Action).
6. `/tickets` page: `TicketFilterBar` replaced by a new `FilterDrawer` component matching the reference's full field set.

**Out of scope (deliberately deferred):**
- **`Accept By` real attribution.** `accept_events` has no actor/`portal_user_id` column — manual accepts today record `detail: {"manual": true}` but not *who*. The column renders in the new table (matching the reference layout) but stays `—` for every row until that's added; inventing a fake actor would be worse than an honest blank. Needs its own design pass (touches `portal_sessions`→`accept_events` linkage) if wanted later.
- **Fixing the pre-existing WS gaps** noted in Fase 7b's whole-branch review (manual accept never emits `ticket_accepted`; `ticket_rejected`/`tickets_removed` have no producer). The summary/widget design below is built to be correct via polling + targeted re-fetch specifically so it does **not** depend on those gaps closing. Not touched here.
- **A real vehicle-type catalog or station-master table.** The Armada and Station dropdowns are populated from *observed data* (`DISTINCT` over existing columns / the existing `/locations` list) — no new admin-managed reference table.
- **Per-operator identity on the "Diambil Operator Lain" widget.** It's a count of `auto_accepted = false` accepted bookings, not a breakdown by which operator. (Consistent with the user's own framing: widgets 4/5 are "1 logika, dipisahkan widget saja".)

## Data Model Changes

New migration `Backend/crates/store/migrations/0021_bookings_spx_derived_columns.sql`, following the exact pattern already established by `is_coc`/`needs_enrichment` (migration 0007) — `GENERATED ALWAYS AS (...) STORED` columns computed from `raw_data`, no backfill step needed (Postgres computes them on `ALTER TABLE ADD COLUMN` for existing rows).

Columns to add, each mirroring a specific piece of `spx_client::booking::normalize_booking`'s logic (`Backend/crates/spx-client/src/booking.rs`) — **the implementer must replicate the same key-priority order**, not invent a different one, since the Rust-side `SpxBooking` (used for the manual-accept path and detail view) and the new SQL-side columns (used for list/filter/sort) must agree on the same booking or the UI will show inconsistent values between the table row and its detail drawer:

| Column | Type | Source keys (priority order, first non-null/non-empty wins) | Notes |
|---|---|---|---|
| `spx_request_id` | `text` | `request_id`, `requestId`, `req_id` | Plain `COALESCE(NULLIF(...))` chain, no numeric conversion. |
| `spx_onsite_id` | `text` (nullable) | `onsite_id`, `onsiteId` | Same pattern; `NULL` when absent (matches Rust's `Option<String>`). |
| `spx_tx_id` | `text` | `booking_name`, `spx_tx_id`, `spxTxId`, `tx_id`, `tracking_no`, else falls back to `spx_id` (the booking's own column) | This is the "Booking Number" (`SPXID_VM_...`) shown in Image #2 — distinct from the internal `id`. |
| `spx_vehicle_type` | `text` (nullable) | Display-name group (`vehicle_type_name`, `right_vehicle_type_name`, `sgi_vehicle_name`) preferred; else code group (`truck_type`, `vehicle_type`, `vehicleType`, `service_type`) **with the same numeric-only-code discard rule** `normalize_booking` uses (a value that is entirely ASCII digits is treated as an internal id, not a real vehicle type, and discarded) — see `booking.rs`'s `numeric_only_vehicle_type_is_discarded` test for the exact behavior to match. |
| `spx_deadline_at` | `timestamptz` (nullable) | `bidding_ddl`, `deadline_at`, `pickup_time_ms`, `expired_at`, numeric value passed through the same `to_ms` rule (0 → 0/NULL; >1e12 already ms; else seconds×1000) | Powers Deadline Bidding's countdown and sort-by-deadline. |
| `spx_pickup_time` | `timestamptz` (nullable) | `booking_date`, `schedule_at`, `pickup_time`, `pickup_date` (same `to_ms` rule), falls back to `spx_deadline_at` if none present | Powers "Jadwal Booking". |
| `spx_trip_type` | `int` (nullable) | `trip_type` (numeric) | Powers the ADHOC/FIX tag — see Open Questions, this mapping is a best-effort guess. |
| `spx_origin_station` | `text` (nullable) | First node's `name` in `route_detail_list[0].node_info_list[0]` | **Deliberate simplification** (ponytail: known ceiling): only the `route_detail_list` path is replicated in SQL, not `normalize_booking`'s full multi-fallback chain (`sgi_province_name`/`province_name` string-splitting) — a booking lacking `route_detail_list` shows `NULL` here even if the Rust-side detail view could resolve a province via the fallback chain. Acceptable because `route_detail_list` is SPX's primary/expected shape; upgrade path is porting the fallback chain into SQL if bookings without it turn out to be common. |
| `spx_dest_station` | `text` (nullable) | Last node's `name` in `route_detail_list[-1].node_info_list[-1]` | Same simplification as above. |

Indexes: `btree (tenant_id, spx_deadline_at)`, `btree (tenant_id, spx_vehicle_type)`, `btree (tenant_id, spx_trip_type)` — added for the filter/sort combinations the new drawer exposes. `spx_origin_station`/`spx_dest_station` get a combined `btree (tenant_id, spx_origin_station, spx_dest_station)` (station filters are typically used together).

## Backend API Changes

**`BookingListItem`** (`api-gateway/src/routes/bookings.rs`) gains: `request_id`, `onsite_id`, `booking_number` (from `spx_tx_id`), `vehicle_type`, `deadline_at`, `pickup_time`, `trip_type`, `booking_type` (`"coc"` | `"reguler"`, from the existing `is_coc`-equivalent logic — reuse `booking_type_of`/`Spxid` mapping already in `core_domain`). All still derived via `spx_client::normalize_booking(&b.raw_data)` at read time for consistency with `BookingDetail` — the new generated columns are for **filtering/sorting/aggregation only**, not a second source of truth for display values. (This mirrors how `route` already works today.)

**`ListParams`/`BookingFilter`** (`api-gateway`/`store`) gains:
- `auto_accepted: Option<bool>` — exact match.
- `accept_reason: Option<String>` — validated against the known vocabulary (`taken_by_other`, `manual_accept_failed`, etc.), matches on `raw_data->>'accept_reason'` (no generated column needed — this key is written by our own code, not multi-key SPX data, so a direct JSONB operator in the `QueryBuilder` is sufficient).
- `vehicle_type: Option<String>` — exact match against `spx_vehicle_type`.
- `trip_type: Option<i32>` — exact match against `spx_trip_type`.
- `booking_type: Option<String>` — `"coc"`/`"reguler"`, matches existing `is_coc`.
- `origin_station` / `dest_station: Option<String>` — exact match against the new station columns.
- `weight_min` / `weight_max: Option<f64>` — range on existing `weight` column.
- `cod: Option<bool>` — `cod_amount > 0` when `true`, `= 0` when `false`.
- `pickup_from` / `pickup_to: Option<DateTime<Utc>>` — range on `spx_pickup_time`.
- `deadline_from` / `deadline_to: Option<DateTime<Utc>>` — range on `spx_deadline_at`.
- `sort: Option<SortKey>` — enum `{ NewestFirst (default, created_at desc), DeadlineSoonest (spx_deadline_at asc nulls last) }`. Only these two — the reference's "Urutkan" dropdown shows more options than we have real fields to back; start with the two that are unambiguous, extend later if needed.

**New endpoint `GET /bookings/summary`** (session-gated, same router as the rest of `/bookings`):
```
{
  "incoming_today": i64,
  "accepted_auto_today": i64,
  "accepted_manual_today": i64,
  "taken_by_other_today": i64,
  "latency_p99_ms": f64 | null
}
```
One SQL query, `COUNT(*) FILTER (WHERE ...)` per bucket plus `percentile_cont(0.99) WITHIN GROUP (ORDER BY accept_latency_ms) FILTER (WHERE auto_accepted AND accept_latency_ms IS NOT NULL)`, scoped to `created_at >= <today's WIB midnight, converted to UTC>`. "Today" is fixed WIB (UTC+7, no DST) — consistent with `spx_client`'s existing `format_times` convention — not per-tenant configurable (no tenant-timezone column exists, and TOWER is single-region).

**Dropdown data sources** (no new endpoints):
- Station Keberangkatan/Tujuan: reuse existing `GET /locations`.
- Armada: `SELECT DISTINCT spx_vehicle_type FROM bookings WHERE tenant_id = $1 AND spx_vehicle_type IS NOT NULL ORDER BY spx_vehicle_type` — new small query in `store::bookings`, exposed as `GET /bookings/vehicle-types` (cheap, index-backed via the new `spx_vehicle_type` index).

## Frontend Architecture

**`/command`** (`Frontend/src/routes/(app)/command/+page.svelte`):
- New `StatCard.svelte` — one KPI card (label, value, optional trend/unit), visually in the family of Image #1's cards but restyled to the existing dark Balanced Duo tokens (`--color-bg-surface`, `--color-accent`, etc.) rather than the reference's light theme — this is the "sesuaikan" the user asked for.
- New `Frontend/src/lib/api-command.ts`: `fetchSummary()` — polls `GET /bookings/summary` every 10s (tighter than the existing 20s ticket-list poll, since these are cheap aggregate queries and the user explicitly asked for minimal lag) + re-fetches immediately on any relevant WS event (`ticket_accepted`).
- Widget 1 (Latency) keeps using `LatencyTape`'s existing WS-sample mechanism, but is now **seeded** with `summary.latency_p99_ms` on mount so it shows a real number immediately instead of `0.00ms` until the first live sample arrives. If `latency_p99_ms` is `null` (no auto-accepts today), show an explicit "Belum ada data hari ini" empty state instead of a bare `0.00ms` — the current literal zero reads as broken/stuck, which is exactly the complaint being addressed.
- Widgets 2-5 are `StatCard`s with `onclick`, each setting a local `activeFilter` state (`incoming` | `taken` | `auto` | `manual`, default `incoming`) that determines which query the list below runs (`/bookings/live` for `incoming`, `/bookings/history?...` with the matching `auto_accepted`/`accept_reason` filter for the other three). The list itself reuses `TicketTicker`/the same row-rendering as today, restyled to sit visually under the new card row.

**`/tickets`**:
- `TicketsTable.svelte` — full rewrite of the column set (still real `<table>` desktop / stacked-card mobile, preserving the existing accessibility pattern: row focus handling, the button/`role="button"` sibling structure, 44px targets). New columns: **ID** (three stacked mono labels BK/REQ/OID from `booking_number`/`request_id`/`onsite_id`), **Booking Number** (`booking_number`), **Route & Vehicle** (existing `route` list + `vehicle_type` chip), **Jadwal Booking** (`pickup_time`), **Deadline Bidding** (`deadline_at` formatted + a live countdown, `<CountdownBadge>` reused for both the large and the small "STANDBY" display per the user's confirmation that both read the same value), **Tags** (`booking_type` → COC/REG chip, `trip_type` → ADHOC/FIX chip per the mapping in Open Questions), **Status** (existing), **Accept By** (always `—` — see Scope), **Action** (existing Terima button).
- New `FilterDrawer.svelte` replaces `TicketFilterBar.svelte` — a slide-in panel (not an inline row, given the field count) matching Image #3's grouping: Urutkan/Rentang waktu cepat, ID fields (Booking/Request/Nama), Rute, Station asal/tujuan (from `/locations`), Armada (from `/bookings/vehicle-types`), Tag Tiket (COC/REG/ADHOC/FIX, one flat list), Status, COD, Berat min/maks, two date-range groups (Periode booking, Batas waktu konfirmasi). Controlled component, same no-mutation contract as the current `TicketFilterBar` (`filters` in, `onFiltersChange` out) so `$lib/tickets.ts`'s existing filter-state handling doesn't need to change shape, only grow more fields. Needs a real focus trap (many interactive fields, not a single button like 7c's drawer) — follow 7d's `AutoAcceptSwitch` modal precedent for the trap implementation, not 7c's simpler single-element case.
- `$lib/tickets.ts`'s `TicketFilters` type grows to match the new `ListParams`; `$lib/api-tickets.ts`'s query-string builder grows accordingly.

## Testing

- **Backend (Rust):** new tests for the migration's generated columns (numeric-vehicle-type discard, `to_ms` boundary at `1e12`, multi-key priority order) mirroring the existing `spx-client` unit tests but as SQL-level fixtures in `store`'s test suite (real-Postgres convention, per this project's standing testing rule). `/bookings/summary` gets an integration test seeding known booking/accept-event rows and asserting exact counts. New `ListParams` filters each get at least one `api-gateway` route test.
- **Unit (Vitest):** `StatCard` value/trend rendering; `FilterDrawer`'s filter-object diffing (no-mutation contract); countdown formatting for `Deadline Bidding`.
- **E2E (Playwright):** `command.spec.ts` — widget click switches the list's query (assert the request URL/params change); `tickets.spec.ts` — new columns render seeded data correctly; a `FilterDrawer` open/filter/apply/reset round trip, including the keyboard focus-trap check (per this project's established pattern of writing one after 7c's drawer bug was only caught by an e2e test, not manual review).

## Open Questions for the Implementer

- **ADHOC/FIX mapping (`trip_type` 0/1) is the user's best recollection from operating the real SPX portal, not independently verified against a captured payload** (the codebase has no captured real SPX bodies at all — see `spx-client/src/booking.rs`'s own test-fixture disclaimer). Implement it as a single named constant/lookup (not scattered magic numbers) so it's a one-line fix if it turns out reversed once real SPX data flows through a connected tenant.
- **`spx_origin_station`/`spx_dest_station`'s simplified derivation** (route_detail_list only, no fallback chain) may show `NULL` for some real-world bookings that `normalize_booking`'s Rust-side full fallback chain would have resolved. If station-filter dropdown coverage turns out incomplete against real data, porting the fallback chain into SQL is the upgrade path (flagged in the Data Model table above, not silently accepted).
