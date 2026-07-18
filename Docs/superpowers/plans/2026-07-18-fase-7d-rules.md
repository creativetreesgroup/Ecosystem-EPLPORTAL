# Fase 7d: `/rules` (Rule Builder) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `/rules`, a full CRUD editor for TOWER's auto-accept rule engine (all 3 modes: `booking_id`/`route`/`filter`) plus the `auto_accept_enabled` kill switch with its OTP arming flow — a pure frontend build against already-complete backend surfaces.

**Architecture:** Local-edit + single-Save SvelteKit page. On mount, `GET /bookings/settings` + `GET /locations` seed local `$state`; every add/edit/delete/reorder mutates local state only; one "Simpan Perubahan" PUTs the whole set and replaces local state with the response (never a merge). Inline-expanding rule rows (no drawer/dialog). Read-only for non-main-account sessions (server-enforced; client mirrors it for UX).

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), `@lucide/svelte`, Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-18-fase-7d-rules-design.md` — read it first for the full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format is snake_case** — no `#[serde(rename_all)]` anywhere in `api-gateway` (verified by grep). WS payloads are the opposite (camelCase) but this page makes no WS calls.
- **Client-supplied rule `id`s never round-trip on save** — `PUT /bookings/settings` always deletes-and-reinserts. Track rules locally by an ephemeral `clientKey` (`crypto.randomUUID()`), never by the server `id`. After a successful save, replace local state wholesale with the response's `rules` (it is the true post-sanitize-post-dedupe state).
- **`GET /bookings/settings` is ungated** (any authenticated user); `PUT` requires `Permission::ManageRules`; arming (`auto_accept_enabled: false→true`) additionally requires `Permission::ArmAutoAccept` + a valid 120s-TTL OTP proof. Both permissions are main-account-only today.
- **Known field vocabularies** (not derivable from schema, confirmed with the user):
  - `service_types`: exactly `TRONTON`, `FUSO`, `CDD LONG`, `CDE LONG`, `BLINDVAN`, `WINGBOX`, `ENGKEL`, `40FCL`.
  - `shift_types`: `1`=Pagi, `2`=Siang, `3`=Malam.
  - `trip_types`: `1`=Berangkat, `2`=Pulang.
  - `booking_type`: `all` / `spxid` / `reguler`.
  - `match_mode` (route mode only): `strict` / `flexible`.
- **`destinations` capped at 5**, order matters (do not sort). **`coc_only`/`non_coc_only` are mutually exclusive** in the UI itself (server silently resolves conflicts by turning `non_coc_only` off, but the UI should never let a user create that conflict in the first place). **`accepted_count` is read-only** (server-maintained counter, round-tripped, never user-edited). **`max_accept_count: 0` means unlimited** — label it explicitly, never leave it looking like "zero allowed."
- **No drag-to-reorder rules** — `priority` is a plain numeric field (`-999..999`); the real ranking is mode-first, then priority, then specificity, so list position must never imply match priority.
- **Accessibility bar (established 7a-7c convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block (`bg-bg-base`, `bg-bg-surface`, `border-border`, `text-text-primary`, `text-text-muted`, `bg-accent`/`text-bg-base`, `bg-live`, `bg-danger`/`text-danger`, `font-heading`/`font-mono`/`font-body`, `rounded-md`/`rounded-lg` — no raw hex, no `rounded-sm` which doesn't exist), `min-h-[44px]`/`min-w-[44px]` tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error/warning banners, glyph+text (never color-only) status, every interaction keyboard-operable with no drag-only affordance.
- **OTP flow constants:** code TTL 180s, resend cooldown 60s, max 5 verify attempts per code, `pwverify` proof TTL 120s (the window `PUT` must land inside once armed).
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `rules.ts` — pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/rules.ts`
- Test: `Frontend/src/lib/rules.test.ts`

**Interfaces:**
- Produces (consumed by Tasks 2, 3, 4, 5, 6, 7): `RuleMode`, `BookingType`, `MatchMode`, `RuleConditions`, `RuleDraft` types; `SERVICE_TYPE_OPTIONS: string[]`; `SHIFT_TYPE_OPTIONS`/`TRIP_TYPE_OPTIONS: { value: number; label: string }[]`; `newRuleDraft(mode: RuleMode): RuleDraft`; `conditionSummary(rule: RuleDraft): string`; `ruleIsEmpty(rule: RuleDraft): boolean`; `setCocOnly(c: RuleConditions, value: boolean): RuleConditions`; `setNonCocOnly(c: RuleConditions, value: boolean): RuleConditions`; `isDirty(current: RulesPageState, lastSaved: RulesPageState): boolean`; `RulesPageState` type.

- [ ] **Step 1: Write the failing test — types, vocabularies, `newRuleDraft`**

```typescript
// Frontend/src/lib/rules.test.ts
import { describe, it, expect } from 'vitest';
import {
	newRuleDraft,
	conditionSummary,
	ruleIsEmpty,
	setCocOnly,
	setNonCocOnly,
	isDirty,
	SERVICE_TYPE_OPTIONS,
	SHIFT_TYPE_OPTIONS,
	TRIP_TYPE_OPTIONS,
	type RuleDraft,
	type RulesPageState
} from './rules';

describe('vocabularies', () => {
	it('exposes the 8 canonical service types', () => {
		expect(SERVICE_TYPE_OPTIONS).toEqual([
			'TRONTON',
			'FUSO',
			'CDD LONG',
			'CDE LONG',
			'BLINDVAN',
			'WINGBOX',
			'ENGKEL',
			'40FCL'
		]);
	});

	it('exposes shift types 1=Pagi, 2=Siang, 3=Malam', () => {
		expect(SHIFT_TYPE_OPTIONS).toEqual([
			{ value: 1, label: 'Pagi' },
			{ value: 2, label: 'Siang' },
			{ value: 3, label: 'Malam' }
		]);
	});

	it('exposes trip types 1=Berangkat, 2=Pulang', () => {
		expect(TRIP_TYPE_OPTIONS).toEqual([
			{ value: 1, label: 'Berangkat' },
			{ value: 2, label: 'Pulang' }
		]);
	});
});

describe('newRuleDraft', () => {
	it('creates an empty rule with the given mode, a fresh clientKey, and no server id', () => {
		const rule = newRuleDraft('route');
		expect(rule.mode).toBe('route');
		expect(rule.id).toBeNull();
		expect(rule.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(rule.enabled).toBe(true);
		expect(rule.priority).toBe(0);
		expect(rule.conditions.destinations).toEqual([]);
		expect(rule.conditions.maxAcceptCount).toBe(0);
		expect(rule.conditions.acceptedCount).toBe(0);
	});

	it('two calls produce different clientKeys', () => {
		expect(newRuleDraft('filter').clientKey).not.toBe(newRuleDraft('filter').clientKey);
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/rules.test.ts`
Expected: FAIL — `Cannot find module './rules'` (file does not exist yet).

- [ ] **Step 3: Write the types, vocabularies, and `newRuleDraft`**

```typescript
// Frontend/src/lib/rules.ts
// Pure logic for the /rules Rule Builder — no fetch, no DOM. Wire-format mapping (snake_case
// <-> these camelCase types) lives in api-rules.ts, matching the tickets.ts/api-tickets.ts split
// already established in Fase 7c.

export type RuleMode = 'booking_id' | 'route' | 'filter';
export type BookingType = 'all' | 'spxid' | 'reguler';
export type MatchMode = 'strict' | 'flexible';

export type RuleConditions = {
	serviceTypes: string[];
	maxWeight: number | null;
	cocOnly: boolean;
	nonCocOnly: boolean;
	maxCodAmount: number | null;
	bookingIds: string[];
	origin: string;
	destinations: string[];
	bookingType: BookingType;
	shiftTypes: number[];
	tripTypes: number[];
	matchMode: MatchMode;
	minDeadlineMin: number | null;
	maxAcceptCount: number;
	/** Server-maintained running counter — read-only in the UI, round-tripped on save. */
	acceptedCount: number;
};

export type RuleDraft = {
	/** Ephemeral, client-generated — for Svelte {#each} keying only. NEVER sent to the server and
	 * NEVER equal to `id` (see Global Constraints: server ids don't round-trip on save). */
	clientKey: string;
	/** The server's Uuid for this rule, or null if it has never been saved. Present only for
	 * traceability/debugging — no code path may key off this for list identity; use clientKey. */
	id: string | null;
	name: string;
	enabled: boolean;
	priority: number;
	mode: RuleMode;
	conditions: RuleConditions;
};

export type RulesPageState = {
	autoAcceptEnabled: boolean;
	rules: RuleDraft[];
};

export const SERVICE_TYPE_OPTIONS = [
	'TRONTON',
	'FUSO',
	'CDD LONG',
	'CDE LONG',
	'BLINDVAN',
	'WINGBOX',
	'ENGKEL',
	'40FCL'
] as const;

export const SHIFT_TYPE_OPTIONS: { value: number; label: string }[] = [
	{ value: 1, label: 'Pagi' },
	{ value: 2, label: 'Siang' },
	{ value: 3, label: 'Malam' }
];

export const TRIP_TYPE_OPTIONS: { value: number; label: string }[] = [
	{ value: 1, label: 'Berangkat' },
	{ value: 2, label: 'Pulang' }
];

function emptyConditions(): RuleConditions {
	return {
		serviceTypes: [],
		maxWeight: null,
		cocOnly: false,
		nonCocOnly: false,
		maxCodAmount: null,
		bookingIds: [],
		origin: '',
		destinations: [],
		bookingType: 'all',
		shiftTypes: [],
		tripTypes: [],
		matchMode: 'strict',
		minDeadlineMin: null,
		maxAcceptCount: 0,
		acceptedCount: 0
	};
}

export function newRuleDraft(mode: RuleMode): RuleDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: null,
		name: '',
		enabled: true,
		priority: 0,
		mode,
		conditions: emptyConditions()
	};
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/rules.test.ts`
Expected: PASS (5/5 so far).

- [ ] **Step 5: Write the failing test — `conditionSummary` and `ruleIsEmpty`**

```typescript
// Append to Frontend/src/lib/rules.test.ts

describe('conditionSummary', () => {
	it('booking_id mode: counts the ids', () => {
		const rule = { ...newRuleDraft('booking_id'), conditions: { ...emptyConditionsForTest(), bookingIds: ['A', 'B', 'C'] } };
		expect(conditionSummary(rule)).toBe('3 Booking ID');
	});

	it('booking_id mode: empty list', () => {
		const rule = newRuleDraft('booking_id');
		expect(conditionSummary(rule)).toBe('Belum ada Booking ID');
	});

	it('route mode: origin, destinations, and service types', () => {
		const rule = {
			...newRuleDraft('route'),
			conditions: {
				...emptyConditionsForTest(),
				origin: 'Padang DC',
				destinations: ['Cileungsi DC', 'Bandung DC'],
				serviceTypes: ['TRONTON', 'FUSO']
			}
		};
		expect(conditionSummary(rule)).toBe('Padang DC → Cileungsi DC → Bandung DC · TRONTON, FUSO');
	});

	it('route mode: missing origin/destinations shows em-dash placeholders', () => {
		const rule = newRuleDraft('route');
		expect(conditionSummary(rule)).toBe('— → —');
	});

	it('filter mode: lists active filters', () => {
		const rule = {
			...newRuleDraft('filter'),
			conditions: { ...emptyConditionsForTest(), serviceTypes: ['TRONTON'], maxWeight: 500, cocOnly: true }
		};
		expect(conditionSummary(rule)).toBe('TRONTON · maks 500kg · COC saja');
	});

	it('filter mode: no active filters', () => {
		const rule = newRuleDraft('filter');
		expect(conditionSummary(rule)).toBe('Tanpa filter');
	});
});

describe('ruleIsEmpty', () => {
	it('booking_id mode with no ids is empty (mirrors the backend warning condition)', () => {
		expect(ruleIsEmpty(newRuleDraft('booking_id'))).toBe(true);
	});

	it('route mode with no origin and no destinations is empty', () => {
		expect(ruleIsEmpty(newRuleDraft('route'))).toBe(true);
	});

	it('route mode with only an origin is not empty', () => {
		const rule = { ...newRuleDraft('route'), conditions: { ...emptyConditionsForTest(), origin: 'Padang DC' } };
		expect(ruleIsEmpty(rule)).toBe(false);
	});

	it('filter mode is never considered empty by this check (no origin/id concept applies)', () => {
		expect(ruleIsEmpty(newRuleDraft('filter'))).toBe(false);
	});
});

// Local test helper — mirrors rules.ts's private emptyConditions() so fixtures above can spread
// a full, valid RuleConditions without repeating every field at every call site.
function emptyConditionsForTest() {
	return {
		serviceTypes: [],
		maxWeight: null,
		cocOnly: false,
		nonCocOnly: false,
		maxCodAmount: null,
		bookingIds: [],
		origin: '',
		destinations: [],
		bookingType: 'all' as const,
		shiftTypes: [],
		tripTypes: [],
		matchMode: 'strict' as const,
		minDeadlineMin: null,
		maxAcceptCount: 0,
		acceptedCount: 0
	};
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/rules.test.ts`
Expected: FAIL — `conditionSummary`/`ruleIsEmpty` are not exported.

- [ ] **Step 7: Implement `conditionSummary` and `ruleIsEmpty`**

```typescript
// Append to Frontend/src/lib/rules.ts

export function conditionSummary(rule: RuleDraft): string {
	const c = rule.conditions;
	if (rule.mode === 'booking_id') {
		return c.bookingIds.length > 0 ? `${c.bookingIds.length} Booking ID` : 'Belum ada Booking ID';
	}
	if (rule.mode === 'route') {
		const route = [c.origin || '—', ...(c.destinations.length > 0 ? c.destinations : ['—'])].join(' → ');
		return c.serviceTypes.length > 0 ? `${route} · ${c.serviceTypes.join(', ')}` : route;
	}
	// filter mode
	const parts: string[] = [];
	if (c.serviceTypes.length > 0) parts.push(c.serviceTypes.join(', '));
	if (c.maxWeight !== null) parts.push(`maks ${c.maxWeight}kg`);
	if (c.maxCodAmount !== null) parts.push(`maks COD Rp${c.maxCodAmount}`);
	if (c.cocOnly) parts.push('COC saja');
	if (c.nonCocOnly) parts.push('Non-COC saja');
	return parts.length > 0 ? parts.join(' · ') : 'Tanpa filter';
}

/** Mirrors core_domain::sanitize_accept_rules's two "kosong" (empty) warning conditions
 * (Backend/crates/core-domain/src/rule.rs) — a client-side early hint, not a save-blocker;
 * the server remains the source of truth and still warns/adjusts on save regardless. */
export function ruleIsEmpty(rule: RuleDraft): boolean {
	if (rule.mode === 'booking_id') return rule.conditions.bookingIds.length === 0;
	if (rule.mode === 'route') return rule.conditions.origin === '' && rule.conditions.destinations.length === 0;
	return false;
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/rules.test.ts`
Expected: PASS (all tests so far green).

- [ ] **Step 9: Write the failing test — `setCocOnly`/`setNonCocOnly` mutual exclusion and `isDirty`**

```typescript
// Append to Frontend/src/lib/rules.test.ts

describe('setCocOnly / setNonCocOnly mutual exclusion', () => {
	it('setCocOnly(true) turns nonCocOnly off', () => {
		const c = { ...emptyConditionsForTest(), nonCocOnly: true };
		const result = setCocOnly(c, true);
		expect(result.cocOnly).toBe(true);
		expect(result.nonCocOnly).toBe(false);
	});

	it('setNonCocOnly(true) turns cocOnly off', () => {
		const c = { ...emptyConditionsForTest(), cocOnly: true };
		const result = setNonCocOnly(c, true);
		expect(result.nonCocOnly).toBe(true);
		expect(result.cocOnly).toBe(false);
	});

	it('setCocOnly(false) does not touch nonCocOnly', () => {
		const c = { ...emptyConditionsForTest(), nonCocOnly: true, cocOnly: true };
		const result = setCocOnly(c, false);
		expect(result.cocOnly).toBe(false);
		expect(result.nonCocOnly).toBe(true);
	});

	it('returns a new object, does not mutate the input', () => {
		const c = emptyConditionsForTest();
		const result = setCocOnly(c, true);
		expect(result).not.toBe(c);
		expect(c.cocOnly).toBe(false);
	});
});

describe('isDirty', () => {
	function state(overrides: Partial<RulesPageState> = {}): RulesPageState {
		return { autoAcceptEnabled: false, rules: [], ...overrides };
	}

	it('false when current equals lastSaved (ignoring clientKey)', () => {
		const rule = newRuleDraft('filter');
		const saved = { ...rule, clientKey: 'different-key-but-same-content' };
		expect(isDirty(state({ rules: [rule] }), state({ rules: [saved] }))).toBe(false);
	});

	it('true when autoAcceptEnabled differs', () => {
		expect(isDirty(state({ autoAcceptEnabled: true }), state({ autoAcceptEnabled: false }))).toBe(true);
	});

	it('true when a rule field differs', () => {
		const a = newRuleDraft('filter');
		const b = { ...a, priority: 5 };
		expect(isDirty(state({ rules: [a] }), state({ rules: [b] }))).toBe(true);
	});

	it('true when rule count differs', () => {
		const a = newRuleDraft('filter');
		expect(isDirty(state({ rules: [a, newRuleDraft('route')] }), state({ rules: [a] }))).toBe(true);
	});

	it('false for two independently-created empty states', () => {
		expect(isDirty(state(), state())).toBe(false);
	});
});
```

- [ ] **Step 10: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/rules.test.ts`
Expected: FAIL — `setCocOnly`/`setNonCocOnly`/`isDirty` are not exported.

- [ ] **Step 11: Implement `setCocOnly`, `setNonCocOnly`, `isDirty`**

```typescript
// Append to Frontend/src/lib/rules.ts

export function setCocOnly(c: RuleConditions, value: boolean): RuleConditions {
	return { ...c, cocOnly: value, nonCocOnly: value ? false : c.nonCocOnly };
}

export function setNonCocOnly(c: RuleConditions, value: boolean): RuleConditions {
	return { ...c, nonCocOnly: value, cocOnly: value ? false : c.cocOnly };
}

/** Deep-equal comparison ignoring `clientKey` (ephemeral, regenerated every load — must never
 * cause a false "dirty" reading) but INCLUDING `id` (a rule going from saved `id: "..."` to a
 * freshly-added `id: null` IS a real content difference). Rule order matters (an unsaved reorder
 * is a real edit). */
export function isDirty(current: RulesPageState, lastSaved: RulesPageState): boolean {
	if (current.autoAcceptEnabled !== lastSaved.autoAcceptEnabled) return true;
	if (current.rules.length !== lastSaved.rules.length) return true;
	return current.rules.some((rule, i) => {
		const other = lastSaved.rules[i];
		const { clientKey: _a, ...ruleRest } = rule;
		const { clientKey: _b, ...otherRest } = other;
		return JSON.stringify(ruleRest) !== JSON.stringify(otherRest);
	});
}
```

- [ ] **Step 12: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/rules.test.ts`
Expected: PASS — all tests in the file green (24 tests).

- [ ] **Step 13: Run svelte-check and commit**

Run: `cd Frontend && pnpm check && pnpm vitest run src/lib/rules.test.ts`
Expected: `0 ERRORS 0 WARNINGS`, all `rules.test.ts` tests passing.

```bash
git add Frontend/src/lib/rules.ts Frontend/src/lib/rules.test.ts
git commit -m "feat(frontend): rules.ts — pure Rule Builder logic (types, vocabularies, summaries, dirty-check)"
```

---

### Task 2: `api-rules.ts` — typed REST helpers

**Files:**
- Create: `Frontend/src/lib/api-rules.ts`
- Test: `Frontend/src/lib/api-rules.test.ts` (wire-mapping round-trip tests only — no live network; see Step 1)

**Interfaces:**
- Consumes: `RuleDraft`, `RuleConditions`, `RulesPageState` (Task 1); `apiPost`, `ApiError` (`Frontend/src/lib/api.ts`, existing).
- Produces (consumed by Task 7): `fetchSettings(): Promise<RulesPageState>`; `saveSettings(state: RulesPageState): Promise<RulesPageState>`; `LocationItem` type (`{id: string; name: string}`); `fetchLocations(): Promise<LocationItem[]>`; `createLocation(name: string): Promise<LocationItem>`; `requestAaOtp(): Promise<void>`; `verifyAaOtp(code: string): Promise<void>`.

**Wire shapes** (all snake_case, verified directly against `Backend/crates/api-gateway/src/routes/rules.rs` and `locations.rs` and `otp.rs` — no `rename_all` anywhere in this crate):

```
GET/PUT /bookings/settings -> SettingsResponse { auto_accept_enabled: bool, rules: RuleOutput[], warnings?: string[] }
RuleOutput { id: uuid, name: string, enabled: bool, priority: i32, mode: string,
  service_types: string[], max_weight: f64|null, coc_only: bool, non_coc_only: bool,
  max_cod_amount: f64|null, booking_ids: string[], origin: string, destinations: string[],
  booking_type: string, shift_types: i32[], trip_types: i32[], match_mode: string,
  min_deadline_min: i32|null, max_accept_count: i32, accepted_count: i32 }
PUT body -> SettingsRequest { auto_accept_enabled: bool, rules: RuleInput[] }
RuleInput -- same fields as RuleOutput's conditions, MINUS `id` (never sent) --

GET /locations -> LocationItem[] { id: uuid, name: string }
POST /locations body { name: string } -> LocationItem

POST /auth/request-aa-otp (no body) -> { ok: bool }
POST /auth/verify-aa-otp body { code: string } -> { ok: bool }
```

- [ ] **Step 1: Write the failing test — wire mapping round-trips**

```typescript
// Frontend/src/lib/api-rules.test.ts
// No network here — these test the pure wire<->domain mapping functions in isolation by
// exporting them for testing. fetchSettings/saveSettings themselves are exercised for real by
// Frontend/tests/rules.spec.ts (Task 8) against a live backend; a mocked-fetch unit test of a
// thin wrapper would just re-assert its own mock, providing no real coverage (same reasoning
// TicketFilterBar's sibling api-tickets.ts module was NOT given its own mock-fetch unit tests).
import { describe, it, expect } from 'vitest';
import { ruleOutputToDraft, draftToRuleInput } from './api-rules';
import { newRuleDraft } from './rules';

describe('ruleOutputToDraft', () => {
	it('maps every snake_case field to its camelCase RuleDraft equivalent', () => {
		const wire = {
			id: 'server-uuid-1',
			name: 'Padang Lane',
			enabled: true,
			priority: 5,
			mode: 'route',
			service_types: ['TRONTON'],
			max_weight: 1000,
			coc_only: false,
			non_coc_only: true,
			max_cod_amount: null,
			booking_ids: [],
			origin: 'Padang DC',
			destinations: ['Cileungsi DC'],
			booking_type: 'reguler',
			shift_types: [1, 2],
			trip_types: [1],
			match_mode: 'flexible',
			min_deadline_min: 30,
			max_accept_count: 10,
			accepted_count: 3
		};
		const draft = ruleOutputToDraft(wire);
		expect(draft.id).toBe('server-uuid-1');
		expect(draft.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(draft.name).toBe('Padang Lane');
		expect(draft.mode).toBe('route');
		expect(draft.conditions.serviceTypes).toEqual(['TRONTON']);
		expect(draft.conditions.maxWeight).toBe(1000);
		expect(draft.conditions.cocOnly).toBe(false);
		expect(draft.conditions.nonCocOnly).toBe(true);
		expect(draft.conditions.origin).toBe('Padang DC');
		expect(draft.conditions.destinations).toEqual(['Cileungsi DC']);
		expect(draft.conditions.bookingType).toBe('reguler');
		expect(draft.conditions.shiftTypes).toEqual([1, 2]);
		expect(draft.conditions.tripTypes).toEqual([1]);
		expect(draft.conditions.matchMode).toBe('flexible');
		expect(draft.conditions.minDeadlineMin).toBe(30);
		expect(draft.conditions.maxAcceptCount).toBe(10);
		expect(draft.conditions.acceptedCount).toBe(3);
	});
});

describe('draftToRuleInput', () => {
	it('maps every camelCase RuleDraft field to its snake_case wire equivalent, omitting id', () => {
		const draft = {
			...newRuleDraft('booking_id'),
			name: 'IDs',
			priority: -3,
			conditions: {
				...newRuleDraft('booking_id').conditions,
				bookingIds: ['SPX1', 'SPX2'],
				maxAcceptCount: 0,
				acceptedCount: 7
			}
		};
		const wire = draftToRuleInput(draft);
		expect(wire).not.toHaveProperty('id');
		expect(wire.name).toBe('IDs');
		expect(wire.priority).toBe(-3);
		expect(wire.mode).toBe('booking_id');
		expect(wire.booking_ids).toEqual(['SPX1', 'SPX2']);
		expect(wire.max_accept_count).toBe(0);
		expect(wire.accepted_count).toBe(7);
	});

	it('round-trips through ruleOutputToDraft . draftToRuleInput back to the same wire shape (minus id)', () => {
		const wire = {
			id: 'x',
			name: 'RT',
			enabled: false,
			priority: 0,
			mode: 'filter',
			service_types: ['FUSO', '40FCL'],
			max_weight: null,
			coc_only: true,
			non_coc_only: false,
			max_cod_amount: 50000,
			booking_ids: [],
			origin: '',
			destinations: [],
			booking_type: 'all',
			shift_types: [],
			trip_types: [2],
			match_mode: 'strict',
			min_deadline_min: null,
			max_accept_count: 0,
			accepted_count: 0
		};
		const { id: _id, ...expected } = wire;
		expect(draftToRuleInput(ruleOutputToDraft(wire))).toEqual(expected);
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-rules.test.ts`
Expected: FAIL — `Cannot find module './api-rules'`.

- [ ] **Step 3: Implement `api-rules.ts`**

```typescript
// Frontend/src/lib/api-rules.ts
// Thin typed REST layer for /rules — no UI logic here. Wire shapes verified directly against
// Backend/crates/api-gateway/src/routes/rules.rs, locations.rs, otp.rs (all snake_case, no
// rename_all anywhere in api-gateway).
import { apiPost, ApiError } from './api';
import {
	newRuleDraft,
	type RuleDraft,
	type RuleConditions,
	type RulesPageState,
	type RuleMode,
	type BookingType,
	type MatchMode
} from './rules';

type RuleOutputWire = {
	id: string;
	name: string;
	enabled: boolean;
	priority: number;
	mode: string;
	service_types: string[];
	max_weight: number | null;
	coc_only: boolean;
	non_coc_only: boolean;
	max_cod_amount: number | null;
	booking_ids: string[];
	origin: string;
	destinations: string[];
	booking_type: string;
	shift_types: number[];
	trip_types: number[];
	match_mode: string;
	min_deadline_min: number | null;
	max_accept_count: number;
	accepted_count: number;
};

type SettingsResponseWire = {
	auto_accept_enabled: boolean;
	rules: RuleOutputWire[];
	warnings?: string[];
};

/** Exported for Task 8's e2e reference only indirectly — the real export surface is
 * fetchSettings/saveSettings below. Exported at module level (not `function` inside
 * fetchSettings) so api-rules.test.ts can unit-test the mapping without a network call. */
export function ruleOutputToDraft(wire: RuleOutputWire): RuleDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: wire.id,
		name: wire.name,
		enabled: wire.enabled,
		priority: wire.priority,
		mode: wire.mode as RuleMode,
		conditions: {
			serviceTypes: wire.service_types,
			maxWeight: wire.max_weight,
			cocOnly: wire.coc_only,
			nonCocOnly: wire.non_coc_only,
			maxCodAmount: wire.max_cod_amount,
			bookingIds: wire.booking_ids,
			origin: wire.origin,
			destinations: wire.destinations,
			bookingType: wire.booking_type as BookingType,
			shiftTypes: wire.shift_types,
			tripTypes: wire.trip_types,
			matchMode: wire.match_mode as MatchMode,
			minDeadlineMin: wire.min_deadline_min,
			maxAcceptCount: wire.max_accept_count,
			acceptedCount: wire.accepted_count
		}
	};
}

type RuleInputWire = Omit<RuleOutputWire, 'id'>;

export function draftToRuleInput(draft: RuleDraft): RuleInputWire {
	const c = draft.conditions;
	return {
		name: draft.name,
		enabled: draft.enabled,
		priority: draft.priority,
		mode: draft.mode,
		service_types: c.serviceTypes,
		max_weight: c.maxWeight,
		coc_only: c.cocOnly,
		non_coc_only: c.nonCocOnly,
		max_cod_amount: c.maxCodAmount,
		booking_ids: c.bookingIds,
		origin: c.origin,
		destinations: c.destinations,
		booking_type: c.bookingType,
		shift_types: c.shiftTypes,
		trip_types: c.tripTypes,
		match_mode: c.matchMode,
		min_deadline_min: c.minDeadlineMin,
		max_accept_count: c.maxAcceptCount,
		accepted_count: c.acceptedCount
	};
}

function fromSettingsWire(wire: SettingsResponseWire): RulesPageState & { warnings: string[] } {
	return {
		autoAcceptEnabled: wire.auto_accept_enabled,
		rules: wire.rules.map(ruleOutputToDraft),
		warnings: wire.warnings ?? []
	};
}

export async function fetchSettings(): Promise<RulesPageState & { warnings: string[] }> {
	const res = await fetch('/bookings/settings', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch rule settings');
	const wire: SettingsResponseWire = await res.json();
	return fromSettingsWire(wire);
}

/** `apiPost` (Frontend/src/lib/api.ts) hardcodes `method: 'POST'` — the backend route is
 * `PUT /bookings/settings` (Backend/crates/api-gateway/src/routes/rules.rs's `rules_router`:
 * `.route("/settings", get(get_settings).put(put_settings))`), so this cannot use `apiPost`; a
 * POST here would 405. Raw `fetch` with `method: 'PUT'`, same header/credentials/error shape as
 * `apiPost` otherwise. */
export async function saveSettings(state: RulesPageState): Promise<RulesPageState & { warnings: string[] }> {
	const res = await fetch('/bookings/settings', {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({
			auto_accept_enabled: state.autoAcceptEnabled,
			rules: state.rules.map(draftToRuleInput)
		})
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save rule settings');
	const wire: SettingsResponseWire = await res.json();
	return fromSettingsWire(wire);
}

export type LocationItem = { id: string; name: string };

export async function fetchLocations(): Promise<LocationItem[]> {
	const res = await fetch('/locations', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch locations');
	return res.json();
}

export async function createLocation(name: string): Promise<LocationItem> {
	return apiPost<LocationItem>('/locations', { name });
}

export async function requestAaOtp(): Promise<void> {
	await apiPost<{ ok: boolean }>('/auth/request-aa-otp', {});
}

export async function verifyAaOtp(code: string): Promise<void> {
	await apiPost<{ ok: boolean }>('/auth/verify-aa-otp', { code });
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-rules.test.ts`
Expected: PASS (3/3).

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

```bash
git add Frontend/src/lib/api-rules.ts Frontend/src/lib/api-rules.test.ts
git commit -m "feat(frontend): api-rules.ts — typed REST layer for /bookings/settings, /locations, OTP arm flow"
```

---

### Task 3: `ChipInput.svelte`

No unit test for this task — this codebase's established convention (`TicketFilterBar.svelte`, `Pagination.svelte`, `TicketsTable.svelte` — none have a `.test.ts`) is that Svelte components are verified via `svelte-check` + the Playwright e2e suite (Task 8), not component-level unit tests. Pure logic stays in `.ts` files (Task 1), which IS unit-tested.

**Files:**
- Create: `Frontend/src/lib/components/ChipInput.svelte`

**Interfaces:**
- Consumes: nothing from earlier tasks (pure UI primitive).
- Produces (consumed by Task 6): a component with props `{ label: string; value: string[]; onChange: (value: string[]) => void; options?: { value: string; label: string }[] }`. When `options` is provided: closed-vocabulary multi-select (click any option to toggle membership in `value`; the option's own text IS the chip, no free text accepted). When `options` is omitted: free-text entry (type + Enter adds a trimmed, non-empty, deduplicated chip; each chip has its own remove button).

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/ChipInput.svelte -->
<!-- Generic chip list: free-text entry (no `options` prop — e.g. booking_ids) or closed-vocabulary
     multi-select (`options` prop — e.g. service_types, shift_types, trip_types). Both render/remove
     chips identically; only whether arbitrary text is accepted differs, so one small prop covers
     both rather than two near-duplicate components. Numeric vocabularies (shift/trip types) are the
     caller's responsibility to map to/from string — this component only ever holds `string[]`. -->
<script lang="ts">
	import { X } from '@lucide/svelte';

	let {
		label,
		value,
		onChange,
		options
	}: {
		label: string;
		value: string[];
		onChange: (value: string[]) => void;
		options?: { value: string; label: string }[];
	} = $props();

	let draft = $state('');
	const inputId = `chip-input-${crypto.randomUUID()}`;

	function addFreeText() {
		const trimmed = draft.trim();
		if (trimmed !== '' && !value.includes(trimmed)) {
			onChange([...value, trimmed]);
		}
		draft = '';
	}

	function remove(item: string) {
		onChange(value.filter((v) => v !== item));
	}

	function toggleOption(optValue: string) {
		if (value.includes(optValue)) {
			onChange(value.filter((v) => v !== optValue));
		} else {
			onChange([...value, optValue]);
		}
	}
</script>

<div class="flex flex-col gap-1.5">
	<span id={`${inputId}-label`} class="text-[10px] font-body text-text-muted uppercase tracking-wide">{label}</span>

	{#if options}
		<div class="flex flex-wrap gap-1.5" role="group" aria-labelledby={`${inputId}-label`}>
			{#each options as opt (opt.value)}
				<button
					type="button"
					aria-pressed={value.includes(opt.value)}
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
		<div class="flex flex-wrap items-center gap-1.5">
			{#each value as item (item)}
				<span
					class="flex items-center gap-1 min-h-[32px] pl-2.5 pr-1.5 rounded-md text-[12px] font-mono bg-bg-base border border-border text-text-primary"
				>
					{item}
					<button
						type="button"
						onclick={() => remove(item)}
						aria-label={`Hapus ${item}`}
						class="min-h-[24px] min-w-[24px] flex items-center justify-center rounded text-text-muted hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						<X size={12} aria-hidden="true" />
					</button>
				</span>
			{/each}
			<input
				id={inputId}
				type="text"
				bind:value={draft}
				aria-labelledby={`${inputId}-label`}
				onkeydown={(e) => {
					if (e.key === 'Enter') {
						e.preventDefault();
						addFreeText();
					}
				}}
				placeholder="Ketik lalu Enter"
				class="min-h-[36px] px-2.5 rounded-md text-[12px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</div>
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/ChipInput.svelte
git commit -m "feat(frontend): ChipInput.svelte — free-text and closed-vocabulary chip list"
```

---

### Task 4: `LocationCombobox.svelte`

No unit test — same rationale as Task 3 (UI-only, verified via `svelte-check` + Task 8's e2e suite).

**Files:**
- Create: `Frontend/src/lib/components/LocationCombobox.svelte`

**Interfaces:**
- Consumes: `LocationItem` type (Task 2, `api-rules.ts`).
- Produces (consumed by Task 6): a component with props `{ label: string; locations: LocationItem[]; value: string[]; onChange: (value: string[]) => void; onCreateLocation: (name: string) => Promise<LocationItem>; multi?: boolean; max?: number }`. `multi=false` (default): single-select — the input hides once one value is set, cleared via the selected chip's remove button. `multi=true`: multi-select capped at `max`, with explicit up/down reorder buttons per chip. `locations` is owned by the page (Task 7) and passed down — this component never fetches; `onCreateLocation` bubbles a new-location request up so the page can add it to its own shared `locations` list once (so every open `RuleRow` sees the new location immediately, not just the one that created it).

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/LocationCombobox.svelte -->
<!-- Separate from ChipInput: this needs remote-list search (native <input list> + <datalist> —
     no hand-rolled ARIA listbox needed, ponytail: native platform feature covers it), inline
     "create new location" when the typed name matches nothing, single/multi selection, and
     (multi only) capped count + explicit reorder. Materially different interaction from a plain
     chip list, so it is its own component rather than a ChipInput variant. -->
<script lang="ts">
	import { X, ChevronUp, ChevronDown, Plus } from '@lucide/svelte';
	import type { LocationItem } from '$lib/api-rules';

	let {
		label,
		locations,
		value,
		onChange,
		onCreateLocation,
		multi = false,
		max
	}: {
		label: string;
		locations: LocationItem[];
		value: string[];
		onChange: (value: string[]) => void;
		onCreateLocation: (name: string) => Promise<LocationItem>;
		multi?: boolean;
		max?: number;
	} = $props();

	let draft = $state('');
	let creating = $state(false);
	let errorMsg = $state('');
	const listId = `location-list-${crypto.randomUUID()}`;

	async function commit() {
		const trimmed = draft.trim();
		if (trimmed === '') return;
		if (!multi && value.includes(trimmed)) {
			draft = '';
			return;
		}
		if (multi && (value.includes(trimmed) || (max !== undefined && value.length >= max))) {
			draft = '';
			return;
		}

		const existing = locations.find((l) => l.name.toLowerCase() === trimmed.toLowerCase());
		if (existing) {
			onChange(multi ? [...value, existing.name] : [existing.name]);
			draft = '';
			errorMsg = '';
			return;
		}

		creating = true;
		errorMsg = '';
		try {
			const created = await onCreateLocation(trimmed);
			onChange(multi ? [...value, created.name] : [created.name]);
			draft = '';
		} catch {
			errorMsg = `Gagal menambah lokasi "${trimmed}".`;
		} finally {
			creating = false;
		}
	}

	function remove(name: string) {
		onChange(value.filter((v) => v !== name));
	}

	function moveUp(index: number) {
		if (index === 0) return;
		const next = [...value];
		[next[index - 1], next[index]] = [next[index], next[index - 1]];
		onChange(next);
	}

	function moveDown(index: number) {
		if (index === value.length - 1) return;
		const next = [...value];
		[next[index], next[index + 1]] = [next[index + 1], next[index]];
		onChange(next);
	}

	const atMax = $derived(multi && max !== undefined && value.length >= max);
	const showInput = $derived(multi ? !atMax : value.length === 0);
</script>

<div class="flex flex-col gap-1.5">
	<span id={`${listId}-label`} class="text-[10px] font-body text-text-muted uppercase tracking-wide">{label}</span>

	{#if value.length > 0}
		<ol class="flex flex-col gap-1">
			{#each value as name, i (name)}
				<li
					class="flex items-center gap-1.5 min-h-[36px] pl-2.5 pr-1.5 rounded-md text-[12px] font-body bg-bg-base border border-border text-text-primary"
				>
					<span class="flex-1">{name}</span>
					{#if multi}
						<button
							type="button"
							onclick={() => moveUp(i)}
							disabled={i === 0}
							aria-label={`Naikkan ${name}`}
							class="min-h-[28px] min-w-[28px] flex items-center justify-center rounded text-text-muted hover:text-text-primary disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							<ChevronUp size={12} aria-hidden="true" />
						</button>
						<button
							type="button"
							onclick={() => moveDown(i)}
							disabled={i === value.length - 1}
							aria-label={`Turunkan ${name}`}
							class="min-h-[28px] min-w-[28px] flex items-center justify-center rounded text-text-muted hover:text-text-primary disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							<ChevronDown size={12} aria-hidden="true" />
						</button>
					{/if}
					<button
						type="button"
						onclick={() => remove(name)}
						aria-label={`Hapus ${name}`}
						class="min-h-[28px] min-w-[28px] flex items-center justify-center rounded text-text-muted hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						<X size={12} aria-hidden="true" />
					</button>
				</li>
			{/each}
		</ol>
	{/if}

	{#if showInput}
		<div class="flex items-center gap-1.5">
			<input
				list={listId}
				type="text"
				bind:value={draft}
				disabled={creating}
				onkeydown={(e) => {
					if (e.key === 'Enter') {
						e.preventDefault();
						commit();
					}
				}}
				aria-labelledby={`${listId}-label`}
				placeholder="Cari atau tambah lokasi"
				class="min-h-[36px] flex-1 px-2.5 rounded-md text-[12px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			<datalist id={listId}>
				{#each locations as loc (loc.id)}
					<option value={loc.name}></option>
				{/each}
			</datalist>
			<button
				type="button"
				onclick={commit}
				disabled={creating || draft.trim() === ''}
				aria-label="Tambah lokasi"
				class="min-h-[36px] min-w-[36px] flex items-center justify-center rounded-md border border-border text-text-muted hover:text-text-primary disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<Plus size={14} aria-hidden="true" />
			</button>
		</div>
	{/if}

	{#if errorMsg}
		<p role="alert" aria-live="polite" class="text-[11px] text-danger">{errorMsg}</p>
	{/if}
</div>
```

**Implementer note (verify live in browser during self-review, not just `pnpm check`):** `<input list>` + `<datalist>` is the native, zero-dependency way to get search-as-you-type suggestions (ponytail: native platform feature over a hand-rolled ARIA combobox). Its exact keyboard behavior when a user arrows-down into a suggestion and presses Enter varies slightly by browser — confirm in a real browser that picking a suggestion via keyboard still lands in `draft` and that this component's own `onkeydown` Enter handler still fires `commit()` afterward (Chromium does; if any target browser genuinely doesn't, note it as a found issue rather than silently reimplementing a custom listbox).

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/LocationCombobox.svelte
git commit -m "feat(frontend): LocationCombobox.svelte — searchable location picker with inline create, single/multi+reorder"
```

---

### Task 5: `AutoAcceptSwitch.svelte` — kill switch + OTP arm modal

No unit test — same rationale as Tasks 3/4 (UI-only; verified via `svelte-check` + Task 8's e2e suite, which specifically exercises the OTP flow against a real backend).

**Files:**
- Create: `Frontend/src/lib/components/AutoAcceptSwitch.svelte`

**Interfaces:**
- Consumes: `requestAaOtp`, `verifyAaOtp` (Task 2, `api-rules.ts`); `ApiError` (`Frontend/src/lib/api.ts`, existing).
- Produces (consumed by Task 7): a component with props `{ enabled: boolean; onChange: (next: boolean) => void; armProofExpired: boolean; onArmProofExpiredHandled: () => void; readOnly: boolean }`. Turning OFF calls `onChange(false)` immediately, no modal. Turning ON opens the OTP modal; on successful verify, calls `onChange(true)` and closes. `armProofExpired`/`onArmProofExpiredHandled` let the page (Task 7) tell this component "your last save failed because the 120s arm window lapsed" — it reopens the modal with an explanatory message and hands control back via the callback, mirroring `TicketDetailDrawer.svelte`'s established `$effect`-reacts-to-prop-change pattern (Fase 7c) rather than an imperative `bind:this` method call. `readOnly` disables the toggle button (both via the native `disabled` attribute and a `toggle()` early-return, matching `RuleRow.svelte`'s `<fieldset disabled>` treatment of every other control on the page — this component is a structural sibling of `RuleRow`, outside any fieldset, so it needs its own explicit gate).

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/AutoAcceptSwitch.svelte -->
<!-- The auto_accept_enabled kill switch + its OTP arm flow. Unlike TicketDetailDrawer.svelte
     (Fase 7c, exactly one focusable element so a full Tab-trap was explicitly deferred), this
     modal has several focusable elements (code input, send/verify/close buttons) and needs a
     REAL wrap-around focus trap — implementing the upgrade path TicketDetailDrawer's own comment
     anticipated, for this component specifically. -->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import { X, ShieldCheck, ShieldOff } from '@lucide/svelte';
	import { requestAaOtp, verifyAaOtp } from '$lib/api-rules';
	import { ApiError } from '$lib/api';

	let {
		enabled,
		onChange,
		armProofExpired,
		onArmProofExpiredHandled,
		readOnly
	}: {
		enabled: boolean;
		onChange: (next: boolean) => void;
		armProofExpired: boolean;
		onArmProofExpiredHandled: () => void;
		readOnly: boolean;
	} = $props();

	const CODE_TTL_MS = 180_000;
	const RESEND_COOLDOWN_MS = 60_000;

	let modalOpen = $state(false);
	let code = $state('');
	let errorMsg = $state('');
	let notConfigured = $state(false);
	let requesting = $state(false);
	let verifying = $state(false);
	let codeExpiresAt = $state<number | null>(null);
	let resendReadyAt = $state<number | null>(null);
	let now = $state(Date.now());
	let dialogEl: HTMLDivElement | undefined = $state();
	let previouslyFocusedEl: HTMLElement | null = null;

	let ticker: ReturnType<typeof setInterval> | undefined;
	$effect(() => {
		if (modalOpen) {
			ticker = setInterval(() => (now = Date.now()), 1000);
			return () => clearInterval(ticker);
		}
	});
	onDestroy(() => clearInterval(ticker));

	const codeSecondsLeft = $derived(codeExpiresAt ? Math.max(0, Math.ceil((codeExpiresAt - now) / 1000)) : 0);
	const resendSecondsLeft = $derived(resendReadyAt ? Math.max(0, Math.ceil((resendReadyAt - now) / 1000)) : 0);

	function openModal(withMessage: string = '') {
		modalOpen = true;
		errorMsg = withMessage;
		notConfigured = false;
		code = '';
		codeExpiresAt = null;
		resendReadyAt = null;
	}

	function closeModal() {
		modalOpen = false;
	}

	async function sendCode() {
		requesting = true;
		errorMsg = '';
		notConfigured = false;
		try {
			await requestAaOtp();
			codeExpiresAt = Date.now() + CODE_TTL_MS;
			resendReadyAt = Date.now() + RESEND_COOLDOWN_MS;
		} catch (e) {
			if (e instanceof ApiError && e.status === 400) {
				notConfigured = true;
			} else if (e instanceof ApiError && e.status === 429) {
				errorMsg = 'Kode sudah dikirim, tunggu sebentar sebelum meminta lagi.';
			} else {
				errorMsg = 'Gagal mengirim kode. Coba lagi.';
			}
		} finally {
			requesting = false;
		}
	}

	async function submitCode() {
		verifying = true;
		errorMsg = '';
		try {
			await verifyAaOtp(code);
			onChange(true);
			modalOpen = false;
		} catch (e) {
			if (e instanceof ApiError && e.status === 401) {
				errorMsg = 'Kode salah atau kedaluwarsa, coba lagi.';
			} else if (e instanceof ApiError && e.status === 429) {
				errorMsg = 'Terlalu banyak percobaan, minta kode baru.';
			} else {
				errorMsg = 'Gagal memverifikasi kode. Coba lagi.';
			}
		} finally {
			verifying = false;
		}
	}

	function toggle() {
		if (readOnly) return;
		if (enabled) {
			onChange(false);
		} else {
			openModal();
		}
	}

	// Mirrors TicketDetailDrawer.svelte's established pattern: react to a prop transition via
	// $effect rather than an imperative bind:this method, so the page (Task 7) stays purely
	// declarative when it needs to tell this component "the arm window lapsed, ask again."
	$effect(() => {
		if (armProofExpired) {
			openModal('Kode kedaluwarsa, verifikasi ulang.');
			onArmProofExpiredHandled();
		}
	});

	$effect(() => {
		if (modalOpen) {
			previouslyFocusedEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			const firstFocusable = dialogEl?.querySelector<HTMLElement>('button:not([disabled]), input:not([disabled])');
			firstFocusable?.focus();
		} else if (previouslyFocusedEl) {
			previouslyFocusedEl.focus();
			previouslyFocusedEl = null;
		}
	});

	function handleDialogKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			closeModal();
			return;
		}
		if (e.key !== 'Tab' || !dialogEl) return;
		const focusables = Array.from(
			dialogEl.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled])')
		);
		if (focusables.length === 0) return;
		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		if (e.shiftKey && document.activeElement === first) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && document.activeElement === last) {
			e.preventDefault();
			first.focus();
		}
	}
</script>

<div class="flex items-center justify-between gap-3 p-4 rounded-lg border border-border bg-bg-surface">
	<div class="flex items-center gap-2.5">
		{#if enabled}
			<ShieldCheck size={18} class="text-live" aria-hidden="true" />
		{:else}
			<ShieldOff size={18} class="text-text-muted" aria-hidden="true" />
		{/if}
		<div>
			<p class="text-[13px] font-heading font-semibold text-text-primary">Auto-Accept</p>
			<p class="text-[11px] font-body text-text-muted" aria-live="polite">
				{enabled ? 'Aktif — booking cocok diterima otomatis' : 'Nonaktif'}
			</p>
		</div>
	</div>
	<button
		type="button"
		role="switch"
		aria-checked={enabled}
		aria-label="Aktifkan atau nonaktifkan Auto-Accept"
		onclick={toggle}
		disabled={readOnly}
		class={`min-h-[44px] min-w-[44px] px-4 rounded-md text-[12px] font-body border disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
			enabled ? 'bg-live text-bg-base border-live' : 'bg-bg-base text-text-muted border-border'
		}`}
	>
		{enabled ? 'ON' : 'OFF'}
	</button>
</div>

{#if modalOpen}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-bg-base/70 p-4">
		<div
			bind:this={dialogEl}
			role="dialog"
			aria-modal="true"
			aria-label="Verifikasi kode OTP"
			onkeydown={handleDialogKeydown}
			class="w-full max-w-sm flex flex-col gap-3 p-4 rounded-lg border border-border bg-bg-surface"
		>
			<div class="flex items-center justify-between">
				<h2 class="text-[13px] font-heading font-semibold text-text-primary">Verifikasi Auto-Accept</h2>
				<button
					type="button"
					onclick={closeModal}
					aria-label="Tutup"
					class="min-h-[32px] min-w-[32px] flex items-center justify-center rounded text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					<X size={16} aria-hidden="true" />
				</button>
			</div>

			{#if notConfigured}
				<p role="alert" aria-live="polite" class="text-[12px] text-danger">
					Pengiriman OTP belum dikonfigurasi untuk tenant ini. Hubungi admin untuk mengatur nomor WhatsApp
					sebelum mengaktifkan Auto-Accept.
				</p>
			{:else}
				{#if errorMsg}
					<p role="alert" aria-live="polite" class="text-[12px] text-danger">{errorMsg}</p>
				{/if}

				{#if codeExpiresAt === null}
					<button
						type="button"
						onclick={sendCode}
						disabled={requesting}
						class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{requesting ? 'Mengirim…' : 'Kirim kode'}
					</button>
				{:else}
					<p class="text-[11px] font-mono text-text-muted" aria-live="polite">
						{codeSecondsLeft > 0 ? `Kode berlaku ${codeSecondsLeft} detik lagi` : 'Kode sudah kedaluwarsa'}
					</p>
					<label class="flex flex-col gap-1">
						<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Kode OTP</span>
						<input
							type="text"
							inputmode="numeric"
							maxlength="6"
							bind:value={code}
							class="min-h-[44px] px-2.5 rounded-md text-[16px] font-mono tracking-widest bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						/>
					</label>
					<button
						type="button"
						onclick={submitCode}
						disabled={verifying || code.trim() === ''}
						class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{verifying ? 'Memverifikasi…' : 'Verifikasi'}
					</button>
					<button
						type="button"
						onclick={sendCode}
						disabled={requesting || resendSecondsLeft > 0}
						class="min-h-[44px] px-4 rounded-md text-[12px] font-body text-text-muted disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{resendSecondsLeft > 0 ? `Kirim ulang (${resendSecondsLeft}s)` : 'Kirim ulang'}
					</button>
				{/if}
			{/if}
		</div>
	</div>
{/if}
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/AutoAcceptSwitch.svelte
git commit -m "feat(frontend): AutoAcceptSwitch.svelte — kill switch + OTP arm modal with focus trap"
```

---

### Task 6: `RuleRow.svelte`

No unit test — same rationale as Tasks 3/4/5 (UI-only; verified via `svelte-check` + Task 8's e2e suite).

**Files:**
- Create: `Frontend/src/lib/components/RuleRow.svelte`

**Interfaces:**
- Consumes: `RuleDraft`, `RuleConditions`, `conditionSummary`, `ruleIsEmpty`, `setCocOnly`, `setNonCocOnly`, `SERVICE_TYPE_OPTIONS`, `SHIFT_TYPE_OPTIONS`, `TRIP_TYPE_OPTIONS` (Task 1); `LocationItem` (Task 2); `ChipInput.svelte` (Task 3); `LocationCombobox.svelte` (Task 4).
- Produces (consumed by Task 7): a component with props `{ rule: RuleDraft; locations: LocationItem[]; onCreateLocation: (name: string) => Promise<LocationItem>; onChange: (rule: RuleDraft) => void; onDelete: () => void; readOnly: boolean }`.

**Collapse/expand keyboard pattern:** deliberately NOT a nested `<button>`-inside-clickable-area layout — Fase 7c's Task 6 hit a Critical bug from exactly that shape (a nested "Terima" button's Enter/Space bubbled into the row's own handler). Here, the expand-toggle region and the enabled-checkbox/delete-button are **structural siblings** in the same flex row, not nested, so there is nothing to bubble and no target-guard is even needed (the guard is only necessary when an interactive control is nested INSIDE another clickable element, which this layout avoids by construction).

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/RuleRow.svelte -->
<!-- One AcceptRule: collapsed summary row + expand-in-place editor. Mode selector swaps the
     visible field set (booking_id: just booking IDs; route: origin/destinations/match_mode;
     filter: neither) on top of the shared fields every mode has. All fields disabled at once via
     a native <fieldset disabled> when readOnly — cascades through ChipInput/LocationCombobox's
     own internal <input>/<button> elements regardless of component boundaries, so no per-field
     readOnly prop threading is needed. -->
<script lang="ts">
	import { Trash2 } from '@lucide/svelte';
	import {
		conditionSummary,
		ruleIsEmpty,
		setCocOnly,
		setNonCocOnly,
		SERVICE_TYPE_OPTIONS,
		SHIFT_TYPE_OPTIONS,
		TRIP_TYPE_OPTIONS,
		type RuleDraft,
		type RuleConditions,
		type RuleMode
	} from '$lib/rules';
	import type { LocationItem } from '$lib/api-rules';
	import ChipInput from './ChipInput.svelte';
	import LocationCombobox from './LocationCombobox.svelte';

	let {
		rule,
		locations,
		onCreateLocation,
		onChange,
		onDelete,
		readOnly
	}: {
		rule: RuleDraft;
		locations: LocationItem[];
		onCreateLocation: (name: string) => Promise<LocationItem>;
		onChange: (rule: RuleDraft) => void;
		onDelete: () => void;
		readOnly: boolean;
	} = $props();

	let expanded = $state(false);

	const MODE_OPTIONS: { value: RuleMode; label: string }[] = [
		{ value: 'booking_id', label: 'Booking ID' },
		{ value: 'route', label: 'Rute' },
		{ value: 'filter', label: 'Filter' }
	];

	function modeLabel(mode: RuleMode): string {
		return MODE_OPTIONS.find((o) => o.value === mode)?.label ?? mode;
	}

	function updateRule(patch: Partial<RuleDraft>) {
		onChange({ ...rule, ...patch });
	}

	function updateConditions(patch: Partial<RuleConditions>) {
		onChange({ ...rule, conditions: { ...rule.conditions, ...patch } });
	}

	function numOrNull(raw: string): number | null {
		return raw === '' ? null : Number(raw);
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
				<span class="text-[10px] font-body px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-muted uppercase">
					{modeLabel(rule.mode)}
				</span>
				<span class="text-[13px] font-heading font-medium text-text-primary">{rule.name || 'Rule tanpa nama'}</span>
			</div>
			<span class="text-[11px] font-mono text-text-muted">{conditionSummary(rule)}</span>
		</div>

		<label class="flex items-center gap-1.5 text-[11px] font-body text-text-muted">
			<input
				type="checkbox"
				checked={rule.enabled}
				disabled={readOnly}
				onchange={(e) => updateRule({ enabled: (e.target as HTMLInputElement).checked })}
				class="h-4 w-4 accent-accent"
			/>
			Aktif
		</label>

		{#if !readOnly}
			<button
				type="button"
				onclick={onDelete}
				aria-label={`Hapus rule ${rule.name || 'tanpa nama'}`}
				class="min-h-[36px] min-w-[36px] flex items-center justify-center rounded text-text-muted hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<Trash2 size={14} aria-hidden="true" />
			</button>
		{/if}
	</div>

	{#if expanded}
		<fieldset disabled={readOnly} class="flex flex-col gap-3 p-3 pt-0 border-0">
			{#if ruleIsEmpty(rule)}
				<p class="text-[11px] text-accent">Rule ini belum punya kondisi — belum akan cocok dengan booking apa pun.</p>
			{/if}

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nama</span>
				<input
					type="text"
					value={rule.name}
					oninput={(e) => updateRule({ name: (e.target as HTMLInputElement).value })}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<div class="flex gap-1" role="radiogroup" aria-label="Mode rule">
				{#each MODE_OPTIONS as opt (opt.value)}
					<button
						type="button"
						role="radio"
						aria-checked={rule.mode === opt.value}
						onclick={() => updateRule({ mode: opt.value })}
						class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
							rule.mode === opt.value
								? 'bg-accent text-bg-base border-accent'
								: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
						}`}
					>
						{opt.label}
					</button>
				{/each}
			</div>

			{#if rule.mode === 'booking_id'}
				<ChipInput
					label="Booking ID"
					value={rule.conditions.bookingIds}
					onChange={(v) => updateConditions({ bookingIds: v })}
				/>
			{/if}

			{#if rule.mode === 'route'}
				<LocationCombobox
					label="Asal"
					{locations}
					{onCreateLocation}
					value={rule.conditions.origin ? [rule.conditions.origin] : []}
					onChange={(v) => updateConditions({ origin: v[0] ?? '' })}
				/>
				<LocationCombobox
					label="Tujuan (urut, maks 5)"
					{locations}
					{onCreateLocation}
					value={rule.conditions.destinations}
					onChange={(v) => updateConditions({ destinations: v })}
					multi
					max={5}
				/>
				<div class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Mode Cocok</span>
					<div class="flex gap-1" role="radiogroup" aria-label="Mode cocok rute">
						{#each [{ value: 'strict', label: 'Ketat' }, { value: 'flexible', label: 'Fleksibel' }] as opt (opt.value)}
							<button
								type="button"
								role="radio"
								aria-checked={rule.conditions.matchMode === opt.value}
								onclick={() => updateConditions({ matchMode: opt.value as 'strict' | 'flexible' })}
								class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
									rule.conditions.matchMode === opt.value
										? 'bg-accent text-bg-base border-accent'
										: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
								}`}
							>
								{opt.label}
							</button>
						{/each}
					</div>
					<p class="text-[11px] text-text-muted">
						{rule.conditions.matchMode === 'strict'
							? 'Semua destinasi wajib muncul berurutan.'
							: 'Hanya destinasi terakhir yang wajib muncul.'}
					</p>
				</div>
			{/if}

			<ChipInput
				label="Jenis Kendaraan"
				value={rule.conditions.serviceTypes}
				onChange={(v) => updateConditions({ serviceTypes: v })}
				options={SERVICE_TYPE_OPTIONS.map((v) => ({ value: v, label: v }))}
			/>

			<div class="grid grid-cols-2 gap-3">
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Berat Maks (kg)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.maxWeight ?? ''}
						oninput={(e) => updateConditions({ maxWeight: numOrNull((e.target as HTMLInputElement).value) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">COD Maks (Rp)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.maxCodAmount ?? ''}
						oninput={(e) => updateConditions({ maxCodAmount: numOrNull((e.target as HTMLInputElement).value) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>

			<div class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Tipe Booking</span>
				<div class="flex gap-1" role="radiogroup" aria-label="Tipe booking">
					{#each [{ value: 'all', label: 'Semua' }, { value: 'spxid', label: 'SPXID' }, { value: 'reguler', label: 'Reguler' }] as opt (opt.value)}
						<button
							type="button"
							role="radio"
							aria-checked={rule.conditions.bookingType === opt.value}
							onclick={() => updateConditions({ bookingType: opt.value as 'all' | 'spxid' | 'reguler' })}
							class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
								rule.conditions.bookingType === opt.value
									? 'bg-accent text-bg-base border-accent'
									: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
							}`}
						>
							{opt.label}
						</button>
					{/each}
				</div>
			</div>

			<ChipInput
				label="Shift"
				value={rule.conditions.shiftTypes.map(String)}
				onChange={(v) => updateConditions({ shiftTypes: v.map(Number) })}
				options={SHIFT_TYPE_OPTIONS.map((o) => ({ value: String(o.value), label: o.label }))}
			/>

			<ChipInput
				label="Jenis Trip"
				value={rule.conditions.tripTypes.map(String)}
				onChange={(v) => updateConditions({ tripTypes: v.map(Number) })}
				options={TRIP_TYPE_OPTIONS.map((o) => ({ value: String(o.value), label: o.label }))}
			/>

			<div class="flex gap-4">
				<label class="flex items-center gap-1.5 text-[12px] font-body text-text-primary">
					<input
						type="checkbox"
						checked={rule.conditions.cocOnly}
						onchange={(e) => updateConditions(setCocOnly(rule.conditions, (e.target as HTMLInputElement).checked))}
						class="h-4 w-4 accent-accent"
					/>
					Hanya COC
				</label>
				<label class="flex items-center gap-1.5 text-[12px] font-body text-text-primary">
					<input
						type="checkbox"
						checked={rule.conditions.nonCocOnly}
						onchange={(e) => updateConditions(setNonCocOnly(rule.conditions, (e.target as HTMLInputElement).checked))}
						class="h-4 w-4 accent-accent"
					/>
					Hanya Non-COC
				</label>
			</div>

			<div class="grid grid-cols-3 gap-3">
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Min. Deadline (menit)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.minDeadlineMin ?? ''}
						oninput={(e) => updateConditions({ minDeadlineMin: numOrNull((e.target as HTMLInputElement).value) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Kuota Maks (0 = tanpa batas)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.maxAcceptCount}
						oninput={(e) =>
							updateConditions({ maxAcceptCount: Math.max(0, Number((e.target as HTMLInputElement).value) || 0) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<div class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Sudah Diterima</span>
					<span class="min-h-[40px] flex items-center px-2.5 rounded-md text-[13px] font-mono bg-bg-base border border-border text-text-muted">
						{rule.conditions.acceptedCount}
					</span>
				</div>
			</div>

			<label class="flex flex-col gap-1 max-w-[160px]">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Prioritas (-999..999)</span>
				<input
					type="number"
					min="-999"
					max="999"
					value={rule.priority}
					oninput={(e) => updateRule({ priority: Number((e.target as HTMLInputElement).value) || 0 })}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-mono bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
		</fieldset>
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/RuleRow.svelte
git commit -m "feat(frontend): RuleRow.svelte — mode-conditional rule editor (expand-in-place)"
```

---

### Task 7: `/rules/+page.svelte` — page assembly

No unit test — page assembly, verified via `svelte-check` + Task 8's e2e suite.

**Files:**
- Create: `Frontend/src/routes/(app)/rules/+page.svelte`

**Interfaces:**
- Consumes: `fetchSettings`, `saveSettings`, `fetchLocations`, `createLocation`, `LocationItem` (Task 2); `newRuleDraft`, `isDirty`, `RuleDraft`, `RulesPageState`, `RuleMode` (Task 1); `RuleRow.svelte` (Task 6); `AutoAcceptSwitch.svelte`, including its `readOnly` prop (Task 5); `ApiError` (`Frontend/src/lib/api.ts`); `data.user.is_main_account` from `(app)/+layout.server.ts`'s existing `load` (already returns `{user: {username, display_name, is_main_account}}` — SvelteKit merges an ancestor layout's `load` return into every descendant `+page.svelte`'s own `data` prop automatically, no new load function needed here).

Session-gating itself (redirect to `/login` if unauthenticated) is already handled by `(app)/+layout.server.ts` for every route under this group — this page needs no auth logic of its own beyond reading `is_main_account`.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/rules/+page.svelte -->
<!-- /rules: local-edit + single-Save Rule Builder. All mutations happen against local $state;
     one "Simpan Perubahan" PUTs the whole set and REPLACES local state with the response (never
     merges) — this is how the user sees server-side dedupe/collapse and sanitize warnings
     reflected, matching what the backend actually did. -->
<script lang="ts">
	import { beforeNavigate } from '$app/navigation';
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchSettings, saveSettings, fetchLocations, createLocation, type LocationItem } from '$lib/api-rules';
	import { ApiError } from '$lib/api';
	import { newRuleDraft, isDirty, type RuleDraft, type RulesPageState, type RuleMode } from '$lib/rules';
	import RuleRow from '$lib/components/RuleRow.svelte';
	import AutoAcceptSwitch from '$lib/components/AutoAcceptSwitch.svelte';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let autoAcceptEnabled = $state(false);
	let rules = $state<RuleDraft[]>([]);
	let locations = $state<LocationItem[]>([]);
	let lastSaved = $state<RulesPageState>({ autoAcceptEnabled: false, rules: [] });
	let loading = $state(true);
	let saving = $state(false);
	let errorMsg = $state('');
	let warnings = $state<string[]>([]);
	let armProofExpired = $state(false);

	const dirty = $derived(isDirty({ autoAcceptEnabled, rules }, lastSaved));

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const [settings, locs] = await Promise.all([fetchSettings(), fetchLocations()]);
			autoAcceptEnabled = settings.autoAcceptEnabled;
			rules = settings.rules;
			warnings = settings.warnings;
			lastSaved = { autoAcceptEnabled: settings.autoAcceptEnabled, rules: settings.rules };
			locations = locs;
		} catch {
			errorMsg = 'Gagal memuat pengaturan rule. Coba lagi.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	function addRule(mode: RuleMode) {
		rules = [...rules, newRuleDraft(mode)];
	}

	function updateRule(clientKey: string, next: RuleDraft) {
		rules = rules.map((r) => (r.clientKey === clientKey ? next : r));
	}

	function deleteRule(clientKey: string) {
		rules = rules.filter((r) => r.clientKey !== clientKey);
	}

	async function handleCreateLocation(name: string): Promise<LocationItem> {
		const created = await createLocation(name);
		locations = [...locations, created];
		return created;
	}

	async function save() {
		saving = true;
		errorMsg = '';
		try {
			const result = await saveSettings({ autoAcceptEnabled, rules });
			autoAcceptEnabled = result.autoAcceptEnabled;
			rules = result.rules;
			warnings = result.warnings;
			lastSaved = { autoAcceptEnabled: result.autoAcceptEnabled, rules: result.rules };
		} catch (e) {
			// Narrowed to exactly the arm-attempt case (mirrors the backend's own
			// `if body.auto_accept_enabled && !currently_enabled` gate in put_settings): a 401 here
			// means the OTP proof window lapsed. A 401 in any OTHER state (session actually expired
			// mid-page) would be misreported as "OTP expired" too — a disclosed limitation, not
			// fixed here, since apiPost/fetch in this codebase don't surface distinguishing error
			// body text (Fase 7c's Task 6 tracked the same generic-error-message gap for
			// /bookings/:id/accept; this is the same underlying apiPost/ApiError design boundary,
			// out of scope to fix from this page alone).
			if (e instanceof ApiError && e.status === 401 && autoAcceptEnabled && !lastSaved.autoAcceptEnabled) {
				armProofExpired = true;
			} else if (e instanceof ApiError) {
				errorMsg = 'Gagal menyimpan. Coba lagi.';
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		} finally {
			saving = false;
		}
	}

	beforeNavigate((nav) => {
		if (dirty && !confirm('Ada perubahan yang belum disimpan. Tetap tinggalkan halaman?')) {
			nav.cancel();
		}
	});
</script>

<svelte:window
	onbeforeunload={(e) => {
		if (dirty) e.preventDefault();
	}}
/>

<svelte:head>
	<title>Rules — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-3xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Rule Builder</h1>

	{#if readOnly}
		<div
			role="alert"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
		>
			Hanya akun utama yang dapat mengubah rule.
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

	{#if warnings.length > 0}
		<div
			role="alert"
			aria-live="polite"
			class="flex flex-col gap-1 px-3.5 py-2.5 rounded-lg text-[12px] font-body border bg-accent/10 text-accent border-accent/30"
		>
			{#each warnings as w (w)}
				<p>{w}</p>
			{/each}
		</div>
	{/if}

	{#if loading}
		<p class="text-[12px] text-text-muted">Memuat…</p>
	{:else}
		<AutoAcceptSwitch
			enabled={autoAcceptEnabled}
			onChange={(next) => (autoAcceptEnabled = next)}
			{armProofExpired}
			onArmProofExpiredHandled={() => (armProofExpired = false)}
			{readOnly}
		/>

		<div class="flex flex-col gap-2">
			{#each rules as rule (rule.clientKey)}
				<RuleRow
					{rule}
					{locations}
					onCreateLocation={handleCreateLocation}
					onChange={(next) => updateRule(rule.clientKey, next)}
					onDelete={() => deleteRule(rule.clientKey)}
					{readOnly}
				/>
			{/each}
		</div>

		{#if !readOnly}
			<div class="flex gap-2">
				<button
					type="button"
					onclick={() => addRule('route')}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Rule Rute
				</button>
				<button
					type="button"
					onclick={() => addRule('booking_id')}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Rule Booking ID
				</button>
				<button
					type="button"
					onclick={() => addRule('filter')}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Rule Filter
				</button>
			</div>

			<button
				type="button"
				onclick={save}
				disabled={saving || !dirty}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{saving ? 'Menyimpan…' : 'Simpan Perubahan'}
			</button>
		{/if}
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`. If `PageProps` is not recognized from `./$types`, run `pnpm exec svelte-kit sync` first (regenerates route types) then re-run `pnpm check`.

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/rules/+page.svelte"
git commit -m "feat(frontend): /rules page assembly — data flow, save, warnings, dirty-guard, permission gating"
```

---

### Task 8: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/rules.spec.ts`

**Interfaces:**
- Consumes: the full `/rules` page built in Tasks 1-7. No new frontend code — this task authors real-stack e2e coverage and runs the full verification suite.

#### Step 1: Seed a non-main-account test user (one-time, direct `psql`)

Mirrors `Frontend/tests/login.spec.ts`'s own `e2e-test-user` seed exactly (same tenant `tower-dev`, id `e03ac22f-729b-436f-a112-08aab5022614`), reusing the SAME password hash literal (`correct-horse-battery-staple` — there is no uniqueness constraint on `password_hash`, so reusing the exact hash string for a second account is valid; no need to re-run the throwaway `hash_pw` example). Distinct username to satisfy `portal_users_tenant_username_unique`. `is_main_account = false` is the whole point of this row.

```bash
PGPASSWORD=tower_dev_only psql -h 127.0.0.1 -p 15432 -U tower -d tower -c "
  INSERT INTO portal_users (tenant_id, username, password_hash, display_name, is_main_account)
  VALUES ('e03ac22f-729b-436f-a112-08aab5022614', 'e2e-readonly-user',
           '\$argon2id\$v=19\$m=19456,t=2,p=1\$wpqXhXebq5sOx4tdhOFnJQ\$rGCnrcQzZfOaFihhvFRi/nskuDjEYSdvlOHZOdaiw7Y',
           'Fase 7d E2E Read-Only User', false);
"
```

#### Step 2: Seed `site_settings.waha_settings` so the OTP request doesn't 400 (one-time, direct `psql`)

`POST /auth/request-aa-otp` 400s with "OTP delivery is not configured for this tenant" unless a `site_settings` row exists with a non-empty `wa_number` (`Backend/crates/api-gateway/src/routes/otp.rs`'s `load_bot_settings`). The actual WhatsApp delivery attempt (`notifier::waha::send_to_waha_many`) is **non-fatal on failure** — `request_otp`'s handler still returns `200 {ok: true}` even when `sent == 0`, it only logs a warning (verified by reading the handler directly, not assumed) — so `waha_url` can point at an address nothing listens on (fails fast with connection-refused, no slow timeout) and the OTP code still gets generated and stored in Redis regardless. `api_key_ciphertext_b64`/`api_key_nonce_b64`/`key_version` are required fields on `WahaSettings` but are never decrypted anywhere in this flow — any placeholder base64 string is fine.

```bash
PGPASSWORD=tower_dev_only psql -h 127.0.0.1 -p 15432 -U tower -d tower -c "
  INSERT INTO site_settings (tenant_id, key, value)
  VALUES (
    'e03ac22f-729b-436f-a112-08aab5022614',
    'waha_settings',
    '{
      \"waha_url\": \"http://127.0.0.1:19999\",
      \"waha_session\": \"default\",
      \"wa_number\": \"628111111111\",
      \"enabled\": true,
      \"webhook_url\": \"\",
      \"wa_group\": \"\",
      \"portal_label\": \"\",
      \"api_key_ciphertext_b64\": \"cGxhY2Vob2xkZXI=\",
      \"api_key_nonce_b64\": \"AAAAAAAAAAAAAAAA\",
      \"key_version\": 1
    }'::jsonb
  )
  ON CONFLICT (tenant_id, key) DO UPDATE SET value = EXCLUDED.value;
"
```

#### Step 3: Write `Frontend/tests/rules.spec.ts`

`e2e-test-user`'s real `portal_users.id` is `0b93247e-2e8d-494a-bcc2-0908389605f0` — looked up directly (`SELECT id FROM portal_users WHERE tenant_id='e03ac22f-729b-436f-a112-08aab5022614' AND username='e2e-test-user'`), not guessed, since the OTP flow test needs the exact Redis key `otp::code_key` builds (`spx:aa_otp:<tenant_id>:<portal_user_id>`, `Backend/crates/api-gateway/src/otp.rs:16-18`) to read the freshly-generated code. This id is stable (the row was seeded once, in Fase 7a) — re-verify it's still correct if `psql -c "SELECT id FROM portal_users WHERE username='e2e-test-user'"` ever returns something different (e.g. a fresh dev DB), and update the constant below if so.

```typescript
// Frontend/tests/rules.spec.ts
//
// REAL end-to-end proof of Fase 7d's /rules Rule Builder. Same real-stack setup as
// tests/login.spec.ts, tests/command.spec.ts, tests/tickets.spec.ts — real reactor-core on
// :8081 behind Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432), real Redis
// (tower-redis, 127.0.0.1:16379). Nothing here is mocked or stubbed.
//
// Prerequisites (see this task's Steps 1-2 for the exact commands, already run once against the
// dev DB before this file was written):
// - e2e-test-user (main-account, from Fase 7a) — used for all rule-editing + OTP-arm tests.
// - e2e-readonly-user / correct-horse-battery-staple (is_main_account=false) — used for the
//   read-only-view test.
// - tower-dev's site_settings.waha_settings row has a non-empty wa_number, so
//   /auth/request-aa-otp succeeds (200) instead of 400ing.
//
// No accept_rules rows are pre-seeded — /rules' whole purpose is creating rules through the UI,
// so the "load and display" coverage below creates a rule via the real Save flow and reloads the
// page to prove it persisted, rather than hand-crafting an INSERT (accept_rules.route_signature
// is a GENERATED ALWAYS column and must never be inserted explicitly — this suite avoids the
// question entirely by never inserting into accept_rules at all).
//
// The OTP code is generated fresh (random) on every /auth/request-aa-otp call, so it cannot be
// pre-seeded — it's read LIVE mid-test directly from Redis via `redis-cli`, the same
// "read/seed backend state directly" precedent tickets.spec.ts and login.spec.ts already
// established via psql, just via Redis instead of Postgres. `e2e-test-user`'s real
// portal_users.id (looked up directly, see this task's Step 3 preamble) is baked into the Redis
// key this constant builds.

import { test, expect } from '@playwright/test';
import { execSync } from 'node:child_process';

const TENANT_ID = 'e03ac22f-729b-436f-a112-08aab5022614';
const E2E_TEST_USER_ID = '0b93247e-2e8d-494a-bcc2-0908389605f0';

function readOtpCodeFromRedis(): string {
	const key = `spx:aa_otp:${TENANT_ID}:${E2E_TEST_USER_ID}`;
	return execSync(`redis-cli -h 127.0.0.1 -p 16379 GET "${key}"`).toString().trim();
}

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /rules redirects to /login', async ({ page }) => {
	await page.goto('/rules');
	await expect(page).toHaveURL(/\/login/);
});

test('main account can create a route-mode rule with a new inline location, save, and it persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/rules');
	await expect(page.getByRole('heading', { name: 'Rule Builder' })).toBeVisible();

	await page.getByRole('button', { name: '+ Rule Rute' }).click();
	// The newly added rule row starts collapsed with no name — expand it (its own clickable
	// header region, not a nested button — see RuleRow.svelte's structural-sibling layout note).
	await page.getByText('Rule tanpa nama').click();

	await page.getByLabel('Nama').fill('E2E Padang Lane');

	// Origin: type a brand-new location name (nothing in route_locations matches it yet) and
	// press Enter — LocationCombobox's commit() falls through to onCreateLocation, exercising the
	// real POST /locations call, not just a pre-existing pick.
	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Padang DC');
	await originInput.press('Enter');
	await expect(page.getByText('E2E Padang DC', { exact: true })).toBeVisible();

	const destInput = page.getByLabel('Tujuan (urut, maks 5)');
	await destInput.fill('E2E Cileungsi DC');
	await destInput.press('Enter');
	await expect(page.getByText('E2E Cileungsi DC', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	// Reload from scratch — proves the save actually persisted server-side, not just local state.
	await page.reload();
	await expect(page.getByText('E2E Padang Lane')).toBeVisible({ timeout: 10_000 });
});

test('editing and deleting an existing rule persists after save and reload', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/rules');

	// Add a throwaway filter-mode rule specifically for this test (independent of the route-mode
	// rule the previous test created — this suite's tests do not depend on execution order, each
	// creates its own fixture data).
	await page.getByRole('button', { name: '+ Rule Filter' }).click();
	await page.getByText('Rule tanpa nama').last().click();
	const nameInputs = page.getByLabel('Nama');
	await nameInputs.last().fill('E2E Throwaway Filter');
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E Throwaway Filter')).toBeVisible({ timeout: 10_000 });

	// Expand it, delete it, save, reload, confirm it's gone.
	await page.getByText('E2E Throwaway Filter').click();
	await page.getByRole('button', { name: 'Hapus rule E2E Throwaway Filter' }).click();
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E Throwaway Filter')).toBeHidden({ timeout: 10_000 });
});

test('non-main-account session sees a read-only view with no edit controls', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/rules');
	await expect(page.getByText('Hanya akun utama yang dapat mengubah rule.')).toBeVisible();
	await expect(page.getByRole('button', { name: '+ Rule Rute' })).toBeHidden();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeHidden();
	// The kill switch is still visible (view is ungated) but must be disabled — AutoAcceptSwitch
	// is a structural sibling of RuleRow's <fieldset>, not nested inside it, so it needs (and
	// Task 5/7 give it) its own explicit `readOnly` prop rather than inheriting the fieldset's
	// disabling for free.
	await expect(page.getByRole('switch', { name: 'Aktifkan atau nonaktifkan Auto-Accept' })).toBeDisabled();
});

test('OTP arm flow: request code, read it from Redis, verify, and auto-accept status persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/rules');

	const killSwitch = page.getByRole('switch', { name: 'Aktifkan atau nonaktifkan Auto-Accept' });
	// Skip if already ON from a prior run of this suite against the same dev DB (idempotent
	// re-run safety, matching this suite's other tests' "reload and confirm" pattern rather than
	// assuming a pristine starting state).
	if ((await killSwitch.getAttribute('aria-checked')) === 'true') {
		test.skip();
	}

	await killSwitch.click();
	await expect(page.getByRole('dialog', { name: 'Verifikasi kode OTP' })).toBeVisible();
	await page.getByRole('button', { name: 'Kirim kode' }).click();
	await expect(page.getByLabel('Kode OTP')).toBeVisible({ timeout: 10_000 });

	const code = readOtpCodeFromRedis();
	expect(code).toMatch(/^\d{6}$/);
	await page.getByLabel('Kode OTP').fill(code);
	await page.getByRole('button', { name: 'Verifikasi' }).click();
	await expect(page.getByRole('dialog', { name: 'Verifikasi kode OTP' })).toBeHidden({ timeout: 10_000 });
	await expect(killSwitch).toHaveAttribute('aria-checked', 'true');

	// Arming flips local state to ON, but PUT /bookings/settings is what actually persists it
	// (per the 120s pwverify-proof contract) — Save must happen while that window is open.
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByRole('switch', { name: 'Aktifkan atau nonaktifkan Auto-Accept' })).toHaveAttribute(
		'aria-checked',
		'true',
		{ timeout: 10_000 }
	);
});
```


#### Step 4: Run the new e2e file alone

Run: `cd Frontend && pnpm exec playwright test tests/rules.spec.ts`
Expected: all tests pass (a live `reactor-core` + `tower-postgres` + `tower-redis` stack must already be running — see `tests/login.spec.ts`'s header comment for the exact `DATABASE_URL`/`REDIS_URL`/`TENANT_SLUG` env the manually-started `reactor-core` needs).

#### Step 5: Run the full Playwright suite (regression check)

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across `login.spec.ts`, `command.spec.ts`, `tickets.spec.ts`, `rules.spec.ts` pass — no regression in earlier phases' coverage.

#### Step 6: Full backend verification

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green, no warnings, no advisory/ban/license/source violations. **Use the `tower` superuser URL for `cargo test`, not `app_role`** — this workspace's tests run migrations directly, and `app_role` lacks `CREATE` on the `public` schema (a local-dev-only gotcha unrelated to this plan's own code; using `app_role` here fails ~5 unrelated `auth_routes` tests with `permission denied for schema public`).

#### Step 7: Full frontend verification

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `rules.test.ts`, Task 2's `api-rules.test.ts`, plus every pre-existing suite — no regression); production build succeeds.

#### Step 8: Commit

```bash
git add Frontend/tests/rules.spec.ts
git commit -m "test(fase-7d): /rules e2e (Playwright, incl. OTP arm flow via live Redis read) — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — local-edit+single-Save (Task 7), all 3 modes (Task 6), kill switch + OTP (Task 5), inline location creation (Task 4), read-only gating (Task 6's `<fieldset disabled>` for rule fields, Task 5/7's `readOnly` prop for the kill switch — both covered, not just one), dirty-state guard (Task 7). Every "Out of scope" bullet (rule-testing tool, templates, audit trail, drag-reorder) has no corresponding task — confirmed absent.

**Placeholder scan:** no TBD/TODO. While drafting Task 8's read-only test, this review caught that `AutoAcceptSwitch.svelte` (Task 5) — a structural sibling of `RuleRow`'s `<fieldset>`, not nested inside it — had no `readOnly` prop of its own, meaning a non-main-account user's client could still open the OTP modal even though the server would ultimately 403 the save. Fixed inline: `readOnly` prop added to Task 5's component (disables the toggle both via the native `disabled` attribute and a `toggle()` early-return) and threaded through from Task 7; Task 8's test asserts the switch is actually `disabled`. No open items remain.

**Type consistency:** `RuleDraft`/`RuleConditions`/`RulesPageState` (Task 1) are the same shapes threaded unchanged through Task 2's wire mapping, Task 6's `RuleRow` props, and Task 7's page state — no renamed fields between tasks (verified by re-reading each task's prop/interface list against Task 1's definitions while writing this plan, not assumed).

**Cross-task dependency ordering:** 1 (types) → 2 (wire mapping, depends on 1) → 3, 4 (generic UI primitives, depend on nothing but Lucide icons) → 5 (kill switch, depends on 2's OTP calls) → 6 (RuleRow, depends on 1, 3, 4) → 7 (page, depends on 1, 2, 5, 6) → 8 (e2e, depends on everything). No task references a later task's output.

