# Fase 7b (nav shell + /command) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the authenticated app shell (top-bar nav) and its default landing surface `/command`, with two live signature components (Latency Tape, Live Ticket Ticker with optimistic manual-accept), backed by two small, additive backend changes (route field exposure, `local_dispatch_us` decision-path instrumentation).

**Architecture:** A new SvelteKit route group `(app)` carries a session-gated layout (server-side check via `GET /auth/me`) with a horizontal top-bar nav. A single WS store (`$lib/ws.svelte.ts`, Svelte 5 runes) owns one `WebSocket` connection to `/ws` (cookie-authenticated, no query param — the first real consumer of Fase 7a's WS-auth-cookie fix) with reconnect/backoff, exposing connection state and a typed event stream. `/command` composes `LatencyTape` (canvas, fed by `localDispatchUs` off `ticket_accepted` events) and `TicketTicker` (fed by an initial `GET /bookings/live` fetch, periodic 20s re-poll for new arrivals, and real-time `ticket_accepted`/`ticket_rejected`/`tickets_removed` WS events, merged through one pure delta-merge function in `$lib/ticker.ts` that both the initial fetch and every subsequent update path call).

**Tech Stack:** Same as Fase 7a (SvelteKit 2.69.2, Svelte 5.56.4 runes, Tailwind v4, adapter-node) — zero new frontend dependencies. Backend: `Backend/crates/poller/src/dispatch.rs` (hot-path instrumentation, already-reviewed code — treat with the same scrutiny this project applies to every prior touch of this file) and `Backend/crates/api-gateway/src/routes/bookings.rs` (additive response field).

## Global Constraints

- Every color/font/radius in new `.svelte`/`.ts` files MUST come from Fase 7a's `--color-*`/`--font-*`/`--radius-*` tokens (Tailwind utility or `var(--...)`) — no raw hex. Semantic assignment already fixed in 7a: **teal (`--color-live`) = data/live status, amber (`--color-accent`) = action/warning**.
- `local_dispatch_us` measurement boundary (see design doc's own correction): starts at `dispatch_booking`'s entry, ends the instant `st.dedup.try_begin_accept(&booking.id)` returns `true` (end of Layer 1, in-proc DashMap CAS) — **excludes** the Layer 2 Redis gate and the HTTP accept call. Do not move this boundary without re-reading the design doc's reasoning.
- `new_tickets` WS event is **NOT** wired in this plan (a real, already-disclosed Fase-5 gap, out of scope) — `/command` uses a 20s poll of `GET /bookings/live` as the fallback for detecting new pending tickets, merged through the SAME delta-merge function real-time WS events use.
- `route: Vec<String>` is a new additive field on `BookingListItem` (REST) and the `ticket_accepted` WS payload — same field name in both places, sourced from `spx_client::normalize_booking(&raw_data).route_stops`, never invented or renamed.
- `ManualAcceptResponse` (backend, `{ok: bool, reason: string, message: string}`) is the exact shape `POST /bookings/:id/accept` already returns (Fase 6c/6e) — the frontend's optimistic-accept flow must not invent a different shape.
- `cargo fmt`/`cargo clippy --workspace --all-targets -- -D warnings`/`cargo test --workspace -- --test-threads=1` clean after every backend-touching task. `pnpm check` clean after every frontend-touching task.
- WCAG 2.2 AA: every new interactive element gets a focus-visible ring, `min-h-[44px]`/`min-w-[44px]` tap targets, Health Pill and "+N new" badge use `aria-live`, Latency Tape's animation respects `prefers-reduced-motion` (already a global CSS rule from 7a — this task must not bypass it with inline/JS-driven animation that ignores the media query).

---

## Task 1: Backend — expose `route` field (REST + WS)

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs`
- Modify: `Backend/crates/poller/src/dispatch.rs`

**Interfaces:**
- Produces (for Task 6/7/8): `BookingListItem.route: Vec<String>` (JSON key `route`), and the `ticket_accepted` WS payload's `"route"` field — same shape, same field name.

- [ ] **Step 1: Add `route` to `BookingListItem` and its `From` impl**

Read `Backend/crates/api-gateway/src/routes/bookings.rs`'s current `BookingListItem` struct and `From<store::models::Booking>` impl in full first (both shown in this task's research, but confirm nothing shifted). Then:

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — modify BookingListItem
#[derive(Debug, Serialize)]
pub struct BookingListItem {
    pub id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    /// SPX route stop names, origin-first. Sourced from `raw_data` via
    /// `spx_client::normalize_booking` (Fase 7b) — not stored as its own DB column, the raw
    /// JSONB blob is the source of truth, matching how `routes/bookings.rs::accept` already
    /// derives `SpxBooking` from `raw_data` for the manual-accept path.
    pub route: Vec<String>,
}

impl From<store::models::Booking> for BookingListItem {
    fn from(b: store::models::Booking) -> Self {
        let route = spx_client::normalize_booking(&b.raw_data).route_stops;
        Self {
            id: b.id,
            account_id: b.account_id,
            spx_id: b.spx_id,
            status: b.status,
            service_type: b.service_type,
            weight: b.weight,
            cod_amount: b.cod_amount,
            auto_accepted: b.auto_accepted,
            rule_matched: b.rule_matched,
            created_at: b.created_at,
            route,
        }
    }
}
```

- [ ] **Step 2: Add `route` to the `ticket_accepted` WS payload**

Read `Backend/crates/poller/src/dispatch.rs`'s current `finalize_win` function in full (already summarized in this plan's research — the `pub_.publish_ticket_accepted(...)` call with its `serde_json::json!({...})` literal). Modify just that literal:

```rust
// Backend/crates/poller/src/dispatch.rs — inside finalize_win, modify the publish_ticket_accepted call
pub_.publish_ticket_accepted(
    &st.account_id,
    serde_json::json!({
        "bookingId": booking.booking_id,
        "latencyMs": latency_ms,
        "autoAccept": true,
        "rule": meta.name,
        "route": booking.route_stops,
    }),
)
.await;
```

(`booking: &SpxBooking` is already `finalize_win`'s parameter — `booking.route_stops` is directly available, no new lookup needed; this is the SAME `SpxBooking` type Task 1's REST change derives via `normalize_booking`, just already in hand here.)

- [ ] **Step 3: Write the failing test**

Read `Backend/crates/api-gateway/tests/bookings_routes.rs`'s existing `GET /bookings/live` test (search for a test hitting that route) for its exact setup pattern (tenant/booking seeding), then add an assertion to it (extend, don't duplicate the whole test):

```rust
// Backend/crates/api-gateway/tests/bookings_routes.rs — add to the existing live-list test,
// after its current assertions, using whatever `raw_data` JSON that test already seeds (if it
// seeds a booking with real SPX-shaped raw_data containing route info) OR seed a fresh booking
// row with a minimal real SPX route_detail_list-shaped raw_data blob if the existing test's seed
// is empty/synthetic. Check `spx_client::normalize_booking`'s real parsing logic (crates/spx-client/src/booking.rs) to know the exact raw_data shape route_stops parses from — do not guess the
// JSON shape, read the parser.
let body: serde_json::Value = /* ...existing response parse... */;
let route = body["items"][0]["route"].as_array().expect("route field present");
assert!(!route.is_empty(), "route must be populated from raw_data, not an empty default");
```

- [ ] **Step 4: Run it, then full workspace verification**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && unset DATABASE_URL && export REDIS_URL="redis://127.0.0.1:16379" && cargo test -p api-gateway --test bookings_routes -- --test-threads=1` — PASS.
Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — 0 failures, clean (this touches `dispatch.rs`, already-reviewed hot-path code — full workspace scope is right here, matching this project's established convention for any `dispatch.rs` touch).

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/poller/src/dispatch.rs \
        Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(api-gateway,poller): expose route field on BookingListItem + ticket_accepted payload"
```

---

## Task 2: Backend — instrument `local_dispatch_us`

**Files:**
- Modify: `Backend/crates/poller/src/dispatch.rs`

**Interfaces:**
- Produces (for Task 7): the `ticket_accepted` WS payload gains `"localDispatchUs": <u64>` (microseconds).

This is the single highest-risk task in this plan — it touches `dispatch_booking`, the real accept hot path, already reviewed at the highest bar in Fase 6c (opus review, "concurrency change to the poller's core select! hot loop"). Read `dispatch_booking`'s ENTIRE current body (not just the excerpt in this brief) before editing, and `finalize_win`'s entire current body, before making any change.

- [ ] **Step 1: Add the measurement in `dispatch_booking`**

```rust
// Backend/crates/poller/src/dispatch.rs — modify dispatch_booking's opening
pub async fn dispatch_booking(
    shared: &PollerShared,
    st: &mut PollerState,
    booking: &SpxBooking,
) -> DispatchResult {
    let decision_started = std::time::Instant::now();

    // 1. Match against compiled rules (first-wins index).
    let core = to_core_booking(booking);
    let idx = match find_best_matching_rule_compiled(&st.rules, &core, &st.match_state) {
        Some(i) => i,
        None => return DispatchResult::Skipped,
    };
    let meta = st.rule_meta[idx].clone();

    // 2. Layer 1 in-proc claim.
    if !st.dedup.try_begin_accept(&booking.id) {
        return DispatchResult::Skipped;
    }
    // Decision-path latency (master spec's "≤1ms p99" headline metric) ends HERE — Layer 1
    // in-proc claim just succeeded. Everything after this point (Layer 2 Redis gate, HTTP
    // dispatch) is deliberately EXCLUDED per the master spec's own hot-path description and
    // Fase 7b's design doc correction — do not move this line without re-reading that reasoning.
    let local_dispatch_us: u64 = decision_started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);

    // 3. Layer 2 durable atomic gate (fail-closed).
    match shared
        .executor
        .try_claim_auto(
            &st.account_id,
            &booking.id,
            Some(meta.uuid),
            meta.cap,
            meta.accepted_count,
        )
        .await
    {
        ClaimOutcome::Proceed => {}
```

Continue reading the REAL current function body from this point on (this brief does not reproduce the rest — the `ClaimOutcome` match arms, the HTTP dispatch, the `AcceptReason` match, and both `finalize_win(...)` call sites) and thread `local_dispatch_us` as a new argument into BOTH existing `finalize_win(shared, st, booking, &meta, latency_ms)` call sites, becoming `finalize_win(shared, st, booking, &meta, latency_ms, local_dispatch_us)`.

- [ ] **Step 2: Thread it through `finalize_win`**

```rust
// Backend/crates/poller/src/dispatch.rs — modify finalize_win's signature and its
// publish_ticket_accepted call (Task 1 already added the "route" field to this same json! call
// — add "localDispatchUs" alongside it, do not remove Task 1's change)
async fn finalize_win(
    shared: &PollerShared,
    st: &mut PollerState,
    booking: &SpxBooking,
    meta: &RuleMeta,
    latency_ms: i32,
    local_dispatch_us: u64,
) {
    // ... existing body unchanged up through the dedup/executor/store calls ...

    if let Some(pub_) = &shared.redis {
        pub_.publish_ticket_accepted(
            &st.account_id,
            serde_json::json!({
                "bookingId": booking.booking_id,
                "latencyMs": latency_ms,
                "autoAccept": true,
                "rule": meta.name,
                "route": booking.route_stops,
                "localDispatchUs": local_dispatch_us,
            }),
        )
        .await;
        // ... existing record_bot_log call unchanged ...
    }
}
```

- [ ] **Step 3: Write the failing test**

Read `Backend/crates/poller/tests/dispatch_pipeline.rs` (or whichever existing test file exercises `dispatch_booking`'s win path against a real wiremock SPX server — check the file list, this project's established pattern per prior sub-phases' ledgers) to find its exact setup helpers. Add an assertion that a real dispatched-and-won booking's published WS payload (read back via the test's own Redis pub/sub subscription helper, if one already exists in that file — reuse it) contains a `localDispatchUs` field that is:
1. Present (not missing/null).
2. A small positive number consistent with an in-memory-only operation — assert it's under some generous ceiling proving it did NOT include network I/O, e.g. `< 50_000` (50ms in microseconds — deliberately generous to avoid test flakiness on a loaded CI machine, while still being orders of magnitude below what a Redis round-trip or HTTP call would add if the measurement boundary regressed).

```rust
// Illustrative shape — match this file's REAL existing test helpers exactly, read them first:
let published = /* ...this file's existing helper for reading back the ticket_accepted payload... */;
let local_dispatch_us = published["localDispatchUs"].as_u64().expect("localDispatchUs present");
assert!(local_dispatch_us < 50_000, "local_dispatch_us={local_dispatch_us} looks like it included network I/O, not just in-proc claim");
```

- [ ] **Step 4: Run it, then full workspace verification**

Run: `cargo test -p poller -- --test-threads=1` — all tests pass, including the new assertion.
Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — 0 failures, clean. Given this task changes `finalize_win`'s signature (2 call sites, both in the same file) and the timing-critical measurement boundary, re-read the full diff once before committing and confirm by inspection that `decision_started` is captured BEFORE step 1 (rule matching) and read ONLY after step 2 (Layer 1 claim) succeeds — not after `return DispatchResult::Skipped` on either early-exit path (those paths correctly never reach the `local_dispatch_us` line at all, by construction, since it's it's placed after both early returns).

- [ ] **Step 5: Commit**

```bash
git add Backend/crates/poller/src/dispatch.rs Backend/crates/poller/tests/dispatch_pipeline.rs
git commit -m "feat(poller): instrument local_dispatch_us — decision-path latency (Layer 1 claim only, excludes Redis gate + HTTP)"
```

---

## Task 3: Frontend — WS store

**Files:**
- Create: `Frontend/src/lib/ws.svelte.ts`
- Test: `Frontend/src/lib/ws.svelte.test.ts` (if a unit-test runner exists — see Step 1's note; otherwise covered by Task 9's e2e suite only, disclosed)

**Interfaces:**
- Produces (for Task 4/6/7/8): `createWsStore(): WsStore` where `WsStore` exposes `status: 'connecting' | 'connected' | 'reconnecting' | 'disconnected'` (a `$state`-backed reactive field) and `onEvent(handler: (event: TowerWsEvent) => void): () => void` (subscribe, returns an unsubscribe fn). `TowerWsEvent` is a discriminated union matching the backend's real `WsEvent` shape for the variants this app cares about.

- [ ] **Step 1: Check for an existing unit-test runner**

```bash
cd Frontend && cat package.json | grep -i vitest
```
This project has never run a frontend unit test before (Fase 7a had none — only Playwright e2e). If `vitest` isn't present, add it now (needed for Task 5's pure delta-merge logic too, per the design doc's explicit requirement that merge/optimistic logic be "diuji terpisah" — tested separately from components):

```bash
pnpm add -D vitest
```
Add to `Frontend/package.json`'s `scripts`: `"test:unit": "vitest run"`.

- [ ] **Step 2: Write the WS store**

```typescript
// Frontend/src/lib/ws.svelte.ts
// Single shared WebSocket connection to /ws — cookie-authenticated (Fase 7a Task 4's
// ws-hub cookie fix), no ?session= needed; the browser attaches the HttpOnly session cookie
// automatically on same-origin WS handshakes (via Caddy in prod, Vite's proxy in dev — both
// already route /ws to reactor-core, Fase 7a Tasks 2/3).
export type WsStatus = 'connecting' | 'connected' | 'reconnecting' | 'disconnected';

// Matches Backend/crates/ws-hub/src/events.rs's WsEvent enum shape exactly
// (#[serde(tag="type", content="data")]) — ONLY the variants this app actually consumes today
// are typed; unknown "type" values are ignored (forward-compatible with new backend event
// variants this frontend doesn't know about yet).
export type TicketAcceptedData = {
	bookingId: string;
	latencyMs: number;
	autoAccept: boolean;
	rule: string;
	route: string[];
	localDispatchUs: number;
};
export type TowerWsEvent =
	| { type: 'connected'; data: { session: string } }
	| { type: 'ticket_accepted'; data: TicketAcceptedData }
	| { type: 'ticket_rejected'; data: { bookingId: string } }
	| { type: 'tickets_removed'; data: { ids: string[] } };

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15000;

export function createWsStore() {
	let status = $state<WsStatus>('connecting');
	let socket: WebSocket | null = null;
	let reconnectAttempt = 0;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let closedByUs = false;
	const handlers = new Set<(event: TowerWsEvent) => void>();

	function scheduleReconnect() {
		if (closedByUs) return;
		status = 'reconnecting';
		const delay = Math.min(RECONNECT_BASE_MS * 2 ** reconnectAttempt, RECONNECT_MAX_MS);
		reconnectAttempt += 1;
		reconnectTimer = setTimeout(connect, delay);
	}

	function connect() {
		const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
		socket = new WebSocket(`${proto}//${location.host}/ws`);
		socket.addEventListener('open', () => {
			status = 'connected';
			reconnectAttempt = 0;
		});
		socket.addEventListener('message', (ev) => {
			let parsed: TowerWsEvent;
			try {
				parsed = JSON.parse(ev.data);
			} catch {
				return;
			}
			for (const h of handlers) h(parsed);
		});
		socket.addEventListener('close', () => {
			if (closedByUs) {
				status = 'disconnected';
				return;
			}
			scheduleReconnect();
		});
		socket.addEventListener('error', () => {
			socket?.close();
		});
	}

	connect();

	return {
		get status() {
			return status;
		},
		onEvent(handler: (event: TowerWsEvent) => void) {
			handlers.add(handler);
			return () => handlers.delete(handler);
		},
		close() {
			closedByUs = true;
			if (reconnectTimer) clearTimeout(reconnectTimer);
			socket?.close();
		}
	};
}

export type WsStore = ReturnType<typeof createWsStore>;
```

- [ ] **Step 3: Write a unit test for the reconnect backoff math**

```typescript
// Frontend/src/lib/ws.svelte.test.ts
// Full WebSocket lifecycle needs a browser/e2e context (Task 9 covers that) — this test
// isolates just the backoff CALCULATION (deterministic, pure), the one piece of ws.svelte.ts
// worth a fast unit test on its own.
import { describe, it, expect } from 'vitest';

function backoffDelay(attempt: number, base = 1000, max = 15000): number {
	return Math.min(base * 2 ** attempt, max);
}

describe('reconnect backoff', () => {
	it('doubles each attempt starting from the base delay', () => {
		expect(backoffDelay(0)).toBe(1000);
		expect(backoffDelay(1)).toBe(2000);
		expect(backoffDelay(2)).toBe(4000);
		expect(backoffDelay(3)).toBe(8000);
	});

	it('caps at the max delay instead of growing unbounded', () => {
		expect(backoffDelay(10)).toBe(15000);
		expect(backoffDelay(100)).toBe(15000);
	});
});
```

- [ ] **Step 4: Run it**

```bash
cd Frontend && pnpm vitest run src/lib/ws.svelte.test.ts
```
Expected: 2 passed.

- [ ] **Step 5: Run `pnpm check`, then commit**

```bash
pnpm check
```
Expected: 0 errors.

```bash
git add Frontend/package.json Frontend/pnpm-lock.yaml Frontend/src/lib/ws.svelte.ts Frontend/src/lib/ws.svelte.test.ts
git commit -m "feat(frontend): WS store — cookie-authenticated /ws connection with reconnect backoff"
```

---

## Task 4: Frontend — nav shell + session-gated layout

**Files:**
- Create: `Frontend/src/routes/(app)/+layout.svelte`
- Create: `Frontend/src/routes/(app)/+layout.server.ts`
- Create: `Frontend/src/lib/components/TopNav.svelte`
- Create: `Frontend/src/lib/components/HealthPill.svelte`

**Interfaces:**
- Consumes: Task 3's `createWsStore()`.
- Produces (for Task 8): the `(app)` route group — `/command` (Task 8) mounts inside it as `Frontend/src/routes/(app)/command/+page.svelte`.

- [ ] **Step 1: Session check in `+layout.server.ts`**

```typescript
// Frontend/src/routes/(app)/+layout.server.ts
import { redirect } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

export const load: LayoutServerLoad = async ({ fetch, cookies }) => {
	if (!cookies.get('spx_session')) {
		redirect(307, '/login');
	}
	const res = await fetch('/auth/me');
	if (!res.ok) {
		redirect(307, '/login');
	}
	const user = await res.json();
	return { user };
};
```

(The `cookies.get(...)` pre-check avoids an unnecessary `fetch` round-trip for the common no-session case; the `fetch('/auth/me')` call is the real authoritative check — SvelteKit's server-side `fetch` in a `load` function forwards the incoming request's cookies automatically to same-origin requests, no manual header-copying needed.)

- [ ] **Step 2: `TopNav.svelte`**

```svelte
<!-- Frontend/src/lib/components/TopNav.svelte -->
<script lang="ts">
	import { page } from '$app/state';
	import HealthPill from './HealthPill.svelte';
	import type { WsStatus } from '$lib/ws.svelte';

	let { wsStatus }: { wsStatus: WsStatus } = $props();

	const NAV_ITEMS = [
		{ href: '/command', label: 'Command' },
		{ href: '/tickets', label: 'Tickets' },
		{ href: '/rules', label: 'Rules' },
		{ href: '/price', label: 'Price' },
		{ href: '/settings', label: 'Settings' },
		{ href: '/activity', label: 'Activity' }
	];
</script>

<nav
	class="h-12 border-b border-border bg-bg-surface flex items-center px-4 gap-5 overflow-x-auto"
	aria-label="Navigasi utama"
>
	<span class="font-heading font-bold text-text-primary text-sm shrink-0">TOWER</span>
	<ul class="flex gap-4 text-xs font-body shrink-0">
		{#each NAV_ITEMS as item (item.href)}
			<li>
				<a
					href={item.href}
					class="inline-block py-3.5 border-b-2 transition-colors min-h-[44px] flex items-center focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent
						{page.url.pathname.startsWith(item.href)
						? 'border-accent text-accent'
						: 'border-transparent text-text-muted hover:text-text-primary'}"
					aria-current={page.url.pathname.startsWith(item.href) ? 'page' : undefined}
				>
					{item.label}
				</a>
			</li>
		{/each}
	</ul>
	<div class="ml-auto flex items-center gap-3 shrink-0">
		<HealthPill status={wsStatus} />
		<button
			type="button"
			aria-label="Notifikasi"
			class="w-9 h-9 min-w-[44px] min-h-[44px] flex items-center justify-center rounded-lg text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			<span aria-hidden="true">&#128276;</span>
		</button>
	</div>
</nav>
```

- [ ] **Step 3: `HealthPill.svelte`**

```svelte
<!-- Frontend/src/lib/components/HealthPill.svelte -->
<script lang="ts">
	import type { WsStatus } from '$lib/ws.svelte';

	let { status }: { status: WsStatus } = $props();

	const CONFIG: Record<WsStatus, { glyph: string; label: string; colorClass: string }> = {
		connected: { glyph: '●', label: 'LIVE', colorClass: 'text-live' },
		connecting: { glyph: '◐', label: 'MENYAMBUNG', colorClass: 'text-accent' },
		reconnecting: { glyph: '◐', label: 'RECONNECTING', colorClass: 'text-accent' },
		disconnected: { glyph: '○', label: 'TERPUTUS', colorClass: 'text-danger' }
	};
	const cfg = $derived(CONFIG[status]);
</script>

<span
	class="inline-flex items-center gap-1.5 text-[10px] font-mono font-semibold {cfg.colorClass}"
	aria-live="polite"
>
	<span aria-hidden="true">{cfg.glyph}</span>
	{cfg.label}
</span>
```

- [ ] **Step 4: `(app)/+layout.svelte`**

```svelte
<!-- Frontend/src/routes/(app)/+layout.svelte -->
<script lang="ts">
	import TopNav from '$lib/components/TopNav.svelte';
	import { createWsStore } from '$lib/ws.svelte';
	import { setContext } from 'svelte';

	let { children } = $props();

	const ws = createWsStore();
	setContext('ws', ws);
</script>

<div class="min-h-screen bg-bg-base">
	<TopNav wsStatus={ws.status} />
	<main>
		{@render children()}
	</main>
</div>
```

(`setContext('ws', ws)` makes the single shared WS connection available to `/command` and every future page in this route group via `getContext('ws')` — one connection for the whole authenticated app, not one per page.)

- [ ] **Step 5: Run `pnpm check`, then commit**

```bash
cd Frontend && pnpm check
```
Expected: 0 errors (note: `/tickets`, `/rules`, `/price`, `/settings`, `/activity` links will 404 until their own sub-fases — this is disclosed, expected, matching the same pattern as `/command`'s own pre-Task-8 404).

```bash
git add Frontend/src/routes/\(app\)/+layout.svelte Frontend/src/routes/\(app\)/+layout.server.ts \
        Frontend/src/lib/components/TopNav.svelte Frontend/src/lib/components/HealthPill.svelte
git commit -m "feat(frontend): nav shell — session-gated (app) route group, top bar, Health Pill"
```

---

## Task 5: Frontend — ticker delta-merge logic (pure, tested)

**Files:**
- Create: `Frontend/src/lib/ticker.ts`
- Test: `Frontend/src/lib/ticker.test.ts`

**Interfaces:**
- Produces (for Task 6): `type TicketRow`, `mergeNewTickets(rows, incoming)`, `applyAccepted(rows, data)`, `applyRejected(rows, bookingId)`, `applyRemoved(rows, ids)` — all pure functions, `(rows: TicketRow[], ...) => TicketRow[]`, never mutate their input array.

- [ ] **Step 1: Write the failing tests**

```typescript
// Frontend/src/lib/ticker.test.ts
import { describe, it, expect } from 'vitest';
import { mergeNewTickets, applyAccepted, applyRejected, applyRemoved, type TicketRow } from './ticker';

function row(overrides: Partial<TicketRow> = {}): TicketRow {
	return {
		spxId: 'SPX1',
		status: 'pending',
		route: ['Jakarta', 'Bandung'],
		latencyMs: null,
		localDispatchUs: null,
		accepting: false,
		...overrides
	};
}

describe('mergeNewTickets', () => {
	it('prepends genuinely new rows (newest-first) and skips ones already present by spxId', () => {
		const existing = [row({ spxId: 'SPX1' })];
		const incoming = [row({ spxId: 'SPX2' }), row({ spxId: 'SPX1' })];
		const result = mergeNewTickets(existing, incoming);
		expect(result.map((r) => r.spxId)).toEqual(['SPX2', 'SPX1']);
	});

	it('does not mutate the input array', () => {
		const existing = [row({ spxId: 'SPX1' })];
		mergeNewTickets(existing, [row({ spxId: 'SPX2' })]);
		expect(existing).toHaveLength(1);
	});
});

describe('applyAccepted', () => {
	it('updates the matching row to accepted with latency + dispatch metrics', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2' })];
		const result = applyAccepted(rows, {
			bookingId: 'SPX1',
			latencyMs: 312,
			autoAccept: true,
			rule: 'Rule A',
			route: ['Jakarta', 'Bandung'],
			localDispatchUs: 850
		});
		const updated = result.find((r) => r.spxId === 'SPX1');
		expect(updated?.status).toBe('accepted');
		expect(updated?.latencyMs).toBe(312);
		expect(updated?.localDispatchUs).toBe(850);
		expect(updated?.accepting).toBe(false);
	});

	it('leaves other rows untouched', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2', status: 'pending' })];
		const result = applyAccepted(rows, {
			bookingId: 'SPX1',
			latencyMs: 312,
			autoAccept: true,
			rule: 'Rule A',
			route: [],
			localDispatchUs: 850
		});
		expect(result.find((r) => r.spxId === 'SPX2')?.status).toBe('pending');
	});
});

describe('applyRejected', () => {
	it('marks the matching row as taken_by_agency', () => {
		const rows = [row({ spxId: 'SPX1' })];
		const result = applyRejected(rows, 'SPX1');
		expect(result[0].status).toBe('taken_by_agency');
	});
});

describe('applyRemoved', () => {
	it('removes rows whose id is in the removed list', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2' })];
		const result = applyRemoved(rows, ['SPX1']);
		expect(result.map((r) => r.spxId)).toEqual(['SPX2']);
	});
});
```

- [ ] **Step 2: Run to verify failure**

```bash
cd Frontend && pnpm vitest run src/lib/ticker.test.ts
```
Expected: FAIL (`./ticker` module not found).

- [ ] **Step 3: Implement**

```typescript
// Frontend/src/lib/ticker.ts
// Pure delta-merge logic for the Live Ticket Ticker — deliberately separate from
// TicketTicker.svelte (Task 6) per the master spec's "logic merge/optimistic di helper $lib
// teruji" requirement. Every function takes the current rows array and returns a NEW array
// (never mutates its input) so Svelte 5's $state reassignment triggers reactivity correctly.
export type TicketRow = {
	spxId: string;
	status: 'pending' | 'accepted' | 'taken_by_agency';
	route: string[];
	latencyMs: number | null;
	localDispatchUs: number | null;
	/** True while an optimistic accept is in flight for this row. */
	accepting: boolean;
};

export function mergeNewTickets(rows: TicketRow[], incoming: TicketRow[]): TicketRow[] {
	const knownIds = new Set(rows.map((r) => r.spxId));
	const genuinelyNew = incoming.filter((r) => !knownIds.has(r.spxId));
	return [...genuinelyNew, ...rows];
}

export function applyAccepted(
	rows: TicketRow[],
	data: { bookingId: string; latencyMs: number; localDispatchUs: number }
): TicketRow[] {
	return rows.map((r) =>
		r.spxId === data.bookingId
			? { ...r, status: 'accepted' as const, latencyMs: data.latencyMs, localDispatchUs: data.localDispatchUs, accepting: false }
			: r
	);
}

export function applyRejected(rows: TicketRow[], bookingId: string): TicketRow[] {
	return rows.map((r) => (r.spxId === bookingId ? { ...r, status: 'taken_by_agency' as const, accepting: false } : r));
}

export function applyRemoved(rows: TicketRow[], ids: string[]): TicketRow[] {
	const removeSet = new Set(ids);
	return rows.filter((r) => !removeSet.has(r.spxId));
}

/** Optimistic accept: mark a row as "in flight" the instant the user clicks, before the server responds. */
export function markAccepting(rows: TicketRow[], spxId: string): TicketRow[] {
	return rows.map((r) => (r.spxId === spxId ? { ...r, accepting: true } : r));
}

/** Revert an optimistic accept that the server rejected (409/500/network failure). */
export function revertAccepting(rows: TicketRow[], spxId: string): TicketRow[] {
	return rows.map((r) => (r.spxId === spxId ? { ...r, accepting: false } : r));
}
```

- [ ] **Step 4: Run to verify all pass**

```bash
pnpm vitest run src/lib/ticker.test.ts
```
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/ticker.ts Frontend/src/lib/ticker.test.ts
git commit -m "feat(frontend): ticker delta-merge logic — pure, unit-tested (mergeNewTickets/applyAccepted/applyRejected/applyRemoved)"
```

---

## Task 6: Frontend — `TicketTicker.svelte` + optimistic accept

**Files:**
- Create: `Frontend/src/lib/components/TicketTicker.svelte`
- Create: `Frontend/src/lib/api-bookings.ts`

**Interfaces:**
- Consumes: Task 5's `ticker.ts` functions, Task 2's `lib/api.ts::apiPost` (Fase 7a).
- Produces (for Task 8): `<TicketTicker rows={...} onAccept={...} />` — but per Svelte 5 idiom, state lives IN this component (rows managed internally via `$state`, fed by props for initial data + an `onNewEvent` callback prop the parent wires to the WS store).

- [ ] **Step 1: `lib/api-bookings.ts` — typed REST helpers**

```typescript
// Frontend/src/lib/api-bookings.ts
import { apiPost, ApiError } from './api';
import type { TicketRow } from './ticker';

type BookingListItem = {
	id: string;
	accountId: string;
	spxId: string;
	status: string;
	route: string[];
};

export async function fetchLiveBookings(): Promise<TicketRow[]> {
	const res = await fetch('/bookings/live', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch live bookings');
	const items: BookingListItem[] = await res.json();
	return items.map((item) => ({
		spxId: item.spxId,
		status: item.status as TicketRow['status'],
		route: item.route,
		latencyMs: null,
		localDispatchUs: null,
		accepting: false
	}));
}

type ManualAcceptResponse = { ok: boolean; reason: string; message: string };

export async function acceptBooking(id: string): Promise<ManualAcceptResponse> {
	return apiPost<ManualAcceptResponse>(`/bookings/${id}/accept`, {});
}
```

Note: `GET /bookings/live` returns items keyed by `id` (the internal UUID row id, per `BookingListItem`), which is what `POST /bookings/:id/accept` expects as its path parameter — but `TicketRow.spxId` (used for WS delta-merge matching, since WS events key on `bookingId`/`spx_id`, not the UUID) is a DIFFERENT identifier. **Read `BookingListItem`'s real JSON field names once `pnpm check`/a live fetch confirms serde's camelCase rename behavior** (Rust's `#[derive(Serialize)]` on `BookingListItem` — confirm whether this crate applies a workspace-wide `#[serde(rename_all = "camelCase")]` or ships snake_case field names as-is; check an existing frontend consumer or the crate's `main.rs`/lib-level serde config before assuming camelCase). If the REST response is snake_case (`spx_id`, `account_id`) rather than camelCase, adjust `BookingListItem` above to match exactly — do not guess.

**Also:** `acceptBooking`'s path parameter needs the row's real UUID `id`, not `spxId` — thread BOTH `id` and `spxId` through `TicketRow` (`ticker.ts`'s Task 5 shape is missing `id`; if this is discovered during this task, that's a legitimate small addition to `TicketRow` — add an `id: string` field to the type in `ticker.ts` and to every test fixture in `ticker.test.ts` that constructs one, re-run Task 5's tests to confirm nothing broke, then proceed here. Flag this in your task report as a disclosed correction to Task 5's shape, found during integration — the plan's author did not have full certainty on this until writing this task).

- [ ] **Step 2: `TicketTicker.svelte`**

```svelte
<!-- Frontend/src/lib/components/TicketTicker.svelte -->
<script lang="ts">
	import type { TicketRow } from '$lib/ticker';
	import { markAccepting, revertAccepting, applyAccepted } from '$lib/ticker';
	import { acceptBooking } from '$lib/api-bookings';
	import { ApiError } from '$lib/api';

	let { rows = $bindable() }: { rows: TicketRow[] } = $props();

	let errorMsg = $state('');

	async function handleAccept(id: string, spxId: string) {
		rows = markAccepting(rows, spxId);
		errorMsg = '';
		try {
			const result = await acceptBooking(id);
			if (!result.ok) {
				rows = revertAccepting(rows, spxId);
				errorMsg = result.message;
				return;
			}
			// Optimistic confirm now; the WS ticket_accepted event that follows (handled by the
			// parent's WS subscription, Task 8) reconciles latency/localDispatchUs authoritatively.
			rows = applyAccepted(rows, { bookingId: spxId, latencyMs: 0, localDispatchUs: 0 });
		} catch {
			rows = revertAccepting(rows, spxId);
			errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
		}
	}
</script>

<div class="rounded-lg border border-border bg-bg-surface overflow-hidden">
	{#if errorMsg}
		<div role="alert" aria-live="polite" class="px-3 py-2 text-[11px] text-danger border-b border-border">
			{errorMsg}
		</div>
	{/if}
	<ul class="divide-y divide-border max-h-[420px] overflow-y-auto">
		{#each rows as row (row.spxId)}
			<li class="flex items-center gap-2.5 px-3 py-2 text-[11px] font-body">
				<span
					aria-hidden="true"
					class="w-1.5 h-1.5 rounded-full shrink-0
						{row.status === 'accepted' ? 'bg-live' : row.status === 'taken_by_agency' ? 'bg-text-muted' : 'bg-accent'}"
				></span>
				<span class="font-mono text-text-muted w-24 shrink-0">{row.spxId}</span>
				<span class="text-text-primary flex-1 truncate">{row.route.join(' → ') || '—'}</span>
				{#if row.status === 'pending'}
					<button
						type="button"
						disabled={row.accepting}
						onclick={() => handleAccept(row.spxId, row.spxId)}
						class="min-h-[32px] px-2.5 rounded-md text-[10px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{row.accepting ? 'Memproses…' : 'Terima'}
					</button>
				{:else if row.status === 'accepted'}
					<span class="font-mono text-live">{row.latencyMs}ms</span>
				{:else}
					<span class="font-mono text-text-muted">diambil lain</span>
				{/if}
			</li>
		{/each}
	</ul>
</div>
```

**Disclosed simplification:** `handleAccept`'s second parameter duplicates `spxId` as a stand-in for the real row `id` needed by `acceptBooking` — Step 1's own note above flags that `TicketRow` needs a real `id` field distinct from `spxId`; once that's added, replace `handleAccept(row.spxId, row.spxId)` with `handleAccept(row.id, row.spxId)` here. Do not ship this placeholder call — resolve it as part of this same task, using the real field once Step 1's investigation lands.

- [ ] **Step 3: Run `pnpm check` + unit tests, then commit**

```bash
cd Frontend && pnpm check && pnpm vitest run
```
Expected: 0 errors, all unit tests pass (including any `TicketRow.id` adjustments from Step 1).

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/components/TicketTicker.svelte Frontend/src/lib/api-bookings.ts \
        Frontend/src/lib/ticker.ts Frontend/src/lib/ticker.test.ts
git commit -m "feat(frontend): TicketTicker component — compact rows, optimistic manual-accept"
```

---

## Task 7: Frontend — `LatencyTape.svelte`

**Files:**
- Create: `Frontend/src/lib/components/LatencyTape.svelte`

**Interfaces:**
- Consumes: a `samples: number[]` prop (microsecond values, newest-last), fed by the parent's WS subscription (Task 8).

- [ ] **Step 1: Write the component**

```svelte
<!-- Frontend/src/lib/components/LatencyTape.svelte -->
<script lang="ts">
	// Canvas-based scope-trace visualization of local_dispatch_us samples — the "phosphor
	// oscilloscope" component validated in Fase 7b's brainstorming. Respects
	// prefers-reduced-motion: renders one static frame instead of a continuous animation loop
	// when the media query matches (checked once on mount; this page doesn't need to react to
	// the preference changing mid-session).
	let { samples }: { samples: number[] } = $props();

	let canvasEl: HTMLCanvasElement | undefined = $state();
	const BUDGET_US = 1000; // 1ms — spikes above this render in --color-accent, not --color-live.

	function readCssColor(varName: string): string {
		return getComputedStyle(document.documentElement).getPropertyValue(varName).trim();
	}

	function draw() {
		if (!canvasEl) return;
		const ctx = canvasEl.getContext('2d');
		if (!ctx) return;
		const { width, height } = canvasEl;
		ctx.clearRect(0, 0, width, height);
		if (samples.length < 2) return;

		const maxSample = Math.max(...samples, BUDGET_US * 1.2);
		const stepX = width / (samples.length - 1);
		const liveColor = readCssColor('--color-live');
		const accentColor = readCssColor('--color-accent');

		ctx.beginPath();
		ctx.strokeStyle = liveColor;
		ctx.lineWidth = 2;
		ctx.shadowColor = liveColor;
		ctx.shadowBlur = 6;
		samples.forEach((sample, i) => {
			const x = i * stepX;
			const y = height - (sample / maxSample) * height;
			if (i === 0) ctx.moveTo(x, y);
			else ctx.lineTo(x, y);
		});
		ctx.stroke();

		// Mark spikes over budget with a separate glowing dot, not part of the continuous stroke.
		samples.forEach((sample, i) => {
			if (sample <= BUDGET_US) return;
			const x = i * stepX;
			const y = height - (sample / maxSample) * height;
			ctx.beginPath();
			ctx.fillStyle = accentColor;
			ctx.shadowColor = accentColor;
			ctx.shadowBlur = 8;
			ctx.arc(x, y, 3, 0, Math.PI * 2);
			ctx.fill();
		});
	}

	$effect(() => {
		const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		draw();
		if (reducedMotion) return; // static single frame, no redraw loop
		// samples is a $state-tracked prop from the parent; re-running this effect whenever it
		// changes (Svelte 5 tracks `samples` access inside draw() automatically) IS the redraw
		// loop — no requestAnimationFrame needed since updates are event-driven (new WS samples),
		// not a continuous clock-driven animation.
	});

	const p99 = $derived.by(() => {
		if (samples.length === 0) return 0;
		const sorted = [...samples].sort((a, b) => a - b);
		const idx = Math.floor(sorted.length * 0.99);
		return sorted[Math.min(idx, sorted.length - 1)];
	});
</script>

<div class="rounded-lg border border-border bg-bg-surface p-4">
	<canvas bind:this={canvasEl} width="600" height="140" class="w-full" aria-hidden="true"></canvas>
	<div class="flex items-baseline gap-2 mt-2">
		<span class="font-mono text-live text-2xl font-semibold">
			{(p99 / 1000).toFixed(2)}<span class="text-xs text-text-muted">ms p99</span>
		</span>
	</div>
	<p class="sr-only" aria-live="polite">Latency keputusan p99: {(p99 / 1000).toFixed(2)} milidetik</p>
</div>
```

- [ ] **Step 2: Run `pnpm check`, then commit**

```bash
cd Frontend && pnpm check
```
Expected: 0 errors.

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/components/LatencyTape.svelte
git commit -m "feat(frontend): LatencyTape component — canvas scope-trace, reduced-motion aware"
```

---

## Task 8: Frontend — `/command` page assembly + Notification Center shell

**Files:**
- Create: `Frontend/src/routes/(app)/command/+page.svelte`
- Create: `Frontend/src/lib/components/NotificationCenter.svelte`
- Modify: `Frontend/src/lib/components/TopNav.svelte`

**Interfaces:**
- Consumes: Task 3 (`getContext('ws')`), Task 4 (nav shell), Task 5 (`ticker.ts`), Task 6 (`TicketTicker`), Task 7 (`LatencyTape`), Task 1's `route`/Task 2's `localDispatchUs` REST+WS fields.

- [ ] **Step 1: `NotificationCenter.svelte` (shell only, per design doc's disclosed scope)**

```svelte
<!-- Frontend/src/lib/components/NotificationCenter.svelte -->
<script lang="ts">
	// Shell only (Fase 7b design doc's disclosed scope) — real notification data source
	// (notifier crate / notifications table, never used by production code so far) needs its
	// own design pass in a future sub-fase. This is the icon + empty panel only.
	let open = $state(false);
</script>

<div class="relative">
	<button
		type="button"
		aria-label="Notifikasi"
		aria-expanded={open}
		onclick={() => (open = !open)}
		class="w-9 h-9 min-w-[44px] min-h-[44px] flex items-center justify-center rounded-lg text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		<span aria-hidden="true">&#128276;</span>
	</button>
	{#if open}
		<div
			class="absolute right-0 mt-2 w-64 rounded-lg border border-border bg-bg-surface shadow-lg p-4 text-xs text-text-muted"
			role="dialog"
			aria-label="Panel notifikasi"
		>
			Belum ada notifikasi.
		</div>
	{/if}
</div>
```

- [ ] **Step 2: Wire `NotificationCenter` into `TopNav`**

Read the current `TopNav.svelte` (Task 4) and replace its inline bell `<button>` with the real component:

```svelte
<!-- Frontend/src/lib/components/TopNav.svelte — replace the inline bell button -->
<script lang="ts">
	import { page } from '$app/state';
	import HealthPill from './HealthPill.svelte';
	import NotificationCenter from './NotificationCenter.svelte';
	import type { WsStatus } from '$lib/ws.svelte';

	let { wsStatus }: { wsStatus: WsStatus } = $props();
	// ...NAV_ITEMS unchanged...
</script>

<!-- ...nav/ul unchanged... -->
	<div class="ml-auto flex items-center gap-3 shrink-0">
		<HealthPill status={wsStatus} />
		<NotificationCenter />
	</div>
<!-- ...</nav> unchanged... -->
```

- [ ] **Step 3: `/command` page**

```svelte
<!-- Frontend/src/routes/(app)/command/+page.svelte -->
<script lang="ts">
	import { getContext, onMount, onDestroy } from 'svelte';
	import type { WsStore, TowerWsEvent } from '$lib/ws.svelte';
	import { fetchLiveBookings } from '$lib/api-bookings';
	import { mergeNewTickets, applyAccepted, applyRejected, applyRemoved, type TicketRow } from '$lib/ticker';
	import TicketTicker from '$lib/components/TicketTicker.svelte';
	import LatencyTape from '$lib/components/LatencyTape.svelte';

	const ws = getContext<WsStore>('ws');

	let rows = $state<TicketRow[]>([]);
	let dispatchSamples = $state<number[]>([]);
	const MAX_SAMPLES = 200;

	function handleWsEvent(event: TowerWsEvent) {
		if (event.type === 'ticket_accepted') {
			rows = applyAccepted(rows, event.data);
			dispatchSamples = [...dispatchSamples, event.data.localDispatchUs].slice(-MAX_SAMPLES);
		} else if (event.type === 'ticket_rejected') {
			rows = applyRejected(rows, event.data.bookingId);
		} else if (event.type === 'tickets_removed') {
			rows = applyRemoved(rows, event.data.ids);
		}
	}

	const LIVE_POLL_INTERVAL_MS = 20_000;
	let pollTimer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		fetchLiveBookings().then((initial) => {
			rows = mergeNewTickets([], initial);
		});
		const unsubscribe = ws.onEvent(handleWsEvent);
		pollTimer = setInterval(() => {
			fetchLiveBookings().then((fresh) => {
				rows = mergeNewTickets(rows, fresh);
			});
		}, LIVE_POLL_INTERVAL_MS);
		return () => {
			unsubscribe();
		};
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<svelte:head>
	<title>Command — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	<LatencyTape samples={dispatchSamples} />
	<TicketTicker bind:rows />
</div>
```

- [ ] **Step 4: Run `pnpm check` + unit tests, then commit**

```bash
cd Frontend && pnpm check && pnpm vitest run
```
Expected: 0 errors, all unit tests pass.

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add "Frontend/src/routes/(app)/command/+page.svelte" Frontend/src/lib/components/NotificationCenter.svelte \
        Frontend/src/lib/components/TopNav.svelte
git commit -m "feat(frontend): /command page — Latency Tape + Ticket Ticker wired to live WS + polling"
```

---

## Task 9: E2E test + final verification

**Files:**
- Create: `Frontend/tests/command.spec.ts`

**Interfaces:** none new — integration proof.

- [ ] **Step 1: Write the e2e test**

```typescript
// Frontend/tests/command.spec.ts
//
// Reuses the same real-stack setup as tests/login.spec.ts (Fase 7a Task 6) — read that file's
// top comment for the full prerequisite list (reactor-core running locally on :8081 with
// TENANT_SLUG=tower-dev, the seeded e2e-test-user). This file additionally needs at least one
// `pending` booking row seeded into `bookings` for the same tenant, so /command's ticker has
// something real to display and accept. Seed via the same direct-psql pattern login.spec.ts's
// header documents (INSERT INTO bookings ...) — this plan's author does not have the exact
// column list memorized; read Backend/crates/store/migrations/ for the bookings table shape
// before writing the seed command, matching the established "verify against real schema"
// discipline this project applies everywhere.
import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page) {
	await page.goto('/login');
	await page.getByLabel('Username').fill('e2e-test-user');
	await page.getByLabel('Password').fill('correct-horse-battery-staple');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /command redirects to /login', async ({ page }) => {
	await page.goto('/command');
	await expect(page).toHaveURL(/\/login/);
});

test('after login, /command shows the nav shell with LIVE health pill once WS connects', async ({ page }) => {
	await login(page);
	await expect(page.getByText('TOWER')).toBeVisible();
	await expect(page.getByText('LIVE')).toBeVisible({ timeout: 10_000 });
});

test('ticket ticker shows the seeded pending booking', async ({ page }) => {
	await login(page);
	await expect(page.getByText('Terima')).toBeVisible({ timeout: 10_000 });
});

test('keyboard-only: tab to a nav link and activate it', async ({ page }) => {
	await login(page);
	await page.getByRole('link', { name: 'Tickets' }).focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/tickets/);
});
```

- [ ] **Step 2: Run the e2e suite for real**

Prerequisites: `tower-postgres`/`tower-redis` up, `reactor-core` running locally (same env block as `login.spec.ts`'s header comment), a seeded pending booking (this task's own job to seed, see Step 1's note).

```bash
cd Frontend && pnpm exec playwright test tests/command.spec.ts
```
Expected: all 4 tests pass. Investigate and root-cause-fix any failure — do not weaken assertions or add arbitrary waits without understanding why a wait is needed (matching Fase 7a Task 6's own precedent for its `networkidle` fix).

- [ ] **Step 3: Full backend + frontend verification**

```bash
cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && unset DATABASE_URL && export REDIS_URL="redis://127.0.0.1:16379"
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cd ../Frontend
pnpm check
pnpm vitest run
pnpm build
```
Expected: all genuinely green.

- [ ] **Step 4: Commit**

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/tests/command.spec.ts
git commit -m "test(fase-7b): /command e2e (Playwright) — full workspace + frontend verification"
```

---

## Self-Review Notes (writing-plans skill, run by the plan author before handoff)

**Spec coverage:** every element of the approved design doc has a task — route field + local_dispatch_us instrumentation (Tasks 1-2), WS store (Task 3), nav shell + session gate + Health Pill (Task 4), ticker delta-merge logic tested separately per master-spec requirement (Task 5), TicketTicker + optimistic accept (Task 6), Latency Tape canvas (Task 7), page assembly + Notification Center shell (Task 8), e2e proof (Task 9). The 20s polling fallback for `new_tickets` (a disclosed, human-approved scope decision) is implemented in Task 8, using the SAME `mergeNewTickets` function real-time WS events use (Task 5) — one merge path, not two.

**Placeholder scan:** Task 6 Step 1/2 explicitly disclose an unresolved detail (whether `BookingListItem`'s JSON uses camelCase, and that `TicketRow` needs a real `id` field distinct from `spxId`) as a NAMED, bounded investigation with a clear resolution path — not a vague "handle appropriately." This mirrors the established convention (Fase 6e's Task 2/3, Fase 7a's Task 4) of disclosing genuine research gaps precisely rather than inventing false certainty. Every other step is complete, runnable code.

**Type consistency:** `TicketRow` (Task 5) is used identically by `ticker.ts`'s own functions, `TicketTicker.svelte` (Task 6), and `/command/+page.svelte` (Task 8) — `spxId`/`status`/`route`/`latencyMs`/`localDispatchUs`/`accepting` fields never renamed between tasks. `TowerWsEvent`'s `ticket_accepted` data shape (Task 3) matches exactly what Task 1/2's backend changes actually publish (`bookingId`/`latencyMs`/`autoAccept`/`rule`/`route`/`localDispatchUs`) — traced field-by-field against the real `serde_json::json!({...})` literal in `finalize_win`, not assumed.

**Cross-task dependency order:** Tasks 1-2 (backend) are independent of Tasks 3-9 (frontend) and of each other, but Task 2 modifies the SAME `finalize_win` call Task 1 also touches — sequenced 1 then 2 to avoid the two tasks conflicting on the same `serde_json::json!` literal in one commit each (Task 2's brief explicitly shows the "route" field from Task 1 already present when it adds "localDispatchUs" alongside it). Task 3 (WS store) has no frontend dependency, could run before or parallel to Task 1/2 in principle. Task 4 depends on Task 3. Task 5 is fully independent (pure logic). Task 6 depends on Task 5 and Fase 7a's `lib/api.ts`. Task 7 is independent (only needs a `number[]` prop shape, defined by Task 5/8's usage, not Task 7 itself). Task 8 depends on Tasks 3-7 all landing first (it wires everything together) plus Task 1/2's backend fields actually existing. Task 9 depends on everything. This ordering (1→2, 3→4, 5→6, 7, then 8, then 9, with 3/4/5/6/7 each independently startable after 1-2 land) is a valid topological sort — subagent-driven-development still runs them strictly serially per that skill's own rule, this note is for understanding, not for parallelizing.
