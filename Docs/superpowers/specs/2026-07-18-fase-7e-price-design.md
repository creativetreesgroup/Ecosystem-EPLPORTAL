# Fase 7e: `/price` (Route Price List) — Design

**Status:** Approved for implementation.

## Context

The master spec (`Docs/tower-master-spec.md:183`) names `/price` as one of Fase 7's 7 surfaces, with no content-level detail beyond that. Project notes had loosely grouped `/price`, `/settings`, and `/activity` together as an indicative "Fase 7e" — research done during this brainstorm found the three are wildly different in size (`/price`: one resource, complete API, smallest; `/settings`: at least 3 separate built-but-unconsumed backend surfaces; `/activity`: a genuinely new TOWER concept with no reference-app equivalent) and should NOT be bundled into one phase, breaking the one-page-per-sub-phase discipline every prior Fase 7 phase has followed. This doc covers `/price` only, chosen as the next sub-phase for being the smallest and most fully decided.

**The backend for this sub-phase is already fully built** — Fase 6d shipped `GET/POST/PUT/DELETE /prices` and the `route_prices` table. This is a pure frontend build against existing, working backend surfaces, same as Fase 7d.

## Scope

**In scope:**
- A single page, `/price`, listing all of the tenant's route prices with client-side filter + pagination.
- Full CRUD: add, edit, delete a price row, reusing Fase 7d's `LocationCombobox` (origin/destinations, with inline location-create) and a closed-vocabulary vehicle-type picker built on the existing `ChipInput` (single-select mode, same 8-value vocabulary as `/rules`).
- Expand-in-place row editing (same proven pattern as `RuleRow.svelte`), with genuine **per-row** persistence (POST for a new row, PUT for an edit, DELETE) — NOT a batch/replace-all model like `/rules`, since the backend here is real per-resource CRUD.
- Read-only view for non-main-account users (view-only; `ManagePrices` gates all mutations).

**Out of scope (deliberately deferred, not silently dropped):**
- **Backend pagination.** `store::route_prices::list_all` has no `LIMIT`/`OFFSET` and returns the full tenant list. At the expected scale (tens to a few hundred routes, comparable to `route_locations`), client-side pagination over one fetched list is sufficient; adding server-side pagination is a follow-up if a tenant's route count ever grows into the thousands.
- **A public-facing price page.** `GET /prices` is already public/unauthenticated (rate-limited 120/min/IP) as a pre-existing backend design choice — likely serving some other consumer (e.g. a customer-facing quote tool) not part of this project's UI. This sub-phase only builds the internal, session-gated management page under `(app)/price`; it does not add any new public-facing surface.
- **Bulk import/export** (CSV, etc.) — not requested, no evidence of need.

## Backend

No changes. Existing surfaces this sub-phase depends on:
- `GET /prices` — public, rate-limited, no session. Returns `RoutePriceItem[]` (`{id, route_code, region, origin, destinations, price, vehicle_type}`, all snake_case, no `rename_all` in this crate). The `(app)/price` page still calls this same endpoint even though it doesn't require a session itself — the page is already behind `(app)/+layout.server.ts`'s session gate, so this is a non-issue; the frontend never needs to special-case the endpoint's own public-ness.
- `POST /prices`, `PUT/DELETE /prices/{id}` — `session_auth` + `Permission::ManagePrices` (main-account only). `PriceInput` body: `{route_code, region?, origin, destinations, price, vehicle_type}` (`region` has `#[serde(default)]`, defaults to `""`). A duplicate `(tenant_id, route_code)` on create/update surfaces as `409 Conflict` (`ApiError::From<sqlx::Error>`'s `23505` mapping) — the frontend must show a specific "Kode rute sudah dipakai" message on 409, not a generic error.
- **Validation mirrored client-side for immediate feedback** (server remains the real gate): `destinations` must be a JSON array of 1-5 non-empty strings (`routes/prices.rs::validate_destinations`, mirroring the DB's `route_prices_destinations_1to5` CHECK). `region` has no format constraint — free text, defaults to empty.
- **`vehicle_type` has no server-side canonicalization or CHECK constraint** (unlike `/rules`' `service_types`, which goes through `core_domain::sanitize_accept_rules`'s vehicle-label canonicalization). The 8-value vocabulary (`TRONTON`/`FUSO`/`CDD LONG`/`CDE LONG`/`BLINDVAN`/`WINGBOX`/`ENGKEL`/`40FCL`) is a **frontend UX consistency choice** with `/rules`, not a backend-enforced rule — whatever string the user picks is stored verbatim.

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/price/+page.svelte` — page assembly: fetch, filter/paginate, add-row flow, permission gating.
- `Frontend/src/lib/prices.ts` — pure logic: `PriceDraft` type, a client-side text-filter matcher (route_code/region/origin substring match), Rupiah formatting (`Rp` + thousand separators) for display. Unit-tested.
- `Frontend/src/lib/api-prices.ts` — typed REST helpers: `fetchPrices()`, `createPrice(draft)`, `updatePrice(id, draft)`, `deletePrice(id)`. Wire-shape mapping (snake_case ↔ camelCase), same split convention as `api-rules.ts`.
- `Frontend/src/lib/components/PriceRow.svelte` — one price row: collapsed summary (route_code, region, origin→destinations, formatted price, vehicle_type) + expand-in-place editing. Reuses `ChipInput` (vehicle_type, closed-vocab single-select — note: `ChipInput` today only supports multi-select semantics for closed-vocab mode; this task must either add a genuine single-select mode or wrap it, see Global Constraints) and `LocationCombobox` (origin single, destinations multi capped 5, with inline-create).
- Reuses as-is, no changes: `Frontend/src/lib/components/Pagination.svelte`, `Frontend/src/lib/components/LocationCombobox.svelte`, `Frontend/src/lib/components/ChipInput.svelte`.

**Data flow:** unlike `/rules`, this page's rows are genuinely independent resources — no `RulesPageState`-style "one big local draft, one big Save" model. Existing rows: expand → edit fields → an explicit per-row "Simpan" calls `updatePrice(id, draft)` immediately (PUT), "Hapus" calls `deletePrice(id)` immediately (DELETE, with a native `confirm()` given deletion has no undo — matching this codebase's one existing precedent for a destructive-action confirmation, the dirty-navigation guard in `/rules`). New rows: "+ Tambah Harga" adds an expanded, not-yet-persisted draft row locally; its own "Simpan" calls `createPrice(draft)` (POST) and, on success, replaces the local draft with the server-returned row (real `id`); its own "Batal" discards the draft with no network call. A 409 from either create or update shows "Kode rute sudah dipakai" inline on that row, not a page-level banner (the error is row-scoped, not global, unlike `/rules`' whole-set warnings).

**`ChipInput` single-select gap:** `ChipInput.svelte` (Fase 7d) was built with two modes — free-text multi-add, and closed-vocabulary multi-select (toggle any number of options). `vehicle_type` here needs closed-vocabulary **single**-select (pick exactly one of the 8). This sub-phase's plan must decide: add a `multi?: boolean` prop to `ChipInput` itself (defaulting to current multi-select behavior, `multi={false}` constrains to at most one active selection — toggling a new option deselects the previous one), or build a small dedicated single-select component. Given the shape is nearly identical to the existing closed-vocab rendering, extending `ChipInput` with a `multi` prop (mirroring `LocationCombobox`'s own `multi` prop convention) is the leaner choice — this is a small, additive change to an already-shipped, already-reviewed component, not a rewrite.

**Permissions:** view is available to any authenticated user (the page itself is behind the `(app)` session gate; the underlying `GET /prices` needs no permission check). `is_main_account` (already available via `data.user`, same pattern as `/rules`) gates all mutation affordances — non-main-account sees every row read-only, no add/edit/delete controls, and the same "Hanya akun utama yang dapat mengubah..." banner convention.

**Accessibility:** consistent with 7a-7d's bar — tokens-only styling, 44px primary tap targets, focus-visible rings, `role="alert" aria-live="polite"` for row-scoped errors, keyboard-operable expand/collapse (same non-nested-interactive-elements structure as `RuleRow.svelte`).

## Global Constraints (carried into the plan)

- `destinations` capped at 5, non-empty strings, order not semantically meaningful for price rows (unlike `/rules`' route matching) but keep consistent left-to-right display order regardless.
- `route_code` uniqueness is server-enforced (409) — never assume client-side uniqueness checking is sufficient.
- No backend changes in this plan. If an implementer believes one is needed (e.g. for pagination), that is a plan contradiction — escalate, don't silently add backend code.
- `ChipInput`'s new `multi` prop must default to preserving its current (multi-select) behavior for existing `/rules` call sites — this is a shared, already-shipped component; changing its default behavior would be a regression for `RuleRow.svelte`.

## Testing

- **Unit (Vitest):** `prices.ts` — filter matching, Rupiah formatting.
- **E2E (Playwright), `Frontend/tests/price.spec.ts`:** unauthenticated redirect; load and display seeded prices (reuse or extend existing e2e seed data — no `route_prices` row exists in any current seed, so this task must seed at least one, verified against the real migration schema); add a price end-to-end (including inline location creation) and verify persistence after reload; edit and delete a price; duplicate `route_code` shows the specific 409 message; non-main-account read-only view.

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming, including the `ChipInput` single-select extension. Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path.
