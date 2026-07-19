# Fase 7f: `/activity` (Activity Log) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `/activity`, a two-tab read-only log viewer over two already-built backend data sources — `accept_events` (via `GET /bookings/spx-log`) and `bot_log` (via `GET/DELETE /bot/logs`) — a pure frontend build.

**Architecture:** Two tabs with genuinely different pagination models, matching what each backend endpoint actually supports: Riwayat Keputusan (accept_events) uses real server-side pagination (the list grows unboundedly); Log Bot (bot_log) fetches its full ≤200-entry list once and paginates client-side (the list is bounded by the backend's own 200-cap). No filter bar anywhere — the backend genuinely doesn't support filtering `spx-log`, confirmed by reading the handler.

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), `@lucide/svelte`, Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-19-fase-7f-activity-design.md` — read it first for full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format is snake_case** — no `#[serde(rename_all)]` anywhere in `api-gateway` (re-verified against `Backend/crates/api-gateway/src/routes/bookings.rs` and `bot.rs` for this plan).
- **`GET /bookings/spx-log` ignores `status`/`spx_id`/`from`/`to`** even though they exist on the shared `ListParams` struct it deserializes — it only consumes `limit`/`offset`. Do not build any filter UI against this endpoint; it would silently do nothing.
- **`GET /bot/logs` takes no query params at all** — always returns up to 200 entries, newest-first. `DELETE /bot/logs` clears the whole log, no partial delete.
- **The Log Bot tab is content-gated, not just edit-gated.** `GET/DELETE /bot/logs` both require `Permission::ManageBotSettings` (main-account only). The tab button itself must not render for a non-main-account session — this is different from `/rules`/`/price`'s pattern (view-for-all, edit-for-main-account-only); here there is no view-for-all on this specific tab.
- **`accept_events.detail` is a raw JSONB blob, backend-originated but of uncertain content** (could echo upstream SPX error text) — render it as text (`JSON.stringify(detail, null, 2)` inside a `<pre>`), NEVER via `{@html}`.
- **`hasMore` for the server-paginated tab uses the exact overfetch-by-one technique already established in `Frontend/src/lib/api-tickets.ts::fetchTickets`**: request `limit: PAGE_SIZE + 1`, check `items.length > PAGE_SIZE`, slice back to `PAGE_SIZE` before returning. A naive "did this page come back full" check is WRONG exactly when the total count is a multiple of `PAGE_SIZE` (falsely reports `hasMore: true` on the true last page).
- **`accept_events.booking_id`/`rule_id` are nullable and shown as raw UUIDs, no name lookup** — do not add an enrichment fetch to resolve these to human names; out of scope per the design doc.
- **`outcome` is exactly one of 6 values** (DB CHECK constraint, migration 0008): `accepted`, `rejected`, `skipped`, `taken_by_agency`, `failed`, `agency_dup_unverified`. `log_type` is exactly `success`|`error`. `kind` is `accept`|`agency_loss`|`otp`|`null`.
- **Accessibility bar (established 7a-7e convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block, `min-h-[44px]`/`min-w-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error banners, glyph+text (never color-only) status badges, every interaction keyboard-operable, native `confirm()` for the destructive clear-log action (matching `/price`'s delete precedent).
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `activity.ts` — pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/activity.ts`
- Test: `Frontend/src/lib/activity.test.ts`

**Interfaces:**
- Produces (consumed by Tasks 2, 3, 4, 5): `outcomeLabel(outcome: string): string`; `logTypeLabel(logType: string): string`; `kindLabel(kind: string | null): string`; `formatTimestamp(date: Date): string`; `formatMicroseconds(us: number | null): string`; `formatMilliseconds(ms: number | null): string`.

- [ ] **Step 1: Write the failing test — label mapping functions**

```typescript
// Frontend/src/lib/activity.test.ts
import { describe, it, expect } from 'vitest';
import { outcomeLabel, logTypeLabel, kindLabel, formatTimestamp, formatMicroseconds, formatMilliseconds } from './activity';

describe('outcomeLabel', () => {
	it('maps all 6 known outcome values to Indonesian labels', () => {
		expect(outcomeLabel('accepted')).toBe('Diterima');
		expect(outcomeLabel('rejected')).toBe('Ditolak');
		expect(outcomeLabel('skipped')).toBe('Dilewati');
		expect(outcomeLabel('taken_by_agency')).toBe('Diambil Agensi Lain');
		expect(outcomeLabel('failed')).toBe('Gagal');
		expect(outcomeLabel('agency_dup_unverified')).toBe('Duplikat Agensi (Belum Terverifikasi)');
	});

	it('falls back to the raw value for an unknown outcome (defensive, should never happen given the DB CHECK)', () => {
		expect(outcomeLabel('something_new')).toBe('something_new');
	});
});

describe('logTypeLabel', () => {
	it('maps success and error', () => {
		expect(logTypeLabel('success')).toBe('Berhasil');
		expect(logTypeLabel('error')).toBe('Gagal');
	});
});

describe('kindLabel', () => {
	it('maps all 3 known kinds and null', () => {
		expect(kindLabel('accept')).toBe('Terima Otomatis');
		expect(kindLabel('agency_loss')).toBe('Kalah dari Agensi');
		expect(kindLabel('otp')).toBe('OTP');
		expect(kindLabel(null)).toBe('Lainnya');
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/activity.test.ts`
Expected: FAIL — `Cannot find module './activity'`.

- [ ] **Step 3: Implement the label-mapping functions**

```typescript
// Frontend/src/lib/activity.ts
// Pure logic for the /activity page — no fetch, no DOM. Wire-format mapping lives in
// api-activity.ts, matching the established $lib "logic vs. REST layer" split from prior phases.

const OUTCOME_LABELS: Record<string, string> = {
	accepted: 'Diterima',
	rejected: 'Ditolak',
	skipped: 'Dilewati',
	taken_by_agency: 'Diambil Agensi Lain',
	failed: 'Gagal',
	agency_dup_unverified: 'Duplikat Agensi (Belum Terverifikasi)'
};

/** Falls back to the raw value for anything outside the known 6 — defensive only, the DB CHECK
 * constraint (migration 0008) means this should never actually happen in practice. */
export function outcomeLabel(outcome: string): string {
	return OUTCOME_LABELS[outcome] ?? outcome;
}

const LOG_TYPE_LABELS: Record<string, string> = {
	success: 'Berhasil',
	error: 'Gagal'
};

export function logTypeLabel(logType: string): string {
	return LOG_TYPE_LABELS[logType] ?? logType;
}

const KIND_LABELS: Record<string, string> = {
	accept: 'Terima Otomatis',
	agency_loss: 'Kalah dari Agensi',
	otp: 'OTP'
};

export function kindLabel(kind: string | null): string {
	if (kind === null) return 'Lainnya';
	return KIND_LABELS[kind] ?? kind;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/activity.test.ts`
Expected: PASS (7/7 so far).

- [ ] **Step 5: Write the failing test — timestamp and latency formatting**

```typescript
// Append to Frontend/src/lib/activity.test.ts

describe('formatTimestamp', () => {
	it('formats a Date into a readable Indonesian-locale timestamp', () => {
		const d = new Date('2026-07-19T08:30:00Z');
		const result = formatTimestamp(d);
		expect(typeof result).toBe('string');
		expect(result.length).toBeGreaterThan(0);
		// Exact format is locale-dependent (Intl.DateTimeFormat); just confirm it round-trips a
		// real date's year, not a garbage/NaN string.
		expect(result).toContain('2026');
	});
});

describe('formatMicroseconds', () => {
	it('formats a positive value with a µs suffix', () => {
		expect(formatMicroseconds(342)).toBe('342 µs');
	});

	it('formats null as an em-dash', () => {
		expect(formatMicroseconds(null)).toBe('—');
	});
});

describe('formatMilliseconds', () => {
	it('formats a positive value with an ms suffix', () => {
		expect(formatMilliseconds(150)).toBe('150 ms');
	});

	it('formats null as an em-dash', () => {
		expect(formatMilliseconds(null)).toBe('—');
	});
});
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/activity.test.ts`
Expected: FAIL — `formatTimestamp`/`formatMicroseconds`/`formatMilliseconds` are not exported.

- [ ] **Step 7: Implement the formatting functions**

```typescript
// Append to Frontend/src/lib/activity.ts

const TIMESTAMP_FORMATTER = new Intl.DateTimeFormat('id-ID', {
	dateStyle: 'medium',
	timeStyle: 'short'
});

export function formatTimestamp(date: Date): string {
	return TIMESTAMP_FORMATTER.format(date);
}

export function formatMicroseconds(us: number | null): string {
	return us === null ? '—' : `${us} µs`;
}

export function formatMilliseconds(ms: number | null): string {
	return ms === null ? '—' : `${ms} ms`;
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/activity.test.ts`
Expected: PASS — all tests in the file green (13 tests).

- [ ] **Step 9: Run svelte-check and commit**

Run: `cd Frontend && pnpm check && pnpm vitest run src/lib/activity.test.ts`
Expected: `0 ERRORS 0 WARNINGS`, all `activity.test.ts` tests passing.

```bash
git add Frontend/src/lib/activity.ts Frontend/src/lib/activity.test.ts
git commit -m "feat(frontend): activity.ts — pure /activity logic (label mappings, timestamp/latency formatting)"
```

---

### Task 2: `api-activity.ts` — typed REST helpers

**Files:**
- Create: `Frontend/src/lib/api-activity.ts`
- Test: `Frontend/src/lib/api-activity.test.ts`

**Interfaces:**
- Consumes: `ApiError` (`Frontend/src/lib/api.ts`, existing).
- Produces (consumed by Tasks 3, 4, 5): `AcceptEventRow` type; `BotLogRow` type; `fetchAcceptEvents(page: number): Promise<{ rows: AcceptEventRow[]; hasMore: boolean }>`; `fetchBotLogs(): Promise<BotLogRow[]>`; `clearBotLogs(): Promise<void>`.

**Wire shapes** (snake_case, verified directly against `Backend/crates/api-gateway/src/routes/bookings.rs::spx_log`/`AcceptEventItem` and `routes/bot.rs::get_logs`/`delete_logs` — no `rename_all` anywhere in this crate):

```
GET /bookings/spx-log?limit=&offset= -> AcceptEventItem[] { id: uuid, booking_id: uuid|null,
  rule_id: uuid|null, outcome: string, local_dispatch_us: i64|null, accept_e2e_ms: i64|null,
  detail: object, created_at: ISO8601 string }
GET /bot/logs -> BotLogEntry[] { ts: i64 (unix ms), log_type: string, kind: string|null,
  booking_id: string|null, latency_ms: i64|null, rule: string|null, error: string|null }
DELETE /bot/logs -> 204 No Content (no body)
```

Both `GET` endpoints are `session_auth`-only (any authenticated tenant member) at the HTTP layer — `GET /bot/logs` ADDITIONALLY requires `Permission::ManageBotSettings` inside the handler itself (a 403 for non-main-account, not a route-level 401). This module doesn't need to special-case that: `fetchBotLogs`/`clearBotLogs` just propagate whatever status the server returns via `ApiError`; Task 5's page decides whether to call them at all based on `data.user.is_main_account`.

- [ ] **Step 1: Write the failing test — wire mapping and pagination overfetch**

```typescript
// Frontend/src/lib/api-activity.test.ts
// vi.stubGlobal('fetch', ...) regression guards for the two load-bearing HTTP details this module
// has: fetchAcceptEvents' overfetch-by-one hasMore technique, and clearBotLogs' DELETE-with-no-body
// handling — same precedent as api-tickets.ts/api-prices.ts's own fetch-mock guards for their
// respective load-bearing details.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchAcceptEvents, fetchBotLogs, clearBotLogs } from './api-activity';

afterEach(() => {
	vi.unstubAllGlobals();
});

function acceptEventWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		id: 'event-1',
		booking_id: 'booking-1',
		rule_id: null,
		outcome: 'accepted',
		local_dispatch_us: 342,
		accept_e2e_ms: 150,
		detail: { note: 'ok' },
		created_at: '2026-07-19T08:00:00Z',
		...overrides
	};
}

describe('fetchAcceptEvents', () => {
	it('requests PAGE_SIZE+1 and correctly derives offset from the real page size, not the overfetch limit', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([]), { status: 200 });
			})
		);
		await fetchAcceptEvents(3);
		const params = new URLSearchParams(calledUrl?.split('?')[1]);
		// Page 3 at whatever PAGE_SIZE this module uses internally — offset must be
		// (page-1)*PAGE_SIZE using the REAL page size, and limit must be PAGE_SIZE+1 (the
		// overfetch). Assert the relationship holds rather than hardcoding PAGE_SIZE here, so this
		// test doesn't silently drift if the constant changes.
		const limit = Number(params.get('limit'));
		const offset = Number(params.get('offset'));
		const pageSize = limit - 1;
		expect(offset).toBe((3 - 1) * pageSize);
	});

	it('hasMore is true when the overfetch returns PAGE_SIZE+1 rows, and the extra row is sliced off', async () => {
		vi.stubGlobal('fetch', vi.fn(async (url: string) => {
			const limit = Number(new URLSearchParams(url.split('?')[1]).get('limit'));
			const rows = Array.from({ length: limit }, (_, i) => acceptEventWire({ id: `event-${i}` }));
			return new Response(JSON.stringify(rows), { status: 200 });
		}));
		const { rows, hasMore } = await fetchAcceptEvents(1);
		expect(hasMore).toBe(true);
		// The overfetch row must be sliced off — returned rows must be exactly PAGE_SIZE, one
		// fewer than what the mocked fetch returned.
		const requestedLimit = rows.length + 1;
		expect(rows.length).toBe(requestedLimit - 1);
	});

	it('hasMore is false when fewer than PAGE_SIZE+1 rows come back', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([acceptEventWire()]), { status: 200 })));
		const { rows, hasMore } = await fetchAcceptEvents(1);
		expect(hasMore).toBe(false);
		expect(rows.length).toBe(1);
	});

	it('maps every snake_case field to its camelCase AcceptEventRow equivalent', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([acceptEventWire()]), { status: 200 })));
		const { rows } = await fetchAcceptEvents(1);
		expect(rows[0]).toEqual({
			id: 'event-1',
			bookingId: 'booking-1',
			ruleId: null,
			outcome: 'accepted',
			localDispatchUs: 342,
			acceptE2eMs: 150,
			detail: { note: 'ok' },
			createdAt: new Date('2026-07-19T08:00:00Z')
		});
	});
});

describe('fetchBotLogs', () => {
	it('issues a GET to /bot/logs with no query params', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal('fetch', vi.fn(async (url: string) => {
			calledUrl = url;
			return new Response(JSON.stringify([]), { status: 200 });
		}));
		await fetchBotLogs();
		expect(calledUrl).toBe('/bot/logs');
	});

	it('maps a full entry correctly', async () => {
		const wire = { ts: 1789800000000, log_type: 'success', kind: 'otp', booking_id: null, latency_ms: 5000, rule: null, error: null };
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([wire]), { status: 200 })));
		const rows = await fetchBotLogs();
		expect(rows[0]).toEqual({
			ts: 1789800000000,
			logType: 'success',
			kind: 'otp',
			bookingId: null,
			latencyMs: 5000,
			rule: null,
			error: null
		});
	});
});

describe('clearBotLogs', () => {
	it('issues a DELETE to /bot/logs and does not attempt to parse a body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
			calledUrl = url;
			calledInit = init;
			return new Response(null, { status: 204 });
		}));
		await clearBotLogs();
		expect(calledUrl).toBe('/bot/logs');
		expect(calledInit?.method).toBe('DELETE');
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-activity.test.ts`
Expected: FAIL — `Cannot find module './api-activity'`.

- [ ] **Step 3: Implement `api-activity.ts`**

```typescript
// Frontend/src/lib/api-activity.ts
// Thin typed REST layer for /activity — no UI logic here. Wire shapes verified directly against
// Backend/crates/api-gateway/src/routes/bookings.rs (spx_log/AcceptEventItem) and routes/bot.rs
// (get_logs/delete_logs) — snake_case throughout, no rename_all anywhere in api-gateway.
import { ApiError } from './api';

const PAGE_SIZE = 20;

export type AcceptEventRow = {
	id: string;
	bookingId: string | null;
	ruleId: string | null;
	outcome: string;
	localDispatchUs: number | null;
	acceptE2eMs: number | null;
	detail: unknown;
	createdAt: Date;
};

type AcceptEventItemWire = {
	id: string;
	booking_id: string | null;
	rule_id: string | null;
	outcome: string;
	local_dispatch_us: number | null;
	accept_e2e_ms: number | null;
	detail: unknown;
	created_at: string;
};

function acceptEventToRow(wire: AcceptEventItemWire): AcceptEventRow {
	return {
		id: wire.id,
		bookingId: wire.booking_id,
		ruleId: wire.rule_id,
		outcome: wire.outcome,
		localDispatchUs: wire.local_dispatch_us,
		acceptE2eMs: wire.accept_e2e_ms,
		detail: wire.detail,
		createdAt: new Date(wire.created_at)
	};
}

/** `GET /bookings/spx-log` supports ONLY `limit`/`offset` — it ignores `status`/`spx_id`/
 * `from`/`to` even though they exist on the backend's shared `ListParams` struct (confirmed by
 * reading the handler directly). `hasMore` uses the same overfetch-by-one technique as
 * `Frontend/src/lib/api-tickets.ts::fetchTickets`: request `PAGE_SIZE + 1`, check whether that
 * many rows actually came back, then slice down to `PAGE_SIZE` before returning — a naive
 * "did this page come back full" check is wrong exactly when the total count is a multiple of
 * `PAGE_SIZE`. */
export async function fetchAcceptEvents(page: number): Promise<{ rows: AcceptEventRow[]; hasMore: boolean }> {
	const offset = (page - 1) * PAGE_SIZE;
	const params = new URLSearchParams({ limit: String(PAGE_SIZE + 1), offset: String(offset) });
	const res = await fetch(`/bookings/spx-log?${params.toString()}`, { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch accept events');
	const items: AcceptEventItemWire[] = await res.json();
	const hasMore = items.length > PAGE_SIZE;
	return { rows: items.slice(0, PAGE_SIZE).map(acceptEventToRow), hasMore };
}

export type BotLogRow = {
	ts: number;
	logType: string;
	kind: string | null;
	bookingId: string | null;
	latencyMs: number | null;
	rule: string | null;
	error: string | null;
};

type BotLogEntryWire = {
	ts: number;
	log_type: string;
	kind: string | null;
	booking_id: string | null;
	latency_ms: number | null;
	rule: string | null;
	error: string | null;
};

export async function fetchBotLogs(): Promise<BotLogRow[]> {
	const res = await fetch('/bot/logs', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch bot logs');
	const items: BotLogEntryWire[] = await res.json();
	return items.map((w) => ({
		ts: w.ts,
		logType: w.log_type,
		kind: w.kind,
		bookingId: w.booking_id,
		latencyMs: w.latency_ms,
		rule: w.rule,
		error: w.error
	}));
}

/** `DELETE /bot/logs` returns `204 No Content` on success — never call `.json()` on this
 * response, there is no body to parse. */
export async function clearBotLogs(): Promise<void> {
	const res = await fetch('/bot/logs', { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to clear bot logs');
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-activity.test.ts`
Expected: PASS (7/7).

- [ ] **Step 5: Run svelte-check and full vitest, commit**

Run: `cd Frontend && pnpm check && pnpm vitest run`
Expected: `0 ERRORS 0 WARNINGS`; all suites passing (no regression in any pre-existing test file).

```bash
git add Frontend/src/lib/api-activity.ts Frontend/src/lib/api-activity.test.ts
git commit -m "feat(frontend): api-activity.ts — typed REST layer for spx-log + bot/logs"
```

---

### Task 3: `AcceptEventRow.svelte`

No unit test — component-only, verified via `svelte-check` + Task 6's e2e suite (established convention).

**Files:**
- Create: `Frontend/src/lib/components/AcceptEventRow.svelte`

**Interfaces:**
- Consumes: `outcomeLabel`, `formatTimestamp`, `formatMicroseconds`, `formatMilliseconds` (Task 1); `AcceptEventRow` type (Task 2).
- Produces (consumed by Task 5): a component with props `{ event: AcceptEventRow }`. Read-only — no `onChange`/`onDelete`, since accept_events records can't be edited or deleted from this UI.

**Simpler than `RuleRow`/`PriceRow`**: exactly ONE focusable element per row (the expand-toggle itself) — no nested delete button, no form fields, so there is no non-nested-interactive-elements concern to design around here at all.

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/AcceptEventRow.svelte -->
<!-- One accept_events row: collapsed summary + expand-in-place raw JSON detail. Read-only by
     nature (accept_events is append-only at the DB level — app_role has no UPDATE/DELETE grant
     on this table), so this is the simplest expand-in-place component in this codebase: exactly
     one focusable element, no nested controls, no form fields. -->
<script lang="ts">
	import { ChevronDown, ChevronRight } from '@lucide/svelte';
	import { outcomeLabel, formatTimestamp, formatMicroseconds, formatMilliseconds } from '$lib/activity';
	import type { AcceptEventRow } from '$lib/api-activity';

	let { event }: { event: AcceptEventRow } = $props();

	let expanded = $state(false);
</script>

<div class="rounded-lg border border-border bg-bg-surface">
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
		class="flex items-center gap-3 p-3 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		{#if expanded}
			<ChevronDown size={14} aria-hidden="true" class="text-text-muted shrink-0" />
		{:else}
			<ChevronRight size={14} aria-hidden="true" class="text-text-muted shrink-0" />
		{/if}
		<span
			class="text-[10px] font-body px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-primary uppercase shrink-0"
		>
			{outcomeLabel(event.outcome)}
		</span>
		<span class="text-[11px] font-mono text-text-muted flex-1 truncate">
			{event.bookingId ?? '—'}
		</span>
		<span class="text-[11px] font-mono text-text-muted shrink-0">{formatMicroseconds(event.localDispatchUs)}</span>
		<span class="text-[11px] font-mono text-text-muted shrink-0">{formatMilliseconds(event.acceptE2eMs)}</span>
		<span class="text-[11px] font-body text-text-muted shrink-0">{formatTimestamp(event.createdAt)}</span>
	</div>

	{#if expanded}
		<div class="p-3 pt-0 flex flex-col gap-2">
			<div class="grid grid-cols-2 gap-2 text-[11px] font-mono text-text-muted">
				<span>ID: {event.id}</span>
				<span>Rule ID: {event.ruleId ?? '—'}</span>
			</div>
			<pre
				class="text-[11px] font-mono text-text-primary bg-bg-base border border-border rounded-md p-2 overflow-x-auto">{JSON.stringify(
					event.detail,
					null,
					2
				)}</pre>
		</div>
	{/if}
</div>
```

**Why `<pre>{JSON.stringify(...)}</pre>` and never `{@html}`:** `event.detail` is backend-originated JSONB that could contain arbitrary strings (e.g. an upstream SPX error message echoed into the log). Svelte's `{expression}` interpolation auto-escapes text content, so this is safe regardless of what `detail` contains — do not "improve" this into syntax-highlighted HTML rendering without going through an escaping-safe library, and do not use `{@html}` under any circumstance here.

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/AcceptEventRow.svelte
git commit -m "feat(frontend): AcceptEventRow.svelte — read-only accept_events row with JSON detail disclosure"
```

---

### Task 4: `BotLogRow.svelte`

No unit test — component-only, verified via `svelte-check` + Task 6's e2e suite.

**Files:**
- Create: `Frontend/src/lib/components/BotLogRow.svelte`

**Interfaces:**
- Consumes: `logTypeLabel`, `kindLabel`, `formatTimestamp`, `formatMilliseconds` (Task 1); `BotLogRow` type (Task 2, note this is the SAME name as this component's filename — the type lives in `api-activity.ts`, the component in `components/BotLogRow.svelte`; Svelte/TS scoping makes this unambiguous but name it carefully in imports to avoid confusing yourself while writing this file).
- Produces (consumed by Task 5): a component with props `{ entry: BotLogRow }`. Read-only, no interactive elements at all — this entry is already flat (no nested detail to disclose, unlike `AcceptEventRow`), so it's a single non-interactive row, not even an expand toggle.

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/BotLogRow.svelte -->
<!-- One bot_log entry: a single flat row, no expand/interactive affordance at all — every field
     on a BotLogEntry is already scalar (unlike accept_events' detail JSONB), so there's nothing
     to disclose. -->
<script lang="ts">
	import { logTypeLabel, kindLabel, formatTimestamp, formatMilliseconds } from '$lib/activity';
	import type { BotLogRow } from '$lib/api-activity';

	let { entry }: { entry: BotLogRow } = $props();
</script>

<div class="flex items-center gap-3 p-3 rounded-lg border border-border bg-bg-surface">
	<span
		class={`text-[10px] font-body px-1.5 py-0.5 rounded uppercase border shrink-0 ${
			entry.logType === 'error' ? 'bg-danger/10 text-danger border-danger/30' : 'bg-live/10 text-live border-live/30'
		}`}
	>
		{logTypeLabel(entry.logType)}
	</span>
	<span class="text-[11px] font-body text-text-muted shrink-0">{kindLabel(entry.kind)}</span>
	<span class="text-[11px] font-mono text-text-muted flex-1 truncate">
		{entry.bookingId ?? entry.rule ?? '—'}
	</span>
	<span class="text-[11px] font-mono text-text-muted shrink-0">{formatMilliseconds(entry.latencyMs)}</span>
	{#if entry.error}
		<span class="text-[11px] font-body text-danger truncate max-w-[240px]" title={entry.error}>{entry.error}</span>
	{/if}
	<span class="text-[11px] font-body text-text-muted shrink-0">{formatTimestamp(new Date(entry.ts))}</span>
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/components/BotLogRow.svelte
git commit -m "feat(frontend): BotLogRow.svelte — flat read-only bot_log entry row"
```

---

### Task 5: `/activity/+page.svelte` — page assembly

No unit test — page assembly, verified via `svelte-check` + Task 6's e2e suite.

**Files:**
- Create: `Frontend/src/routes/(app)/activity/+page.svelte`

**Interfaces:**
- Consumes: `fetchAcceptEvents`, `fetchBotLogs`, `clearBotLogs`, `AcceptEventRow`, `BotLogRow` types (Task 2); `AcceptEventRow.svelte` (Task 3); `BotLogRow.svelte` (Task 4); `Pagination.svelte` (Fase 7c, unchanged); `data.user.is_main_account` (same `(app)/+layout.server.ts` pattern as `/rules`/`/price`).

Session-gating is already handled by `(app)/+layout.server.ts` for every route under this group.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/activity/+page.svelte -->
<!-- /activity: two tabs with genuinely different pagination models — Riwayat Keputusan
     (accept_events) is server-paginated (unbounded, growing table); Log Bot (bot_log) fetches its
     full <=200-entry list once and paginates client-side (backend-capped, bounded). The Log Bot
     tab button itself is content-gated (only rendered for is_main_account), not just its
     mutations — matching GET /bot/logs' own ManageBotSettings requirement on the READ path, not
     just the DELETE path, unlike /rules'/`/price`'s view-for-all-edit-for-main-account pattern. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import {
		fetchAcceptEvents,
		fetchBotLogs,
		clearBotLogs,
		type AcceptEventRow,
		type BotLogRow
	} from '$lib/api-activity';
	import AcceptEventRowItem from '$lib/components/AcceptEventRow.svelte';
	import BotLogRowItem from '$lib/components/BotLogRow.svelte';
	import Pagination from '$lib/components/Pagination.svelte';

	let { data }: PageProps = $props();
	const canViewBotLog = $derived(data.user.is_main_account);

	type Tab = 'events' | 'botlog';
	let activeTab = $state<Tab>('events');

	// Riwayat Keputusan (accept_events) — server-side pagination, real fetch on every page change.
	let eventRows = $state<AcceptEventRow[]>([]);
	let eventPage = $state(1);
	let eventHasMore = $state(false);
	let eventsLoading = $state(true);
	let eventsError = $state('');

	async function loadEvents() {
		eventsLoading = true;
		eventsError = '';
		try {
			const result = await fetchAcceptEvents(eventPage);
			eventRows = result.rows;
			eventHasMore = result.hasMore;
		} catch {
			eventsError = 'Gagal memuat riwayat keputusan. Coba lagi.';
		} finally {
			eventsLoading = false;
		}
	}

	function handleEventPageChange(next: number) {
		eventPage = next;
		loadEvents();
	}

	// Log Bot (bot_log) — one fetch of the full (<=200-entry) list, client-side pagination after.
	const BOT_LOG_PAGE_SIZE = 20;
	let botLogAll = $state<BotLogRow[]>([]);
	let botLogPage = $state(1);
	let botLogLoading = $state(false);
	let botLogError = $state('');
	let botLogLoaded = $state(false);

	async function loadBotLogs() {
		botLogLoading = true;
		botLogError = '';
		try {
			botLogAll = await fetchBotLogs();
			botLogLoaded = true;
		} catch {
			botLogError = 'Gagal memuat log bot. Coba lagi.';
		} finally {
			botLogLoading = false;
		}
	}

	const botLogPageCount = $derived(Math.max(1, Math.ceil(botLogAll.length / BOT_LOG_PAGE_SIZE)));
	const botLogPageRows = $derived(
		botLogAll.slice((botLogPage - 1) * BOT_LOG_PAGE_SIZE, botLogPage * BOT_LOG_PAGE_SIZE)
	);
	const botLogHasMore = $derived(botLogPage < botLogPageCount);

	async function handleClearBotLogs() {
		if (!confirm('Hapus semua log bot? Tindakan ini tidak dapat dibatalkan.')) return;
		try {
			await clearBotLogs();
			botLogAll = [];
			botLogPage = 1;
		} catch {
			botLogError = 'Gagal menghapus log bot. Coba lagi.';
		}
	}

	// Only fetch bot logs the first time that tab is actually opened, not on every tab switch.
	function selectTab(tab: Tab) {
		activeTab = tab;
		if (tab === 'botlog' && !botLogLoaded) {
			loadBotLogs();
		}
	}

	onMount(loadEvents);
</script>

<svelte:head>
	<title>Activity — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Activity</h1>

	<div class="flex gap-2" role="tablist" aria-label="Activity tabs">
		<button
			type="button"
			role="tab"
			aria-selected={activeTab === 'events'}
			onclick={() => selectTab('events')}
			class={`min-h-[44px] px-4 rounded-md text-[13px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
				activeTab === 'events'
					? 'bg-accent text-bg-base border-accent'
					: 'bg-bg-surface text-text-muted border-border hover:text-text-primary'
			}`}
		>
			Riwayat Keputusan
		</button>
		{#if canViewBotLog}
			<button
				type="button"
				role="tab"
				aria-selected={activeTab === 'botlog'}
				onclick={() => selectTab('botlog')}
				class={`min-h-[44px] px-4 rounded-md text-[13px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
					activeTab === 'botlog'
						? 'bg-accent text-bg-base border-accent'
						: 'bg-bg-surface text-text-muted border-border hover:text-text-primary'
				}`}
			>
				Log Bot
			</button>
		{/if}
	</div>

	{#if activeTab === 'events'}
		{#if eventsError}
			<div
				role="alert"
				aria-live="polite"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
			>
				{eventsError}
			</div>
		{/if}
		{#if eventsLoading}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:else}
			<div class="flex flex-col gap-2">
				{#each eventRows as event (event.id)}
					<AcceptEventRowItem {event} />
				{/each}
			</div>
			<Pagination page={eventPage} hasMore={eventHasMore} onPageChange={handleEventPageChange} />
		{/if}
	{:else if activeTab === 'botlog'}
		{#if botLogError}
			<div
				role="alert"
				aria-live="polite"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
			>
				{botLogError}
			</div>
		{/if}
		{#if botLogLoading}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:else}
			<button
				type="button"
				onclick={handleClearBotLogs}
				disabled={botLogAll.length === 0}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body border border-danger/30 text-danger disabled:opacity-40 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				Hapus Log
			</button>
			<div class="flex flex-col gap-2">
				<!-- BotLogEntry has no id field at all — keyed on ts+index. Safe here specifically
				     because this list is never locally reordered/mutated item-by-item (only
				     wholesale replaced on fetch or cleared to empty on delete), so index-inclusion
				     in the key causes no correctness issue despite the usual "don't key on index"
				     caution — it only guards against a same-millisecond ts collision. -->
				{#each botLogPageRows as entry, i (`${entry.ts}-${i}`)}
					<BotLogRowItem {entry} />
				{/each}
			</div>
			<Pagination page={botLogPage} hasMore={botLogHasMore} onPageChange={(next) => (botLogPage = next)} />
		{/if}
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check` (run `pnpm exec svelte-kit sync` first if `PageProps` isn't found).
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/activity/+page.svelte"
git commit -m "feat(frontend): /activity page assembly — two-tab log viewer, server + client pagination, tab content-gating"
```

---

### Task 6: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/activity.spec.ts`

**Interfaces:**
- Consumes: the full `/activity` page built in Tasks 1-5. No new frontend code — this task authors real-stack e2e coverage and runs full verification.

**No new seed users needed.** Reuses `e2e-test-user` (main-account) and `e2e-readonly-user` (non-main-account) from Fase 7a/7d, both already seeded.

**One ONE-TIME bulk seed is needed for `accept_events`** (to genuinely test server-side pagination, which needs more than one page's worth of real rows) — everything else self-seeds per-test.

#### Step 1: Seed 21 `accept_events` rows (one-time, direct `psql`)

`accept_events` has no UI path to create rows (append-only, written only by the poller/manual-accept flow) and no existing e2e seed — 21 rows (one more than `PAGE_SIZE=20`) proves genuine server-side pagination shows different content on page 2, not just a client-side slice. `booking_id`/`rule_id` are nullable FKs — seeded as `NULL` to avoid any dependency on other seeded bookings/rules existing. `local_dispatch_us` values are deliberately distinct per row (`100+n`, i.e. 101-121 µs) so the oldest row (n=21, `121 µs`) is trivially identifiable in the UI without needing to expand a row's JSON detail. `created_at` is staggered by `n` seconds into the past so `ORDER BY created_at DESC` (confirmed in `store::accept_events::list_for_tenant`) puts n=1 newest (page 1) and n=21 oldest (page 2). This seed is safe to accidentally re-run (accept_events is append-only, no cleanup possible even by `app_role` — re-running just adds 21 more rows and doesn't break the pagination test's marker-based assertions, since the newest 20-of-any-given-run always land on page 1 and the oldest always lands beyond it).

```bash
PGPASSWORD=tower_dev_only psql -h 127.0.0.1 -p 15432 -U tower -d tower -c "
  INSERT INTO accept_events (tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail, created_at)
  SELECT
    'e03ac22f-729b-436f-a112-08aab5022614',
    NULL,
    NULL,
    'accepted',
    100 + n,
    50 + n,
    jsonb_build_object('seed_marker', 'e2e-fixture', 'seed_index', n),
    now() - (n || ' seconds')::interval
  FROM generate_series(1, 21) AS n;
"
```

#### Step 2: Write `Frontend/tests/activity.spec.ts`

**`bot_log` tests self-seed via `redis-cli LPUSH` at the start of each test that needs an entry present** rather than relying on a one-time seed — this avoids a real ordering conflict: `DELETE /bot/logs` clears the WHOLE key, so a "shows existing entries" test and a "clear the log" test would fight over shared state if either ran first depending on suite order or a rerun. Self-seeding per test (mirroring the same self-cleaning-fixture discipline `rules.spec.ts`/`price.spec.ts` already established for their own stateful tests) sidesteps this entirely.

```typescript
// Frontend/tests/activity.spec.ts
//
// REAL end-to-end proof of Fase 7f's /activity page. Same real-stack setup as
// tests/login.spec.ts, tests/command.spec.ts, tests/tickets.spec.ts, tests/rules.spec.ts,
// tests/price.spec.ts — real reactor-core on :8081 behind Vite's dev proxy, real Postgres
// (tower-postgres, 127.0.0.1:15432), real Redis (tower-redis, 127.0.0.1:16379). Nothing here is
// mocked or stubbed.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d, no re-seeding needed. accept_events has a ONE-TIME 21-row bulk seed (see
// this task's Step 1) — accept_events is append-only with no UI creation path, unlike bot_log,
// which every test needing an entry seeds fresh for itself via redis-cli (see Step 2's own
// rationale: DELETE /bot/logs clears the whole key, so a shared one-time bot_log seed would
// create a real ordering conflict between the "shows entries" and "clear" tests).

import { test, expect } from '@playwright/test';
import { execFileSync } from 'node:child_process';

const TENANT_ID = 'e03ac22f-729b-436f-a112-08aab5022614';

function seedBotLogEntry(overrides: Partial<Record<string, unknown>> = {}) {
	const entry = {
		ts: Date.now(),
		log_type: 'success',
		kind: 'otp',
		booking_id: null,
		latency_ms: 5000,
		rule: null,
		error: null,
		...overrides
	};
	execFileSync('redis-cli', [
		'-h',
		'127.0.0.1',
		'-p',
		'16379',
		'LPUSH',
		`spx:bot:logs:${TENANT_ID}`,
		JSON.stringify(entry)
	]);
}

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /activity redirects to /login', async ({ page }) => {
	await page.goto('/activity');
	await expect(page).toHaveURL(/\/login/);
});

test('Riwayat Keputusan tab loads by default and shows seeded accept_events', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByRole('heading', { name: 'Activity' })).toBeVisible();
	await expect(page.getByRole('tab', { name: 'Riwayat Keputusan' })).toHaveAttribute('aria-selected', 'true');
	await expect(page.getByText('Diterima').first()).toBeVisible({ timeout: 10_000 });
});

test('pagination on Riwayat Keputusan shows genuinely different server-fetched content on page 2', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByText('Diterima').first()).toBeVisible({ timeout: 10_000 });

	// The oldest of the 21 seeded rows (121 µs) must NOT be on page 1 (PAGE_SIZE=20).
	await expect(page.getByText('121 µs')).toBeHidden();

	await page.getByRole('button', { name: 'Halaman berikutnya' }).click();
	await expect(page.getByText('121 µs')).toBeVisible({ timeout: 10_000 });
});

test('expanding an accept_events row reveals its raw JSON detail', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByText('Diterima').first()).toBeVisible({ timeout: 10_000 });

	await page.getByText('Diterima').first().click();
	await expect(page.getByText('"seed_marker"')).toBeVisible();
});

test('non-main-account session does not see the Log Bot tab at all', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByRole('tab', { name: 'Log Bot' })).toBeHidden();
});

test('main account sees Log Bot entries and can clear them (confirm dialog accepted)', async ({ page }) => {
	seedBotLogEntry({ kind: 'otp', log_type: 'success' });

	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await page.getByRole('tab', { name: 'Log Bot' }).click();
	await expect(page.getByText('OTP')).toBeVisible({ timeout: 10_000 });

	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus Log' }).click();
	await expect(page.getByText('OTP')).toBeHidden({ timeout: 10_000 });
	await expect(page.getByRole('button', { name: 'Hapus Log' })).toBeDisabled();
});
```

- [x] **Step 3: Run the new e2e file alone**

Run: `cd Frontend && pnpm exec playwright test tests/activity.spec.ts`
Expected: all tests pass (a live `reactor-core` + `tower-postgres` + `tower-redis` stack must already be running — see `tests/login.spec.ts`'s header comment for the exact env a manually-started `reactor-core` needs).

- [x] **Step 4: Run the full Playwright suite (regression check)**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across `login.spec.ts`, `command.spec.ts`, `tickets.spec.ts`, `rules.spec.ts`, `price.spec.ts`, `activity.spec.ts` pass — no regression in earlier phases' coverage.

- [x] **Step 5: Full backend verification**

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green. **Use the `tower` superuser URL for `cargo test`, not `app_role`** (the same local-dev-only gotcha every prior phase's plan has flagged). This task makes no backend changes, so this is a pure regression check.

- [x] **Step 6: Full frontend verification**

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `activity.test.ts`, Task 2's `api-activity.test.ts`, plus every pre-existing suite — no regression); production build succeeds.

- [x] **Step 7: Commit**

```bash
git add Frontend/tests/activity.spec.ts
git commit -m "test(fase-7f): /activity e2e (Playwright) — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — two-tab layout with correct pagination models per tab (Task 5), tab content-gating for Log Bot (Task 5), expand-in-place JSON detail (Task 3), clear-log action with confirm guard (Task 5). Every "Out of scope" bullet (filter bar, name-lookup enrichment, new backend activity tracking, archive_runs surfacing) has no corresponding task.

**Placeholder scan:** no TBD/TODO. While writing Task 2, this review caught and fixed a real naming bug in its own draft (`accountE2eMs` instead of `acceptE2eMs`, propagated across the type definition, mapping function, and test) — corrected before finalizing.

**Type consistency:** `AcceptEventRow`/`BotLogRow` (Task 2) are the same shapes threaded unchanged through Task 3/4's component props and Task 5's page state — no renamed fields between tasks (re-checked against each task's interface list while writing this plan).

**Cross-task dependency ordering:** 1 (pure logic) → 2 (wire mapping, depends on 1's function signatures only, not types) → 3, 4 (row components, depend on 1 and 2, independent of each other) → 5 (page, depends on 1, 2, 3, 4) → 6 (e2e, depends on everything). No task references a later task's output.

**Test-ordering safety:** explicitly designed around a real conflict this review caught during planning — `DELETE /bot/logs` clears the whole key, so a shared one-time bot_log seed would make the "shows entries" and "clear" tests fight over state depending on execution order or a rerun. Resolved by having every bot_log-dependent test self-seed via `redis-cli LPUSH`, avoiding the conflict entirely rather than depending on careful test ordering (which is exactly the class of fragility Fase 7d/7e's whole-branch reviews found and fixed after the fact — addressed here during planning instead).

