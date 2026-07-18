# Fase 7e: `/price` (Route Price List) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `/price`, a full CRUD editor for TOWER's route price list — a pure frontend build against an already-complete backend, reusing Fase 7d's `LocationCombobox`/`ChipInput`/`Pagination` components.

**Architecture:** Unlike `/rules`' local-edit-then-batch-Save model, `/price`'s backend is genuine per-resource REST (`POST` creates one row, `PUT`/`DELETE` act on one row by id) — so each row persists independently, immediately, on its own Save/Delete action. Expand-in-place rows (same proven pattern as `RuleRow.svelte`), client-side filter + pagination over one fetched list (no backend pagination — the expected scale is tens to a few hundred rows).

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), `@lucide/svelte`, Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-18-fase-7e-price-design.md` — read it first for full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format is snake_case** — no `#[serde(rename_all)]` anywhere in `api-gateway` (established convention, re-verified against `Backend/crates/api-gateway/src/routes/prices.rs` for this plan).
- **This is genuine per-resource CRUD, not a replace-all model.** `createPrice` returns the server-assigned row (with its real `id`) — the caller must replace its local draft with that response, not assume the client-supplied data is now "saved as-is." `updatePrice`/`deletePrice` act on a specific `id`.
- **`destinations` is capped at 5 non-empty strings** (DB CHECK `route_prices_destinations_1to5`, HTTP-layer-validated in `routes/prices.rs::validate_destinations`) — `LocationCombobox`'s existing `multi max={5}` mode already enforces this cap and non-empty-string invariant by construction; no extra client-side validation needed beyond using that component correctly.
- **`route_code` uniqueness is server-enforced** (`(tenant_id, route_code)` unique constraint) — a duplicate on create/update surfaces as `409 Conflict`. The frontend MUST show a specific message ("Kode rute sudah dipakai") on 409, distinct from other error statuses.
- **`vehicle_type` has no server-side canonicalization** — the 8-value vocabulary (`TRONTON`, `FUSO`, `CDD LONG`, `CDE LONG`, `BLINDVAN`, `WINGBOX`, `ENGKEL`, `40FCL`, same as `/rules`' `SERVICE_TYPE_OPTIONS`) is a frontend UX consistency choice only — whatever string is picked is stored verbatim, no backend validation to rely on.
- **`ChipInput.svelte` gets a new `multi` prop** (default `true`, preserving every existing call site's behavior unchanged) to support genuine single-select closed-vocabulary mode for `vehicle_type`. This is a shared, already-shipped, already-reviewed component consumed by `/rules`' `RuleRow.svelte` — any change must not alter that existing call site's behavior. `RuleRow.svelte` itself is NOT modified by this plan (it simply never passes the new prop, so it keeps today's multi-select behavior via the default).
- **Accessibility bar (established 7a-7d convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block, `min-h-[44px]`/`min-w-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error banners, glyph+text (never color-only) status, every interaction keyboard-operable, no drag-only affordance, non-nested-interactive-elements row layout (matching `RuleRow.svelte`'s proven structural-siblings pattern — collapsed-row toggle region, enabled-state, and delete button as siblings, never nested).
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `prices.ts` — pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/prices.ts`
- Test: `Frontend/src/lib/prices.test.ts`

**Interfaces:**
- Produces (consumed by Tasks 2, 4, 5): `PriceDraft` type; `newPriceDraft(): PriceDraft`; `formatRupiah(amount: number): string`; `matchesFilter(draft: PriceDraft, query: string): boolean`; `priceDraftIsValid(draft: PriceDraft): boolean`.

- [ ] **Step 1: Write the failing test — types, `newPriceDraft`, `formatRupiah`**

```typescript
// Frontend/src/lib/prices.test.ts
import { describe, it, expect } from 'vitest';
import { newPriceDraft, formatRupiah, matchesFilter, priceDraftIsValid, type PriceDraft } from './prices';

describe('newPriceDraft', () => {
	it('creates an empty draft with a fresh clientKey and no server id', () => {
		const draft = newPriceDraft();
		expect(draft.id).toBeNull();
		expect(draft.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(draft.routeCode).toBe('');
		expect(draft.region).toBe('');
		expect(draft.origin).toBe('');
		expect(draft.destinations).toEqual([]);
		expect(draft.price).toBe(0);
		expect(draft.vehicleType).toBe('');
	});

	it('two calls produce different clientKeys', () => {
		expect(newPriceDraft().clientKey).not.toBe(newPriceDraft().clientKey);
	});
});

describe('formatRupiah', () => {
	it('formats with Indonesian thousand separators and an Rp prefix', () => {
		expect(formatRupiah(1500000)).toBe('Rp 1.500.000');
	});

	it('formats zero', () => {
		expect(formatRupiah(0)).toBe('Rp 0');
	});

	it('formats a small amount with no separator needed', () => {
		expect(formatRupiah(500)).toBe('Rp 500');
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/prices.test.ts`
Expected: FAIL — `Cannot find module './prices'`.

- [ ] **Step 3: Write the types, `newPriceDraft`, `formatRupiah`**

```typescript
// Frontend/src/lib/prices.ts
// Pure logic for the /price route price list — no fetch, no DOM. Wire-format mapping lives in
// api-prices.ts, matching the rules.ts/api-rules.ts split established in Fase 7d.

export type PriceDraft = {
	/** Ephemeral, client-generated — for Svelte {#each} keying only, same discipline as
	 * rules.ts's RuleDraft.clientKey (never sent to the server, never used for list identity). */
	clientKey: string;
	/** The server's Uuid for this row, or null if it has never been saved. Unlike /rules, this
	 * DOES round-trip meaningfully — /price's backend is per-resource CRUD (PUT/DELETE act on a
	 * real id), so a saved row's id is load-bearing, not merely informational. */
	id: string | null;
	routeCode: string;
	region: string;
	origin: string;
	destinations: string[];
	price: number;
	vehicleType: string;
};

export function newPriceDraft(): PriceDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: null,
		routeCode: '',
		region: '',
		origin: '',
		destinations: [],
		price: 0,
		vehicleType: ''
	};
}

const RUPIAH_FORMATTER = new Intl.NumberFormat('id-ID');

export function formatRupiah(amount: number): string {
	return `Rp ${RUPIAH_FORMATTER.format(amount)}`;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/prices.test.ts`
Expected: PASS (5/5 so far).

- [ ] **Step 5: Write the failing test — `matchesFilter` and `priceDraftIsValid`**

```typescript
// Append to Frontend/src/lib/prices.test.ts

function draft(overrides: Partial<PriceDraft> = {}): PriceDraft {
	return { ...newPriceDraft(), ...overrides };
}

describe('matchesFilter', () => {
	it('matches on routeCode, case-insensitively', () => {
		expect(matchesFilter(draft({ routeCode: 'JKT-BDG-01' }), 'jkt')).toBe(true);
	});

	it('matches on region', () => {
		expect(matchesFilter(draft({ region: 'Sumatra' }), 'sumat')).toBe(true);
	});

	it('matches on origin', () => {
		expect(matchesFilter(draft({ origin: 'Padang DC' }), 'padang')).toBe(true);
	});

	it('empty query matches everything', () => {
		expect(matchesFilter(draft(), '')).toBe(true);
	});

	it('no match returns false', () => {
		expect(matchesFilter(draft({ routeCode: 'JKT-BDG-01', region: 'Jawa', origin: 'Jakarta DC' }), 'zzz')).toBe(
			false
		);
	});
});

describe('priceDraftIsValid', () => {
	it('a fully-empty draft is invalid', () => {
		expect(priceDraftIsValid(newPriceDraft())).toBe(false);
	});

	it('valid once routeCode, origin, destinations, vehicleType are set and price is positive', () => {
		const valid = draft({
			routeCode: 'JKT-BDG-01',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			vehicleType: 'TRONTON',
			price: 150000
		});
		expect(priceDraftIsValid(valid)).toBe(true);
	});

	it('invalid when price is zero or negative', () => {
		const zeroPrice = draft({
			routeCode: 'JKT-BDG-01',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			vehicleType: 'TRONTON',
			price: 0
		});
		expect(priceDraftIsValid(zeroPrice)).toBe(false);
	});

	it('invalid when destinations is empty', () => {
		const noDest = draft({
			routeCode: 'JKT-BDG-01',
			origin: 'Jakarta DC',
			destinations: [],
			vehicleType: 'TRONTON',
			price: 150000
		});
		expect(priceDraftIsValid(noDest)).toBe(false);
	});

	it('region is never required (may stay empty)', () => {
		const noRegion = draft({
			routeCode: 'JKT-BDG-01',
			region: '',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			vehicleType: 'TRONTON',
			price: 150000
		});
		expect(priceDraftIsValid(noRegion)).toBe(true);
	});
});
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/prices.test.ts`
Expected: FAIL — `matchesFilter`/`priceDraftIsValid` are not exported.

- [ ] **Step 7: Implement `matchesFilter` and `priceDraftIsValid`**

```typescript
// Append to Frontend/src/lib/prices.ts

export function matchesFilter(draft: PriceDraft, query: string): boolean {
	const q = query.trim().toLowerCase();
	if (q === '') return true;
	return (
		draft.routeCode.toLowerCase().includes(q) ||
		draft.region.toLowerCase().includes(q) ||
		draft.origin.toLowerCase().includes(q)
	);
}

/** Mirrors the fields the Save button should gate on client-side, for immediate feedback — the
 * server remains the real validator (destinations 1-5 non-empty, route_code uniqueness via 409). */
export function priceDraftIsValid(draft: PriceDraft): boolean {
	return (
		draft.routeCode.trim() !== '' &&
		draft.origin.trim() !== '' &&
		draft.destinations.length > 0 &&
		draft.vehicleType.trim() !== '' &&
		draft.price > 0
	);
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/prices.test.ts`
Expected: PASS — all tests in the file green (16 tests).

- [ ] **Step 9: Run svelte-check and commit**

Run: `cd Frontend && pnpm check && pnpm vitest run src/lib/prices.test.ts`
Expected: `0 ERRORS 0 WARNINGS`, all `prices.test.ts` tests passing.

```bash
git add Frontend/src/lib/prices.ts Frontend/src/lib/prices.test.ts
git commit -m "feat(frontend): prices.ts — pure /price logic (types, Rupiah formatting, filter, validation)"
```

---

### Task 2: `api-prices.ts` — typed REST helpers

**Files:**
- Create: `Frontend/src/lib/api-prices.ts`
- Test: `Frontend/src/lib/api-prices.test.ts`

**Interfaces:**
- Consumes: `PriceDraft`, `newPriceDraft` (Task 1); `apiPost`, `ApiError` (`Frontend/src/lib/api.ts`, existing).
- Produces (consumed by Task 5): `fetchPrices(): Promise<PriceDraft[]>`; `createPrice(draft: PriceDraft): Promise<PriceDraft>`; `updatePrice(id: string, draft: PriceDraft): Promise<PriceDraft>`; `deletePrice(id: string): Promise<void>`.

**Wire shapes** (snake_case, verified directly against `Backend/crates/api-gateway/src/routes/prices.rs` — no `rename_all` anywhere in this crate):

```
GET /prices -> RoutePriceItem[] { id: uuid, route_code: string, region: string, origin: string,
  destinations: string[], price: i64, vehicle_type: string }
POST /prices body (PriceInput, no id) -> RoutePriceItem
PUT /prices/{id} body (PriceInput, no id) -> RoutePriceItem
DELETE /prices/{id} -> 204 No Content (no body)
```

`PriceInput` (POST/PUT body): `{ route_code: string, region?: string, origin: string, destinations: string[], price: number, vehicle_type: string }`. `region` has `#[serde(default)]` server-side — omitting it is equivalent to `""`, but this module always sends it explicitly (simpler, no behavioral difference).

- [ ] **Step 1: Write the failing test — wire mapping round-trip**

```typescript
// Frontend/src/lib/api-prices.test.ts
// No network for the pure mapping — fetchPrices/createPrice/updatePrice/deletePrice themselves
// are exercised for real by Frontend/tests/price.spec.ts (Task 6) against a live backend, PLUS a
// vi.stubGlobal('fetch', ...) regression guard here for the one load-bearing HTTP-method detail
// this module has (PUT for update, DELETE for delete — neither of which apiPost can send), same
// precedent as api-rules.test.ts's saveSettings guard (Fase 7d, added after a review finding
// that the brief's own "no test needed" reasoning cited a false precedent).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { priceOutputToDraft, draftToPriceInput, fetchPrices, updatePrice, deletePrice } from './api-prices';
import { newPriceDraft } from './prices';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('priceOutputToDraft', () => {
	it('maps every snake_case field to its camelCase PriceDraft equivalent', () => {
		const wire = {
			id: 'server-uuid-1',
			route_code: 'JKT-BDG-01',
			region: 'Jawa',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			price: 150000,
			vehicle_type: 'TRONTON'
		};
		const draft = priceOutputToDraft(wire);
		expect(draft.id).toBe('server-uuid-1');
		expect(draft.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(draft.routeCode).toBe('JKT-BDG-01');
		expect(draft.region).toBe('Jawa');
		expect(draft.origin).toBe('Jakarta DC');
		expect(draft.destinations).toEqual(['Bandung DC']);
		expect(draft.price).toBe(150000);
		expect(draft.vehicleType).toBe('TRONTON');
	});
});

describe('draftToPriceInput', () => {
	it('maps every camelCase PriceDraft field to its snake_case wire equivalent, omitting id/clientKey', () => {
		const draft = {
			...newPriceDraft(),
			routeCode: 'JKT-BDG-01',
			region: 'Jawa',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			price: 150000,
			vehicleType: 'TRONTON'
		};
		const wire = draftToPriceInput(draft);
		expect(wire).not.toHaveProperty('id');
		expect(wire).not.toHaveProperty('clientKey');
		expect(wire.route_code).toBe('JKT-BDG-01');
		expect(wire.region).toBe('Jawa');
		expect(wire.origin).toBe('Jakarta DC');
		expect(wire.destinations).toEqual(['Bandung DC']);
		expect(wire.price).toBe(150000);
		expect(wire.vehicle_type).toBe('TRONTON');
	});
});

describe('fetchPrices', () => {
	it('issues a GET to /prices', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([]), { status: 200 });
			})
		);
		await fetchPrices();
		expect(calledUrl).toBe('/prices');
	});
});

describe('updatePrice', () => {
	it('issues a PUT to /prices/{id} with the mapped body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(
					JSON.stringify({
						id: 'x',
						route_code: 'JKT-BDG-01',
						region: '',
						origin: 'Jakarta DC',
						destinations: ['Bandung DC'],
						price: 150000,
						vehicle_type: 'TRONTON'
					}),
					{ status: 200 }
				);
			})
		);
		const draft = { ...newPriceDraft(), routeCode: 'JKT-BDG-01', origin: 'Jakarta DC', destinations: ['Bandung DC'], price: 150000, vehicleType: 'TRONTON' };
		await updatePrice('server-id-1', draft);
		expect(calledUrl).toBe('/prices/server-id-1');
		expect(calledInit?.method).toBe('PUT');
		const body = JSON.parse(calledInit?.body as string);
		expect(body.route_code).toBe('JKT-BDG-01');
	});
});

describe('deletePrice', () => {
	it('issues a DELETE to /prices/{id} and does not attempt to parse a body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(null, { status: 204 });
			})
		);
		await deletePrice('server-id-1');
		expect(calledUrl).toBe('/prices/server-id-1');
		expect(calledInit?.method).toBe('DELETE');
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-prices.test.ts`
Expected: FAIL — `Cannot find module './api-prices'`.

- [ ] **Step 3: Implement `api-prices.ts`**

```typescript
// Frontend/src/lib/api-prices.ts
// Thin typed REST layer for /price — no UI logic here. Wire shapes verified directly against
// Backend/crates/api-gateway/src/routes/prices.rs (snake_case, no rename_all anywhere in
// api-gateway). Genuine per-resource CRUD (unlike /rules' replace-all /bookings/settings) — POST
// creates exactly one row and returns it with a real server id; PUT/DELETE act on that id.
import { apiPost, ApiError } from './api';
import { newPriceDraft, type PriceDraft } from './prices';

type RoutePriceItemWire = {
	id: string;
	route_code: string;
	region: string;
	origin: string;
	destinations: string[];
	price: number;
	vehicle_type: string;
};

type PriceInputWire = Omit<RoutePriceItemWire, 'id'>;

export function priceOutputToDraft(wire: RoutePriceItemWire): PriceDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: wire.id,
		routeCode: wire.route_code,
		region: wire.region,
		origin: wire.origin,
		destinations: wire.destinations,
		price: wire.price,
		vehicleType: wire.vehicle_type
	};
}

export function draftToPriceInput(draft: PriceDraft): PriceInputWire {
	return {
		route_code: draft.routeCode,
		region: draft.region,
		origin: draft.origin,
		destinations: draft.destinations,
		price: draft.price,
		vehicle_type: draft.vehicleType
	};
}

export async function fetchPrices(): Promise<PriceDraft[]> {
	const res = await fetch('/prices', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch prices');
	const items: RoutePriceItemWire[] = await res.json();
	return items.map(priceOutputToDraft);
}

export async function createPrice(draft: PriceDraft): Promise<PriceDraft> {
	const wire = await apiPost<RoutePriceItemWire>('/prices', draftToPriceInput(draft));
	return priceOutputToDraft(wire);
}

/** `apiPost` hardcodes `method: 'POST'` — this is `PUT /prices/{id}`, so it cannot use `apiPost`;
 * raw `fetch` with `method: 'PUT'`, same pattern `api-rules.ts`'s `saveSettings` already
 * established for the identical PUT-vs-apiPost situation. */
export async function updatePrice(id: string, draft: PriceDraft): Promise<PriceDraft> {
	const res = await fetch(`/prices/${id}`, {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(draftToPriceInput(draft))
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to update price');
	const wire: RoutePriceItemWire = await res.json();
	return priceOutputToDraft(wire);
}

/** `DELETE /prices/{id}` returns `204 No Content` on success — never call `res.json()` on this
 * response, there is no body to parse. */
export async function deletePrice(id: string): Promise<void> {
	const res = await fetch(`/prices/${id}`, { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to delete price');
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-prices.test.ts`
Expected: PASS (6/6).

- [ ] **Step 5: Run svelte-check and full vitest, commit**

Run: `cd Frontend && pnpm check && pnpm vitest run`
Expected: `0 ERRORS 0 WARNINGS`; all suites passing (no regression in any pre-existing test file).

```bash
git add Frontend/src/lib/api-prices.ts Frontend/src/lib/api-prices.test.ts
git commit -m "feat(frontend): api-prices.ts — typed REST layer for /prices CRUD"
```

---

### Task 3: Extend `ChipInput.svelte` with a `multi` prop (regression-safe)

**Files:**
- Modify: `Frontend/src/lib/components/ChipInput.svelte`

**Interfaces:**
- Produces (consumed by Task 4): a new optional prop `multi?: boolean` (default `true`). When `true` (or omitted — every existing caller, i.e. `RuleRow.svelte`'s 3 closed-vocab usages, omits this prop and MUST see zero behavior change), closed-vocabulary mode behaves exactly as today (any number of options togglable independently). When `false`, closed-vocabulary mode becomes single-select: clicking an unselected option replaces the current selection (`value` becomes `[optValue]`); clicking the currently-selected option again clears the selection (`value` becomes `[]`) — this lets a "nothing chosen yet" state exist, which `/price`'s `vehicleType` genuinely needs for a fresh draft.

**This task does NOT touch `RuleRow.svelte`** — it keeps calling `ChipInput` without the `multi` prop, so it silently keeps today's multi-select behavior via the new prop's default. Do not modify `RuleRow.svelte` in this task.

- [ ] **Step 1: Modify the component**

Read the current file first (`Frontend/src/lib/components/ChipInput.svelte`) to confirm you're editing the real current version, not an assumed one — then make exactly these changes:

1. Add `multi = true` to the destructured props (with type `multi?: boolean`):

```typescript
	let {
		label,
		value,
		onChange,
		options,
		multi = true
	}: {
		label: string;
		value: string[];
		onChange: (value: string[]) => void;
		options?: { value: string; label: string }[];
		multi?: boolean;
	} = $props();
```

2. Replace the existing `toggleOption` function body with:

```typescript
	function toggleOption(optValue: string) {
		if (multi) {
			if (value.includes(optValue)) {
				onChange(value.filter((v) => v !== optValue));
			} else {
				onChange([...value, optValue]);
			}
		} else {
			// Single-select: clicking the already-selected option clears it (allows a genuine
			// "nothing chosen yet" state); clicking any other option replaces the selection.
			onChange(value.includes(optValue) ? [] : [optValue]);
		}
	}
```

3. In the closed-vocabulary markup block (the `{#if options}` branch), change the wrapping `<div>`'s `role` and each `<button>`'s ARIA attributes to reflect single-vs-multi semantics — multi-select keeps today's `role="group"`/`aria-pressed` (a set of independent toggles); single-select uses `role="radiogroup"`/`role="radio"`/`aria-checked` (an exclusive choice), matching the same radiogroup convention `RuleRow.svelte`'s own mode/booking-type selectors already use elsewhere in this codebase:

```svelte
	{#if options}
		<div
			class="flex flex-wrap gap-1.5"
			role={multi ? 'group' : 'radiogroup'}
			aria-labelledby={`${inputId}-label`}
		>
			{#each options as opt (opt.value)}
				<button
					type="button"
					role={multi ? undefined : 'radio'}
					aria-pressed={multi ? value.includes(opt.value) : undefined}
					aria-checked={multi ? undefined : value.includes(opt.value)}
					onclick={() => toggleOption(opt.value)}
					class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
						value.includes(opt.value)
							? 'bg-accent text-bg-base border-accent'
							: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
					}`}
				>
					{opt.label}
				</button>
			{/each}
		</div>
	{:else}
```

(The `{:else}` branch — free-text mode — is completely unchanged; only the `{#if options}` branch's wrapper `<div>` and inner `<button>` attributes change.)

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Regression-check the existing `/rules` call site**

`RuleRow.svelte` calls `ChipInput` three times in closed-vocabulary mode (service types, shift types, trip types), none passing `multi` — confirm by reading `Frontend/src/lib/components/RuleRow.svelte` that none of these three call sites need any change, and that with `multi` defaulting to `true`, their rendered `aria-pressed`/`role="group"` output is byte-identical to before this task's edit (the `multi ? X : Y` ternaries all resolve to the pre-existing `X` branch when `multi` is `true`).

If you have a way to spot-check this live (dev server + Playwright), open `/rules`, expand a rule, and confirm the service-type/shift/trip chip pickers still allow selecting multiple options simultaneously (unchanged behavior).

- [ ] **Step 4: Commit**

```bash
git add Frontend/src/lib/components/ChipInput.svelte
git commit -m "feat(frontend): ChipInput — add multi prop for single-select closed-vocabulary mode (default true, no change to existing /rules usage)"
```

---

### Task 4: `PriceRow.svelte`

No unit test — component-only, verified via `svelte-check` + Task 6's e2e suite (established convention).

**Files:**
- Create: `Frontend/src/lib/components/PriceRow.svelte`

**Interfaces:**
- Consumes: `PriceDraft`, `formatRupiah`, `priceDraftIsValid` (Task 1); `createPrice`, `updatePrice`, `deletePrice` (Task 2); `LocationItem`, `ApiError` (existing); `ChipInput.svelte` (Task 3, with the new `multi` prop); `LocationCombobox.svelte` (Fase 7d, unchanged).
- Produces (consumed by Task 5): a component with props `{ draft: PriceDraft; locations: LocationItem[]; onCreateLocation: (name: string) => Promise<LocationItem>; onSaved: (saved: PriceDraft) => void; onRemove: () => void; readOnly: boolean }`. `onSaved` fires after a successful create OR update, with the server-confirmed row. `onRemove` fires after a successful delete OR when "Batal" discards a not-yet-saved new row (no network call in that case) — the page treats both the same way (remove this row from its list), so one callback covers both.

**Key difference from `RuleRow.svelte` (Fase 7d)**: this component owns its OWN local edit-in-progress copy of the draft (`local`, initialized once from the `draft` prop) — it does NOT propagate every keystroke up to the parent via `onChange` the way `RuleRow` does, because `/price` has no page-level "dirty" tracking or single global Save; each row persists independently on its own explicit Save action. A row whose `draft.id === null` is a not-yet-saved new row: it starts expanded automatically, its Save button calls `createPrice`, and it additionally shows a "Batal" button. An existing row (`draft.id !== null`) starts collapsed, its Save button calls `updatePrice`, and has no "Batal" (no undo for edits to an already-saved row in this scope — reloading the page is the escape hatch, matching this plan's YAGNI discipline).

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/PriceRow.svelte -->
<!-- One route_prices row: collapsed summary + expand-in-place editor. Unlike RuleRow.svelte
     (Fase 7d), this component owns its own local edit-in-progress state and persists on its own
     explicit Save action (createPrice/updatePrice) — /price's backend is genuine per-resource
     CRUD, not a replace-all model, so there is no page-level batch Save to propagate edits into. -->
<script lang="ts">
	import { Trash2 } from '@lucide/svelte';
	import { formatRupiah, priceDraftIsValid, type PriceDraft } from '$lib/prices';
	import { createPrice, updatePrice, deletePrice } from '$lib/api-prices';
	import { ApiError } from '$lib/api';
	import type { LocationItem } from '$lib/api-rules';
	import ChipInput from './ChipInput.svelte';
	import LocationCombobox from './LocationCombobox.svelte';

	const SERVICE_TYPE_OPTIONS = ['TRONTON', 'FUSO', 'CDD LONG', 'CDE LONG', 'BLINDVAN', 'WINGBOX', 'ENGKEL', '40FCL'];

	let {
		draft,
		locations,
		onCreateLocation,
		onSaved,
		onRemove,
		readOnly
	}: {
		draft: PriceDraft;
		locations: LocationItem[];
		onCreateLocation: (name: string) => Promise<LocationItem>;
		onSaved: (saved: PriceDraft) => void;
		onRemove: () => void;
		readOnly: boolean;
	} = $props();

	let local = $state<PriceDraft>({ ...draft });
	// Reactive to local.id, NOT computed once from the `draft` prop — after a successful create,
	// `local.id` is updated to the server-assigned id (see save() below) and this must flip to
	// false immediately, or a subsequent edit+save on the SAME now-persisted row would call
	// createPrice again instead of updatePrice (duplicate row / spurious 409).
	const isNew = $derived(local.id === null);
	let expanded = $state(local.id === null);
	let saving = $state(false);
	let deleting = $state(false);
	let errorMsg = $state('');

	function updateLocal(patch: Partial<PriceDraft>) {
		local = { ...local, ...patch };
	}

	const summary = $derived(
		`${local.origin || '—'} → ${local.destinations.length > 0 ? local.destinations.join(' → ') : '—'} · ${formatRupiah(local.price)}`
	);

	async function save() {
		if (!priceDraftIsValid(local)) {
			errorMsg = 'Lengkapi semua field yang wajib diisi (kode rute, asal, min. 1 tujuan, jenis kendaraan, harga > 0).';
			return;
		}
		saving = true;
		errorMsg = '';
		try {
			const saved = isNew ? await createPrice(local) : await updatePrice(local.id as string, local);
			// Adopt the server-confirmed id/fields, but keep OUR OWN clientKey stable — the parent
			// keys its {#each} on clientKey, and preserving it here (rather than taking whatever
			// fresh clientKey api-prices.ts's priceOutputToDraft generated) avoids an unnecessary
			// remount of this component on every successful save.
			local = { ...saved, clientKey: local.clientKey };
			onSaved(local);
		} catch (e) {
			if (e instanceof ApiError && e.status === 409) {
				errorMsg = 'Kode rute sudah dipakai.';
			} else {
				errorMsg = 'Gagal menyimpan. Coba lagi.';
			}
		} finally {
			saving = false;
		}
	}

	async function del() {
		if (!confirm(`Hapus harga untuk rute "${local.routeCode}"?`)) return;
		deleting = true;
		errorMsg = '';
		try {
			await deletePrice(local.id as string);
			onRemove();
		} catch {
			errorMsg = 'Gagal menghapus. Coba lagi.';
			deleting = false;
		}
	}
</script>

<div class="rounded-lg border border-border bg-bg-surface">
	<div class="flex items-center gap-3 p-3">
		<div
			role="button"
			tabindex="0"
			aria-expanded={expanded}
			onclick={() => (expanded = !expanded)}
			onkeydown={(e) => {
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					expanded = !expanded;
				}
			}}
			class="flex-1 flex flex-col gap-0.5 cursor-pointer rounded-md px-1 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			<div class="flex items-center gap-2">
				<span class="text-[13px] font-heading font-medium text-text-primary">{local.routeCode || 'Rute baru'}</span>
				{#if local.region}
					<span class="text-[10px] font-body px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-muted">
						{local.region}
					</span>
				{/if}
			</div>
			<span class="text-[11px] font-mono text-text-muted">{summary}</span>
		</div>

		{#if !readOnly}
			<button
				type="button"
				onclick={del}
				disabled={deleting || isNew}
				aria-label={`Hapus harga rute ${local.routeCode || 'baru'}`}
				class="min-h-[36px] min-w-[36px] flex items-center justify-center rounded text-text-muted hover:text-danger disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<Trash2 size={14} aria-hidden="true" />
			</button>
		{/if}
	</div>

	{#if expanded}
		<fieldset disabled={readOnly} class="flex flex-col gap-3 p-3 pt-0 border-0">
			{#if errorMsg}
				<p role="alert" aria-live="polite" class="text-[12px] text-danger">{errorMsg}</p>
			{/if}

			<div class="grid grid-cols-2 gap-3">
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Kode Rute</span>
					<input
						type="text"
						value={local.routeCode}
						oninput={(e) => updateLocal({ routeCode: (e.target as HTMLInputElement).value })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Region</span>
					<input
						type="text"
						value={local.region}
						oninput={(e) => updateLocal({ region: (e.target as HTMLInputElement).value })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>

			<LocationCombobox
				label="Asal"
				{locations}
				{onCreateLocation}
				value={local.origin ? [local.origin] : []}
				onChange={(v) => updateLocal({ origin: v[0] ?? '' })}
			/>

			<LocationCombobox
				label="Tujuan (maks 5)"
				{locations}
				{onCreateLocation}
				value={local.destinations}
				onChange={(v) => updateLocal({ destinations: v })}
				multi
				max={5}
			/>

			<ChipInput
				label="Jenis Kendaraan"
				value={local.vehicleType ? [local.vehicleType] : []}
				onChange={(v) => updateLocal({ vehicleType: v[0] ?? '' })}
				options={SERVICE_TYPE_OPTIONS.map((v) => ({ value: v, label: v }))}
				multi={false}
			/>

			<label class="flex flex-col gap-1 max-w-[200px]">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Harga (Rp)</span>
				<input
					type="number"
					min="0"
					value={local.price}
					oninput={(e) => updateLocal({ price: Number((e.target as HTMLInputElement).value) || 0 })}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-mono bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<div class="flex gap-2">
				<button
					type="button"
					onclick={save}
					disabled={saving}
					class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					{saving ? 'Menyimpan…' : 'Simpan'}
				</button>
				{#if isNew}
					<button
						type="button"
						onclick={onRemove}
						class="min-h-[44px] px-4 rounded-md text-[13px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						Batal
					</button>
				{/if}
			</div>
		</fieldset>
	{/if}
</div>
```

**Note on the `SERVICE_TYPE_OPTIONS` duplication:** this list is copy-pasted from `Frontend/src/lib/rules.ts`'s exported `SERVICE_TYPE_OPTIONS` rather than imported, because `rules.ts` is `/rules`-domain-named and importing a vehicle-type vocabulary from it into `/price`'s code would be a confusing cross-domain dependency for a value that isn't actually rules-specific (it's a vehicle-class vocabulary that both pages happen to share). This is a deliberate, small, disclosed duplication (8 string literals), not an oversight — do not "fix" it by importing from `rules.ts` without checking with the plan owner first, and do not extract a new shared module for 2 call sites (YAGNI).

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

**Step 2b: if you can reach a live backend (check `curl -s http://127.0.0.1:8081/healthz`), live-verify the create-then-edit-then-update sequence specifically** — this is the exact bug class this task's own code was already caught having during this plan's own writing (see the `isNew`/`local.id` reactivity comment in the code above): add a new row, fill valid fields, click Simpan (creates it), then edit a field on that SAME now-saved row and click Simpan again. Confirm the second save is a genuine `PUT` (not a second `POST` — check the network tab, or confirm no duplicate row / no spurious 409 appears). If you cannot reach a live backend, verify by careful code tracing instead and say so explicitly in your report — do not skip this check silently.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/PriceRow.svelte
git commit -m "feat(frontend): PriceRow.svelte — per-row price editor (expand-in-place, own Save/Delete)"
```

---

### Task 5: `/price/+page.svelte` — page assembly

No unit test — page assembly, verified via `svelte-check` + Task 6's e2e suite.

**Files:**
- Create: `Frontend/src/routes/(app)/price/+page.svelte`

**Interfaces:**
- Consumes: `fetchPrices` (Task 2); `fetchLocations`, `createLocation`, `LocationItem` (Fase 7d's `api-rules.ts` — reused as-is, the SAME shared location list `/rules` uses, not a separate one); `newPriceDraft`, `matchesFilter`, `PriceDraft` (Task 1); `PriceRow.svelte` (Task 4); `Pagination.svelte` (Fase 7c, unchanged); `data.user.is_main_account` (same `(app)/+layout.server.ts` pattern as `/rules`).

Session-gating is already handled by `(app)/+layout.server.ts` for every route under this group — this page needs no auth logic of its own beyond reading `is_main_account`.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/price/+page.svelte -->
<!-- /price: client-side filter + pagination over the full tenant price list. Each row persists
     independently (PriceRow's own Save/Delete) — no page-level dirty-tracking or batch Save,
     unlike /rules, since /price's backend is genuine per-resource REST. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchPrices } from '$lib/api-prices';
	import { fetchLocations, createLocation, type LocationItem } from '$lib/api-rules';
	import { newPriceDraft, matchesFilter, type PriceDraft } from '$lib/prices';
	import PriceRow from '$lib/components/PriceRow.svelte';
	import Pagination from '$lib/components/Pagination.svelte';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	const PAGE_SIZE = 20;

	let rows = $state<PriceDraft[]>([]);
	let locations = $state<LocationItem[]>([]);
	let loading = $state(true);
	let errorMsg = $state('');
	let query = $state('');
	let page = $state(1);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const [prices, locs] = await Promise.all([fetchPrices(), fetchLocations()]);
			rows = prices;
			locations = locs;
		} catch {
			errorMsg = 'Gagal memuat daftar harga. Coba lagi.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	const filtered = $derived(rows.filter((r) => matchesFilter(r, query)));
	const pageCount = $derived(Math.max(1, Math.ceil(filtered.length / PAGE_SIZE)));
	const pageRows = $derived(filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE));
	const hasMore = $derived(page < pageCount);

	function handleQueryChange(next: string) {
		query = next;
		page = 1;
	}

	function addDraftRow() {
		rows = [newPriceDraft(), ...rows];
		query = '';
		page = 1;
	}

	function handleSaved(saved: PriceDraft) {
		rows = rows.map((r) => (r.clientKey === saved.clientKey ? saved : r));
	}

	function handleRemove(clientKey: string) {
		rows = rows.filter((r) => r.clientKey !== clientKey);
	}

	async function handleCreateLocation(name: string): Promise<LocationItem> {
		const created = await createLocation(name);
		locations = [...locations, created];
		return created;
	}
</script>

<svelte:head>
	<title>Harga — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Daftar Harga</h1>

	{#if readOnly}
		<div
			role="alert"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
		>
			Hanya akun utama yang dapat mengubah harga.
		</div>
	{/if}

	{#if errorMsg}
		<div
			role="alert"
			aria-live="polite"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
		>
			{errorMsg}
		</div>
	{/if}

	{#if loading}
		<p class="text-[12px] text-text-muted">Memuat…</p>
	{:else}
		<div class="flex items-end gap-3">
			<label class="flex-1 flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Cari</span>
				<input
					type="text"
					value={query}
					oninput={(e) => handleQueryChange((e.target as HTMLInputElement).value)}
					placeholder="Kode rute, region, atau asal"
					class="min-h-[44px] px-3 rounded-md text-[13px] font-body bg-bg-surface border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			{#if !readOnly}
				<button
					type="button"
					onclick={addDraftRow}
					class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Tambah Harga
				</button>
			{/if}
		</div>

		<div class="flex flex-col gap-2">
			{#each pageRows as row (row.clientKey)}
				<PriceRow
					draft={row}
					{locations}
					onCreateLocation={handleCreateLocation}
					onSaved={handleSaved}
					onRemove={() => handleRemove(row.clientKey)}
					{readOnly}
				/>
			{/each}
		</div>

		<Pagination {page} {hasMore} onPageChange={(next) => (page = next)} />
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/price/+page.svelte"
git commit -m "feat(frontend): /price page assembly — client-side filter+pagination, per-row CRUD, permission gating"
```

---

### Task 6: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/price.spec.ts`

**Interfaces:**
- Consumes: the full `/price` page built in Tasks 1-5. No new frontend code — this task authors real-stack e2e coverage and runs the full verification suite.

**No new seed users needed.** This task reuses `e2e-test-user` (main-account, `tower-dev` tenant) and `e2e-readonly-user` (non-main-account) — both already seeded by Fase 7a/7d, no re-seeding required. **No `route_prices` rows are pre-seeded either** — like `/rules`, this page's whole purpose is creating rows through the UI, so every test creates its own fixture via the real Save flow rather than hand-crafting an INSERT.

**Playwright auto-dismisses `window.confirm()` by default.** `PriceRow.svelte`'s delete flow calls `confirm(...)` before deleting — a test that clicks "Hapus" without first registering `page.on('dialog', (dialog) => dialog.accept())` will see the confirm auto-dismissed (Playwright's default), the delete silently no-op via the `if (!confirm(...)) return;` early return, and a confusing "still visible after reload" assertion failure with no obvious cause. Register the dialog handler BEFORE triggering the delete click in every test that deletes a row.

- [ ] **Step 1: Write `Frontend/tests/price.spec.ts`**

```typescript
// Frontend/tests/price.spec.ts
//
// REAL end-to-end proof of Fase 7e's /price route price list. Same real-stack setup as
// tests/login.spec.ts, tests/command.spec.ts, tests/tickets.spec.ts, tests/rules.spec.ts — real
// reactor-core on :8081 behind Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432),
// real Redis (tower-redis, 127.0.0.1:16379). Nothing here is mocked or stubbed.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d, no re-seeding needed. No route_prices rows are pre-seeded either — every
// test creates its own fixture via the real Save flow, same precedent as rules.spec.ts.
//
// IMPORTANT: PriceRow.svelte's delete flow calls window.confirm() before deleting. Playwright
// auto-DISMISSES confirm() by default (returns false) unless a page.on('dialog', ...) handler is
// registered to accept it first — every test below that deletes a row registers one BEFORE the
// click that triggers the dialog.

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /price redirects to /login', async ({ page }) => {
	await page.goto('/price');
	await expect(page).toHaveURL(/\/login/);
});

test('main account can create a price with a new inline location, and it persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');
	await expect(page.getByRole('heading', { name: 'Daftar Harga' })).toBeVisible();

	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').fill('E2E-CREATE-01');
	await page.getByLabel('Region').fill('Jawa');

	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Jakarta DC');
	await originInput.press('Enter');
	await expect(page.getByText('E2E Jakarta DC', { exact: true })).toBeVisible();

	const destInput = page.getByLabel('Tujuan (maks 5)');
	await destInput.fill('E2E Bandung DC');
	await destInput.press('Enter');
	await expect(page.getByText('E2E Bandung DC', { exact: true })).toBeVisible();

	await page.getByRole('radio', { name: 'TRONTON' }).click();
	await page.getByLabel('Harga (Rp)').fill('150000');

	await page.getByRole('button', { name: 'Simpan' }).click();
	// After a successful save, the "Batal" button (new-row-only) disappears — a reliable signal
	// the row transitioned from new/unsaved to saved, distinct from just "no error shown."
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E-CREATE-01')).toBeVisible({ timeout: 10_000 });
});

test('editing an existing price persists after save and reload', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	// Create a throwaway fixture for this test, independent of the previous test's row.
	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').fill('E2E-EDIT-01');
	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Surabaya DC');
	await originInput.press('Enter');
	const destInput = page.getByLabel('Tujuan (maks 5)');
	await destInput.fill('E2E Malang DC');
	await destInput.press('Enter');
	await page.getByRole('radio', { name: 'FUSO' }).click();
	await page.getByLabel('Harga (Rp)').fill('200000');
	await page.getByRole('button', { name: 'Simpan' }).click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await page.getByText('E2E-EDIT-01').click();
	await page.getByLabel('Region').fill('Jawa Timur');
	await page.getByRole('button', { name: 'Simpan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan' })).toBeEnabled({ timeout: 10_000 });

	await page.reload();
	await page.getByText('E2E-EDIT-01').click();
	await expect(page.getByLabel('Region')).toHaveValue('Jawa Timur');
});

test('deleting a price removes it after reload (confirm dialog accepted)', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').fill('E2E-DELETE-01');
	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Medan DC');
	await originInput.press('Enter');
	const destInput = page.getByLabel('Tujuan (maks 5)');
	await destInput.fill('E2E Pekanbaru DC');
	await destInput.press('Enter');
	await page.getByRole('radio', { name: 'CDD LONG' }).click();
	await page.getByLabel('Harga (Rp)').fill('300000');
	await page.getByRole('button', { name: 'Simpan' }).click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E-DELETE-01')).toBeVisible({ timeout: 10_000 });

	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus harga rute E2E-DELETE-01' }).click();
	await expect(page.getByText('E2E-DELETE-01')).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E-DELETE-01')).toBeHidden({ timeout: 10_000 });
});

test('duplicate route_code on create shows the specific 409 message', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	// First row with a fixed route_code.
	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').first().fill('E2E-DUP-01');
	const originInput1 = page.getByLabel('Asal').first();
	await originInput1.fill('E2E Semarang DC');
	await originInput1.press('Enter');
	const destInput1 = page.getByLabel('Tujuan (maks 5)').first();
	await destInput1.fill('E2E Solo DC');
	await destInput1.press('Enter');
	await page.getByRole('radio', { name: 'ENGKEL' }).first().click();
	await page.getByLabel('Harga (Rp)').first().fill('100000');
	await page.getByRole('button', { name: 'Simpan' }).first().click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	// Second row, SAME route_code — expect a 409 with the specific message, row stays unsaved.
	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').first().fill('E2E-DUP-01');
	const originInput2 = page.getByLabel('Asal').first();
	await originInput2.fill('E2E Yogyakarta DC');
	await originInput2.press('Enter');
	const destInput2 = page.getByLabel('Tujuan (maks 5)').first();
	await destInput2.fill('E2E Magelang DC');
	await destInput2.press('Enter');
	await page.getByRole('radio', { name: 'WINGBOX' }).first().click();
	await page.getByLabel('Harga (Rp)').first().fill('120000');
	await page.getByRole('button', { name: 'Simpan' }).first().click();
	await expect(page.getByText('Kode rute sudah dipakai.')).toBeVisible({ timeout: 10_000 });
	// Still unsaved — "Batal" is still present on this second, still-new row.
	await expect(page.getByRole('button', { name: 'Batal' })).toBeVisible();
});

test('search filters the visible list client-side', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').first().fill('E2E-FILTER-UNIQUE-01');
	const originInput = page.getByLabel('Asal').first();
	await originInput.fill('E2E Palembang DC');
	await originInput.press('Enter');
	const destInput = page.getByLabel('Tujuan (maks 5)').first();
	await destInput.fill('E2E Jambi DC');
	await destInput.press('Enter');
	await page.getByRole('radio', { name: 'BLINDVAN' }).first().click();
	await page.getByLabel('Harga (Rp)').first().fill('90000');
	await page.getByRole('button', { name: 'Simpan' }).first().click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.getByLabel('Cari').fill('zzz-no-such-route-zzz');
	await expect(page.getByText('E2E-FILTER-UNIQUE-01')).toBeHidden();

	await page.getByLabel('Cari').fill('E2E-FILTER-UNIQUE-01');
	await expect(page.getByText('E2E-FILTER-UNIQUE-01')).toBeVisible();
});

test('non-main-account session sees a read-only view with no edit controls', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/price');
	await expect(page.getByText('Hanya akun utama yang dapat mengubah harga.')).toBeVisible();
	await expect(page.getByRole('button', { name: '+ Tambah Harga' })).toBeHidden();
});
```

- [ ] **Step 2: Run the new e2e file alone**

Run: `cd Frontend && pnpm exec playwright test tests/price.spec.ts`
Expected: all tests pass (a live `reactor-core` + `tower-postgres` + `tower-redis` stack must already be running — see `tests/login.spec.ts`'s header comment for the exact env a manually-started `reactor-core` needs).

- [ ] **Step 3: Run the full Playwright suite (regression check)**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across `login.spec.ts`, `command.spec.ts`, `tickets.spec.ts`, `rules.spec.ts`, `price.spec.ts` pass — no regression in earlier phases' coverage. In particular, re-confirm `rules.spec.ts` still passes given Task 3 modified the shared `ChipInput.svelte` component that `RuleRow.svelte` depends on.

- [ ] **Step 4: Full backend verification**

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green. **Use the `tower` superuser URL for `cargo test`, not `app_role`** (the same local-dev-only gotcha every prior phase's plan has flagged — `app_role` lacks `CREATE` on the `public` schema, breaking migrations during tests). This task makes no backend changes, so this step is a pure regression check.

- [ ] **Step 5: Full frontend verification**

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `prices.test.ts`, Task 2's `api-prices.test.ts`, plus every pre-existing suite — no regression, especially not in anything touching `ChipInput.svelte`); production build succeeds.

- [ ] **Step 6: Commit**

```bash
git add Frontend/tests/price.spec.ts
git commit -m "test(fase-7e): /price e2e (Playwright) — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — full CRUD with per-row persistence (Tasks 4, 5), `LocationCombobox`/`ChipInput` reuse including the new single-select mode (Tasks 3, 4), read-only gating (Tasks 4, 5), client-side filter+pagination (Task 5). Every "Out of scope" bullet (backend pagination, a public-facing page, bulk import/export) has no corresponding task.

**Placeholder scan:** no TBD/TODO. While writing Task 5, this review caught a real bug in Task 4's own draft code: `isNew` was computed once from the `draft` prop instead of reactively from `local.id`, meaning a row that had just been created would still think it was unsaved on its next edit — calling `createPrice` a second time instead of `updatePrice` (duplicate row / spurious 409). Fixed inline in Task 4 (made `isNew` a `$derived` off `local.id`, and `save()` now adopts the server-confirmed row into `local` after success) before finalizing this plan, with an explicit live-verification step added to Task 4 itself so the implementer re-confirms the fix holds.

**Type consistency:** `PriceDraft` (Task 1) is the same shape threaded unchanged through Task 2's wire mapping, Task 4's `PriceRow` props/local state, and Task 5's page state — no renamed fields between tasks (re-checked against each task's interface list while writing this plan).

**Cross-task dependency ordering:** 1 (types) → 2 (wire mapping, depends on 1) → 3 (ChipInput extension, depends on nothing new — a modification to an existing Fase 7d component) → 4 (PriceRow, depends on 1, 2, 3, and Fase 7d's `LocationCombobox`) → 5 (page, depends on 1, 2, 4) → 6 (e2e, depends on everything, plus a regression check on Task 3's shared-component change against `/rules`). No task references a later task's output.

