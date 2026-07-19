# Fase 7i: `/settings/locations` — Design

**Status:** Approved for implementation.

## Context

Third of five `/settings/*` sub-phases (see Fase 7g's design doc for the full decomposition decision). Fase 7g built the shared `/settings` shell + `/settings/branding`; Fase 7h added `/settings/bot`. This phase adds `/settings/locations` — a management page over the already-existing, partially-consumed `GET/POST/DELETE /locations` endpoint (Fase 6d). "Partially consumed" because `GET`/`POST` are already exercised today via `LocationCombobox` (used inline from `/rules` and `/price` to create a location on the fly while building a rule or price row) — but there is no dedicated page to VIEW the full list or DELETE a location. This phase closes that gap.

**RBAC matches Branding's pattern, not Bot's**: `GET /locations` is open to any authenticated session (any tenant member can see the list); only `POST`/`DELETE` require `Permission::ManageLocations` (main-account only). So this page is edit-gated like Branding — always visible in nav, real data shown to everyone, mutating controls disabled for non-main-account — NOT content-gated like Bot.

**Deleting a location is genuinely safe, verified by reading the schema directly**: `route_locations` (migration `0014_route_locations.sql`) has no incoming foreign keys from any other table. `accept_rules`' route-mode conditions and `route_prices.origin`/`.destinations` store location names as plain `TEXT`/`JSONB` strings, not `route_locations.id` references (confirmed by reading `0005_accept_rules.sql`/`0013_route_prices.sql` — corrected filename, whole-branch review finding: the file is NOT `0002_accept_rules_and_targets.sql`, which doesn't exist) — `route_locations` is purely a convenience list of known names for `LocationCombobox`'s autocomplete, decoupled from any rule/price that already used a name. Deleting a location never orphans or breaks existing rules/prices; it only stops that name from appearing as a suggestion for NEW rules/prices going forward (a user can still free-type the same name again, which `LocationCombobox`'s own inline-create flow would just re-insert).

**A duplicate name is a real, already-handled case**: `route_locations_tenant_name_unique` (migration 0014) + `ApiError`'s generic `sqlx::Error` conversion (`Backend/crates/api-gateway/src/error.rs`) means `POST /locations` with an existing name returns `409 {"error": "already exists"}` automatically — same pattern already proven in `/price`'s "duplicate route_code" e2e test.

**No rename/edit capability exists in the backend** (`route_locations` has no `updated_at` column, no update fn — confirmed in `Backend/crates/store/src/route_locations.rs`'s own header comment: "a location's name is either right or gets deleted and re-added, never edited in place"). This phase does not invent one.

## Scope

**In scope:**
- `/settings/locations` page: a flat, alphabetically-sorted list of the tenant's locations (the backend already returns them `ORDER BY name ASC`, no client-side sorting needed), with an "add new location" input+button at the top and a delete button (native `confirm()` guard, matching `/rules`/`/price`'s established delete precedent) per row.
- The "Locations" entry in the `/settings` shell's secondary nav, visible to ALL authenticated sessions (like Branding, unlike Bot) — the fix-point for the nav-array refactor Fase 7h's whole-branch review tracked (today's ternary, which only had one always-visible + one main-account-only entry, becomes awkward with a second always-visible entry — see Frontend Architecture below).
- Read-only view for non-main-account sessions (disabled add-input/button and disabled delete buttons), matching Branding's `<fieldset disabled>`-equivalent pattern — NOT Bot's content-gating, since `GET /locations` has no permission gate at all.

**Out of scope (deliberately deferred, not silently dropped):**
- **No pagination.** `GET /locations` has no `limit`/`offset` support at all (confirmed by reading the handler — it always returns the full list) and locations are expected to be a small, hand-curated set (pickup/dropoff points), not a high-cardinality resource like bookings or prices. A flat list is the right scale match; add pagination only if a real tenant's location count ever makes this unwieldy.
- **No search/filter box** — same reasoning as no-pagination; revisit only if list length becomes a real problem.
- **No rename/edit-in-place** — the backend has no update capability for this resource (add/delete only, by design, per the store module's own header comment). Not something this phase invents.
- **No "in use by N rules/prices" indicator on delete.** Confirmed via schema that deletion never breaks anything referencing a name, so there is no real warning to surface — a generic confirm() ("Hapus lokasi ini?") is sufficient, matching every other destructive-action precedent in this codebase. Do not build a usage-count feature nobody asked for and the data model doesn't cleanly support anyway (would require a reverse-scan of `accept_rules`/`route_prices` JSONB/text fields for name matches, a real feature with real cost, not a small addition).

## Backend

No changes. Existing surface (`Backend/crates/api-gateway/src/routes/locations.rs`):

- **`GET /locations`** — `session_auth` only (any authenticated tenant member). Returns `LocationItem[]`: `{id: uuid, name: string}`, alphabetically sorted by the backend.
- **`POST /locations`** — `Permission::ManageLocations` (main-account only). Body: `{name: string}`. `name` is trimmed and rejected as `400` if empty (`"name is required"`). A duplicate `(tenant_id, name)` returns `409 {"error": "already exists"}` (generic `sqlx::Error`→`ApiError` conversion, not location-specific code). Returns the created `LocationItem` (with its real server-assigned `id`).
- **`DELETE /locations/:id`** — `Permission::ManageLocations` (main-account only). Returns `204` on success, `404` if the id doesn't exist for this tenant (already-deleted / wrong tenant — RLS-scoped).

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/settings/+layout.svelte` — **modified**: the nav-entry logic is refactored from the current if/else-branch ternary (Fase 7h's tracked Minor) to a flat array of `{href, label, mainAccountOnly}` filtered by `data.user.is_main_account`. This phase is the natural point to do it — it's the first time a SECOND always-visible entry needs adding, which is exactly what made the ternary awkward.
- `Frontend/src/routes/(app)/settings/locations/+page.svelte` — page assembly: add-location form, location list, delete-with-confirm, read-only gating.
- `Frontend/src/lib/api-locations.ts` — typed REST helpers: `fetchLocations(): Promise<LocationItem[]>`, `createLocation(name: string): Promise<LocationItem>`, `deleteLocation(id: string): Promise<void>`. **Deliberately a NEW module, not a reuse of `Frontend/src/lib/api-rules.ts`'s existing `fetchLocations`/`createLocation`** (that module already has its own copy for `/rules`' inline-create flow) — duplicating the tiny wire-mapping logic here is cheaper and cleaner than introducing a shared cross-page dependency between `/rules` and `/settings/locations` for 3 one-line functions; matches this codebase's existing tolerance for small, page-scoped API modules over premature sharing (e.g. `/price` has its own `api-prices.ts` rather than importing from `/rules`).
- No new pure-logic module needed — there is no validation logic beyond "non-empty name," trivial enough to inline in the page component (unlike Branding's multi-field length limits or Bot's URL-format check).

**Data flow:**
- On mount (`onMount`, per the established SSR-safety convention), fetch the full location list.
- Add form: single text input + button (or Enter-to-submit). On submit, calls `createLocation(name)`; on success, the new item is appended to the local list (re-sorted client-side to match the backend's alphabetical order, since the new item's exact insertion point isn't known without a full refetch — a full refetch is also acceptable and simpler; either is fine, cheapest wins). **Error message convention (matches this codebase's established pattern, e.g. `/price`'s duplicate-route_code handling in `PriceRow.svelte`):** `api-locations.ts`'s `createLocation` throws a generic `ApiError` (it does not parse the response body — no module in this codebase does); the PAGE checks `err instanceof ApiError && err.status === 409` and shows its OWN hardcoded Indonesian message ("Lokasi ini sudah ada") — this is a client-side string keyed off the status code, not literally the backend's raw JSON body text. Any other error shows a generic fallback message.
- Delete: `confirm()` guard (native, matching `/rules`/`/price`), then `deleteLocation(id)`; on success, remove the item from the local list (no refetch needed — the backend has already confirmed the deletion).
- **RBAC:** `data.user.is_main_account` disables the add-input/button and every delete button for non-main-account sessions — the list itself always shows real data (never hidden), matching Branding's pattern exactly.

**The nav-array refactor** (Fase 7h's tracked Minor, resolved here):
```ts
type NavItem = { href: string; label: string; mainAccountOnly?: boolean };
const ALL_NAV_ITEMS: NavItem[] = [
  { href: '/settings/branding', label: 'Branding' },
  { href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
  { href: '/settings/locations', label: 'Lokasi' }
];
const NAV_ITEMS = $derived(ALL_NAV_ITEMS.filter((item) => !item.mainAccountOnly || data.user.is_main_account));
```
This scales cleanly to 7j (Sub-users, main-account-only like Bot) and 7k (SPX Credentials, open like Branding/Locations) without further restructuring.

**Accessibility:** consistent with 7a-7h's bar — tokens-only styling, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error banners, `<svelte:head><title>Lokasi — TOWER</title></svelte:head>`, native `confirm()` for delete (matching `/rules`/`/price`'s precedent, not a custom modal).

## Testing

- **Unit (Vitest):** `api-locations.ts` — wire mapping (`LocationItem` round-trip), `fetchLocations`/`createLocation`/`deleteLocation` HTTP method/URL correctness (notably `DELETE /locations/:id` — the id must be interpolated into the path, not sent as a body), and a `deleteLocation` test confirming it doesn't attempt to parse a body on the `204` response (matching `clearBotLogs`'s established precedent for handling no-content responses).
- **E2E (Playwright), `Frontend/tests/settings-locations.spec.ts`:** unauthenticated visit redirects to `/login`; main-account session sees the "Locations" nav entry and the real existing location list; adding a new location persists it (visible after reload); deleting a location (confirm dialog accepted) removes it (gone after reload); a duplicate-name add shows the page's own "Lokasi ini sudah ada" message (409-status-driven, matching `/price`'s established convention); non-main-account session sees the real list but with disabled add/delete controls (read-only, matching `/rules`'/`/price`'s non-main-account test pattern — NOT a hidden-nav-entry test, since this resource is open like Branding).

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming (list-vs-pagination scale, delete safety confirmed via schema inspection, the nav-array refactor point). Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path.
