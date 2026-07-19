# Fase 7i: `/settings/locations` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `/settings/locations`, a flat add/delete management page over the already-existing `GET/POST/DELETE /locations` endpoint, and refactor the `/settings` shell's nav array from an if/else ternary to a flag-filtered flat array (tracked as a Minor from Fase 7h's whole-branch review). Pure frontend build.

**Architecture:** Unlike Fase 7h's Bot page (content-gated), this resource is edit-gated like Fase 7g's Branding page: `GET /locations` is open to any session, so the page and its real data are always visible; only the add-input and delete buttons are disabled for non-main-account (no `<fieldset disabled>`-wrapped read-only view is needed for a list this simple — each disabled control is marked individually). No pagination (the backend returns the full list, unbounded, and locations are expected to be a small hand-curated set). No rename — the backend only supports add/delete.

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-20-fase-7i-settings-locations-design.md` — read it first for full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format needs no snake_case↔camelCase mapping for this resource** — `LocationItem` is `{id: string, name: string}` on both the wire and in the app; the backend's `id`/`name` fields already match camelCase directly (re-verified against `Backend/crates/api-gateway/src/routes/locations.rs` for this plan).
- **`GET /locations` has NO permission gate** — any authenticated session can view the full list. Only `POST`/`DELETE` require `Permission::ManageLocations` (main-account only). This page is therefore edit-gated (disable add/delete controls), never content-gated (never hide the list or the nav entry).
- **A duplicate `(tenant_id, name)` on `POST /locations` returns `409` with a generic body `{"error": "already exists"}`** — this codebase's convention (confirmed in `/price`'s `PriceRow.svelte`) is to catch `err instanceof ApiError && err.status === 409` and show a client-hardcoded Indonesian message ("Lokasi ini sudah ada."), NOT to parse the response body for the backend's literal text. No `api-*.ts` module in this codebase reads error response bodies — do not start here either.
- **`DELETE /locations/:id` returns `204 No Content`** — never call `.json()` on that response.
- **Deleting a location is safe and has no cascading effect** — confirmed by reading the schema directly: `route_locations` has no incoming foreign keys, and `accept_rules`/`route_prices` store location names as plain text/JSON strings, not ids. A native `confirm()` guard (matching `/rules`'/`/price`'s established delete precedent) is sufficient; do not build a "used by N rules" warning feature — the data model doesn't cleanly support it and nothing actually breaks on delete.
- **No rename/edit-in-place** — the backend has no update capability for this resource by design (no `updated_at` column, no update fn). Do not add a rename UI affordance.
- **`onMount(load)`, never a bare top-level `load()` call** — this app runs SSR (`adapter-node`, no `ssr = false` anywhere) and a relative-path `fetch` has no origin during Node SSR. This exact bug was caught by Fase 7g's Task 4 review — do not reintroduce it.
- **`Frontend/src/lib/api-locations.ts` is a deliberately NEW, separate module** from `Frontend/src/lib/api-rules.ts`'s own existing `fetchLocations`/`createLocation` (used by `/rules`' inline `LocationCombobox` create-flow). Do not import from or refactor `api-rules.ts` — duplicating these 3 small functions is cheaper than introducing a cross-page dependency, matching this codebase's existing pattern (`api-prices.ts` doesn't import from `api-rules.ts` either).
- **Accessibility bar (established 7a-7h convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error banners, a `<svelte:head><title>Lokasi — TOWER</title></svelte:head>`, "…" ellipsis character (not "...") for loading/adding/deleting text.
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `api-locations.ts` — typed REST layer

**Files:**
- Create: `Frontend/src/lib/api-locations.ts`
- Test: `Frontend/src/lib/api-locations.test.ts`

**Interfaces:**
- Produces: `type LocationItem = { id: string; name: string }`. `fetchLocations(): Promise<LocationItem[]>`. `createLocation(name: string): Promise<LocationItem>`. `deleteLocation(id: string): Promise<void>`.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/api-locations.test.ts
// vi.stubGlobal('fetch', ...) regression guards for this module's load-bearing HTTP details:
// POST (via apiPost) for create, DELETE-with-id-in-path-and-no-body for delete, and status-code
// propagation (409 for duplicate, etc.) via ApiError.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchLocations, createLocation, deleteLocation } from './api-locations';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('fetchLocations', () => {
	it('issues a GET to /locations and returns the list as-is (already camelCase)', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([{ id: 'loc-1', name: 'Jakarta' }]), { status: 200 });
			})
		);
		const locations = await fetchLocations();
		expect(calledUrl).toBe('/locations');
		expect(locations).toEqual([{ id: 'loc-1', name: 'Jakarta' }]);
	});

	it('throws ApiError with the real status code on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
		await expect(fetchLocations()).rejects.toMatchObject({ status: 500 });
	});
});

describe('createLocation', () => {
	it('issues a POST to /locations with the name', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify({ id: 'loc-2', name: 'Bandung' }), { status: 200 });
			})
		);
		const created = await createLocation('Bandung');
		expect(calledUrl).toBe('/locations');
		expect(calledInit?.method).toBe('POST');
		expect(JSON.parse(calledInit?.body as string)).toEqual({ name: 'Bandung' });
		expect(created).toEqual({ id: 'loc-2', name: 'Bandung' });
	});

	it('throws ApiError with status 409 on a duplicate name', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'already exists' }), { status: 409 }))
		);
		await expect(createLocation('Jakarta')).rejects.toMatchObject({ status: 409 });
	});
});

describe('deleteLocation', () => {
	it('issues a DELETE to /locations/{id} and does not attempt to parse a body', async () => {
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
		await deleteLocation('loc-1');
		expect(calledUrl).toBe('/locations/loc-1');
		expect(calledInit?.method).toBe('DELETE');
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 404 })));
		await expect(deleteLocation('missing-id')).rejects.toThrow();
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-locations.test.ts`
Expected: FAIL — `./api-locations` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `api-locations.ts`**

```typescript
// Frontend/src/lib/api-locations.ts
// Typed REST layer for /settings/locations. Deliberately a SEPARATE module from
// Frontend/src/lib/api-rules.ts's own fetchLocations/createLocation (which serves /rules' inline
// LocationCombobox create-flow) — duplicating these 3 small functions is cheaper than introducing
// a cross-page dependency, matching this codebase's tolerance for small, page-scoped API modules
// (api-prices.ts doesn't import from api-rules.ts either). Wire shape verified against
// Backend/crates/api-gateway/src/routes/locations.rs — id/name already match camelCase
// field-for-field, no snake_case conversion needed.
import { apiPost, ApiError } from './api';

export type LocationItem = {
	id: string;
	name: string;
};

export async function fetchLocations(): Promise<LocationItem[]> {
	const res = await fetch('/locations', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch locations');
	return res.json();
}

export async function createLocation(name: string): Promise<LocationItem> {
	return apiPost<LocationItem>('/locations', { name });
}

/** `DELETE /locations/{id}` returns `204 No Content` on success — never call `res.json()` on
 * this response, there is no body to parse. */
export async function deleteLocation(id: string): Promise<void> {
	const res = await fetch(`/locations/${id}`, { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to delete location');
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-locations.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/api-locations.ts Frontend/src/lib/api-locations.test.ts
git commit -m "feat(frontend): api-locations.ts — typed REST layer for /settings/locations"
```

---

### Task 2: `/settings` shell — refactor nav to flag-filtered array, add "Locations"

**Files:**
- Modify: `Frontend/src/routes/(app)/settings/+layout.svelte`

**Interfaces:**
- Consumes: `data.user.is_main_account` (ambient, unchanged).
- Produces: the `NAV_ITEMS` array every `/settings/*` page (including Task 3's `/settings/locations`) renders inside — now with a "Locations" entry, always visible.

**Current file content** (for exact context — you are modifying this, not creating it fresh):

```svelte
<!-- Shared shell for every /settings/* sub-route. The nav array grows by one entry per future
     sub-phase (Locations, Sub-users, SPX Credentials — Fase 7i-7k) — no placeholder entries for
     resources that don't exist yet, matching TopNav.svelte's own established convention of not
     building UI for not-yet-built surfaces. "Bot" is main-account-only (unlike "Branding"):
     GET /bot/settings itself requires Permission::ManageBotSettings, so a non-main-account
     session must never even see this nav entry — matching Fase 7f's Log Bot tab-hiding pattern,
     not Fase 7g's Branding read-only-view pattern. -->
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { children, data }: LayoutProps = $props();

	const NAV_ITEMS = $derived(
		data.user.is_main_account
			? [
					{ href: '/settings/branding', label: 'Branding' },
					{ href: '/settings/bot', label: 'Bot' }
				]
			: [{ href: '/settings/branding', label: 'Branding' }]
	);
</script>

<div class="flex flex-col gap-4 p-4">
	<h1 class="font-heading font-bold text-lg text-text-primary">Settings</h1>
	<nav class="flex gap-4 border-b border-border" aria-label="Navigasi settings">
		{#each NAV_ITEMS as item (item.href)}
			<a
				href={item.href}
				class="pb-2 border-b-2 text-[13px] font-body min-h-[44px] flex items-center focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent
					{page.url.pathname.startsWith(item.href)
					? 'border-accent text-accent'
					: 'border-transparent text-text-muted hover:text-text-primary'}"
				aria-current={page.url.pathname.startsWith(item.href) ? 'page' : undefined}
			>
				{item.label}
			</a>
		{/each}
	</nav>
	{@render children()}
</div>
```

- [ ] **Step 1: Replace the `<script>` block's comment + `NAV_ITEMS` logic**

Replace everything from the top comment through the closing `</script>` tag with:

```svelte
<!-- Shared shell for every /settings/* sub-route. NAV_ITEMS is a flat array with a per-item
     mainAccountOnly flag, filtered by data.user.is_main_account — refactored from the previous
     if/else ternary (tracked as a Minor in Fase 7h's whole-branch review) now that a SECOND
     always-visible entry ("Lokasi") needs adding; the ternary would have needed updating in both
     branches for every future always-visible entry. Scales cleanly to Fase 7j (Sub-users,
     main-account-only like Bot) and 7k (SPX Credentials, open like Branding/Locations) without
     further restructuring. No placeholder entries for resources that don't exist yet, matching
     TopNav.svelte's own established convention. -->
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { children, data }: LayoutProps = $props();

	type NavItem = { href: string; label: string; mainAccountOnly?: boolean };

	const ALL_NAV_ITEMS: NavItem[] = [
		{ href: '/settings/branding', label: 'Branding' },
		{ href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
		{ href: '/settings/locations', label: 'Lokasi' }
	];

	const NAV_ITEMS = $derived(ALL_NAV_ITEMS.filter((item) => !item.mainAccountOnly || data.user.is_main_account));
</script>
```

Leave the markup below the `</script>` tag exactly as-is — it already iterates `NAV_ITEMS` correctly and needs no changes.

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings. (`/settings/locations` doesn't exist yet until Task 3 — the new nav link's href being momentarily a 404 is expected and harmless; svelte-check does not verify route existence.)

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/+layout.svelte"
git commit -m "refactor(frontend): /settings shell — flag-filtered nav array, add Locations entry"
```

---

### Task 3: `/settings/locations/+page.svelte` — page assembly

**Files:**
- Create: `Frontend/src/routes/(app)/settings/locations/+page.svelte`

**Interfaces:**
- Consumes: `LocationItem`, `fetchLocations`, `createLocation`, `deleteLocation` from `$lib/api-locations` (Task 1). `ApiError` from `$lib/api`. `data.user.is_main_account` from the ambient `(app)/+layout.server.ts` data (same convention as `/rules`/`/price`/`/settings/branding`).
- Produces: nothing further — leaf page.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/settings/locations/+page.svelte -->
<!-- Flat add/delete management list for the tenant's known route locations. GET /locations has
     no permission gate (any authenticated session sees the real list), only POST/DELETE are
     main-account-gated — so this page is edit-gated like /settings/branding (always visible,
     controls individually disabled), never content-gated like /settings/bot. Deleting a location
     is genuinely safe (confirmed via schema: no other table references route_locations by id,
     accept_rules/route_prices store location names as plain text) — a native confirm() is
     sufficient, no "in use" warning needed. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchLocations, createLocation, deleteLocation, type LocationItem } from '$lib/api-locations';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let locations = $state<LocationItem[]>([]);
	let newName = $state('');
	let loading = $state(true);
	let adding = $state(false);
	let deletingId = $state<string | null>(null);
	let errorMsg = $state('');

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			locations = await fetchLocations();
		} catch {
			errorMsg = 'Gagal memuat lokasi.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function handleAdd() {
		const trimmed = newName.trim();
		if (trimmed === '') return;
		adding = true;
		errorMsg = '';
		try {
			const created = await createLocation(trimmed);
			locations = [...locations, created].sort((a, b) => a.name.localeCompare(b.name));
			newName = '';
		} catch (err) {
			errorMsg = err instanceof ApiError && err.status === 409 ? 'Lokasi ini sudah ada.' : 'Gagal menambah lokasi.';
		} finally {
			adding = false;
		}
	}

	async function handleDelete(location: LocationItem) {
		if (!confirm(`Hapus lokasi "${location.name}"?`)) return;
		deletingId = location.id;
		errorMsg = '';
		try {
			await deleteLocation(location.id);
			locations = locations.filter((l) => l.id !== location.id);
		} catch {
			errorMsg = 'Gagal menghapus lokasi.';
		} finally {
			deletingId = null;
		}
	}
</script>

<svelte:head>
	<title>Lokasi — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}

		<fieldset disabled={readOnly} class="flex gap-2 border-0 p-0">
			<label class="sr-only" for="new-location-name">Nama lokasi baru</label>
			<input
				id="new-location-name"
				type="text"
				bind:value={newName}
				onkeydown={(e) => e.key === 'Enter' && handleAdd()}
				placeholder="Nama lokasi baru"
				class="flex-1 min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			<button
				type="button"
				onclick={handleAdd}
				disabled={adding || newName.trim() === ''}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{adding ? 'Menambah…' : 'Tambah'}
			</button>
		</fieldset>

		{#if locations.length === 0}
			<p class="text-[13px] text-text-muted">Belum ada lokasi.</p>
		{:else}
			<ul class="flex flex-col gap-2">
				{#each locations as location (location.id)}
					<li class="flex items-center justify-between gap-2 rounded-lg border border-border bg-bg-surface p-3">
						<span class="text-[13px] font-body text-text-primary">{location.name}</span>
						<button
							type="button"
							disabled={readOnly || deletingId === location.id}
							onclick={() => handleDelete(location)}
							class="min-h-[36px] px-2 text-[11px] text-danger disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							{deletingId === location.id ? 'Menghapus…' : 'Hapus'}
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/locations/+page.svelte"
git commit -m "feat(frontend): /settings/locations — page assembly (add/delete list, RBAC-disabled controls)"
```

---

### Task 4: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/settings-locations.spec.ts`

**Interfaces:**
- Consumes: the full `/settings` shell (Task 2) + `/settings/locations` page (Task 3) built in Tasks 1-3. No new frontend code — this task authors real-stack e2e coverage and runs full verification.

**No new seed users needed.** Reuses `e2e-test-user` (main-account) and `e2e-readonly-user` (non-main-account) from Fase 7a/7d, both already seeded. **No new data seed needed either** — this task creates and deletes its own location rows within the test itself, self-cleaning (no shared-fixture risk like Fase 7h's `waha_settings` row, since `route_locations` has no other dependents).

- [ ] **Step 1: Write `Frontend/tests/settings-locations.spec.ts`**

```typescript
// Frontend/tests/settings-locations.spec.ts
//
// REAL end-to-end proof of Fase 7i's /settings/locations page. Same real-stack setup as
// tests/login.spec.ts, tests/settings-branding.spec.ts, tests/settings-bot.spec.ts — real
// reactor-core on :8081 behind Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432).
// Nothing here is mocked.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d. Every location this suite creates is deleted by the end of its own test
// (or is itself the thing being deleted) — no shared fixture risk, unlike Fase 7h's waha_settings
// row: route_locations has no other suite depending on its contents.

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /settings/locations redirects to /login', async ({ page }) => {
	await page.goto('/settings/locations');
	await expect(page).toHaveURL(/\/login/);
});

test('main account sees the Locations nav entry and can add and delete a location', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByRole('link', { name: 'Lokasi' })).toBeVisible();

	await page.goto('/settings/locations');
	const nameInput = page.getByLabel('Nama lokasi baru');
	await expect(nameInput).toBeVisible({ timeout: 10_000 });

	const uniqueName = `E2E Test Location ${Date.now()}`;
	await nameInput.fill(uniqueName);
	await page.getByRole('button', { name: 'Tambah' }).click();
	await expect(page.getByText(uniqueName)).toBeVisible({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText(uniqueName)).toBeVisible({ timeout: 10_000 });

	page.once('dialog', (dialog) => dialog.accept());
	await page
		.locator('li', { hasText: uniqueName })
		.getByRole('button', { name: 'Hapus' })
		.click();
	await expect(page.getByText(uniqueName)).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText(uniqueName)).toBeHidden({ timeout: 10_000 });
});

test('adding a duplicate name shows the specific 409 message', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/locations');
	const nameInput = page.getByLabel('Nama lokasi baru');
	await expect(nameInput).toBeVisible({ timeout: 10_000 });

	const uniqueName = `E2E Dup Location ${Date.now()}`;
	await nameInput.fill(uniqueName);
	await page.getByRole('button', { name: 'Tambah' }).click();
	await expect(page.getByText(uniqueName)).toBeVisible({ timeout: 10_000 });

	// Attempt to add the exact same name again.
	await nameInput.fill(uniqueName);
	await page.getByRole('button', { name: 'Tambah' }).click();
	await expect(page.getByText('Lokasi ini sudah ada.')).toBeVisible({ timeout: 10_000 });

	// Clean up — delete the one real row this test created.
	page.once('dialog', (dialog) => dialog.accept());
	await page
		.locator('li', { hasText: uniqueName })
		.getByRole('button', { name: 'Hapus' })
		.click();
	await expect(page.getByText(uniqueName)).toBeHidden({ timeout: 10_000 });
});

test('non-main-account session sees the real list with disabled add/delete controls', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/locations');

	const nameInput = page.getByLabel('Nama lokasi baru');
	await expect(nameInput).toBeVisible({ timeout: 10_000 });
	await expect(nameInput).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Tambah' })).toBeDisabled();
});
```

- [ ] **Step 2: Run the new e2e file alone**

Run: `cd Frontend && pnpm exec playwright test tests/settings-locations.spec.ts --workers=1`
Expected: all tests pass (a live `reactor-core` + `tower-postgres` stack must already be running).

- [ ] **Step 3: Run the full Playwright suite (regression check)**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across every prior suite plus `settings-locations.spec.ts` pass. **If any pre-existing test fails showing a still-on-`/login` symptom, check for the known shared-`reactor-core` login rate-limiter flake (see Fase 7f/7g/7h's own notes) before assuming a regression** — restart `reactor-core` and rerun the failing file alone to confirm.

- [ ] **Step 4: Full backend verification**

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green. This task makes no backend changes, so this is a pure regression check. **Run these as normal foreground commands and wait for their actual output — do not background them and lose track of results** (a repeated hiccup in Fase 7f/7g/7h's own Task 5 implementer runs).

- [ ] **Step 5: Full frontend verification**

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `api-locations.test.ts`, plus every pre-existing suite — no regression); production build succeeds.

- [ ] **Step 6: Commit**

```bash
git add Frontend/tests/settings-locations.spec.ts
git commit -m "test(fase-7i): /settings/locations e2e — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — flat add/delete list (Task 3), always-visible nav entry via the refactored array (Task 2), edit-gating with individually-disabled controls (Task 3). Every "Out of scope" bullet (pagination, search/filter, rename, "in use" warnings) has no corresponding task, and the Global Constraints section explicitly calls out why not.

**Placeholder scan:** no TBD/TODO. Every code block is complete, runnable content.

**Type consistency:** `LocationItem` (Task 1) — `{id, name}` — is the exact shape threaded unchanged through Task 3's page state and Task 4's e2e assertions. No wire-mapping bugs possible since the wire and app shapes are identical for this resource (unlike Branding/BotSettings's snake_case↔camelCase split).

**Cross-task dependency ordering:** 1 (REST layer) and 2 (shell nav refactor) are independent of each other → 3 (page, depends on 1 and 2's route existing) → 4 (e2e, depends on everything). No task references a later task's output.

**A genuinely simpler phase than 7g/7h, by design, not by accident:** no pure-logic module needed (no validation beyond non-empty-name, trivial enough to inline); no shared-fixture risk in e2e (each test creates and cleans up its own rows); no SSRF/secret-handling concerns; RBAC model matches the already-proven Branding pattern rather than introducing a third variant. The one genuinely new piece of work — the nav-array refactor — was explicitly flagged and deferred to this exact phase by Fase 7h's own whole-branch review, not discovered fresh here.
