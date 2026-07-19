# Fase 7j: `/settings/sub-users` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `/settings/sub-users`, a create/delete management page over the already-existing `GET/POST/DELETE /auth/portal-users` endpoint, and append a "Sub-user" entry to the `/settings` shell's nav array. Pure frontend build.

**Architecture:** Edit-gated like Fase 7g's Branding and Fase 7i's Locations (`GET` open to any session, `POST`/`DELETE` main-account-gated) — NOT content-gated like Fase 7h's Bot. This is a correction from a wrong assumption baked into Fase 7h/7i's own code comments (which said "Sub-users, main-account-only like Bot") — re-reading `portal_users.rs` directly during this phase's brainstorming confirmed `GET` has no permission gate at all. The list is always visible; only the create-form and delete buttons are disabled for non-main-account. A genuine backend self-lockout guard (a main account cannot delete their own row) is surfaced client-side by disabling that one row's delete button with an inline note, detected by matching `username` (the only identity `/auth/me` exposes — no portal_user id).

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), `@lucide/svelte`, Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-20-fase-7j-settings-sub-users-design.md` — read it first for full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format needs snake_case↔camelCase mapping** — `display_name`/`is_main_account` on the wire become `displayName`/`isMainAccount` in the app (re-verified against `Backend/crates/api-gateway/src/routes/portal_users.rs`'s real `PortalUserSummary`/`CreatePortalUser` structs for this plan).
- **`GET /auth/portal-users` has NO permission gate** — any authenticated session can view the full list. Only `POST`/`DELETE` require `Permission::ManageSubUsers` (main-account only). This page is edit-gated, never content-gated — do not hide the list or the nav entry for any authenticated role.
- **Password minimum is 8 characters, enforced by the backend with a `400`** — mirror this exactly client-side (`validatePassword`) for immediate feedback. Never echo a password back anywhere; `PortalUserSummary` never carries one.
- **A duplicate `(tenant_id, username)` on `POST` returns `409` with a generic body** — same client-hardcoded-message convention as Locations (`err.status === 409` → "Username ini sudah dipakai."), never parse the response body.
- **`DELETE /auth/portal-users/:id` has a real self-lockout guard**: `400 "cannot delete your own account"` if the id belongs to the CALLER. The frontend cannot compare ids directly (`/auth/me` exposes no portal_user id) — detect "is this my own row" by comparing `username` to `data.user.username` via the `isSelf` helper (exact string equality — usernames are compared as-is by the backend, confirmed by reading `portal_users.rs::find_by_username`, no case-folding anywhere).
- **No edit/rename/enable-disable capability exists in the backend** — create+delete only. Do not add UI for a capability the API doesn't support. `enabled` is part of the wire type (for honesty about the real API shape) but is NEVER rendered — no code path can ever make it anything but `true`.
- **`onMount(load)`, never a bare top-level `load()` call** — this app runs SSR (`adapter-node`, no `ssr = false` anywhere) and a relative-path `fetch` has no origin during Node SSR. This exact bug was caught by Fase 7g's Task 4 review — do not reintroduce it.
- **Reuse `/login`'s exact password show/hide toggle pattern** (`Frontend/src/routes/login/+page.svelte`) — `Eye`/`EyeOff` from `@lucide/svelte`, `aria-pressed`, `sr-only` label text. Do not invent a new password-visibility pattern.
- **Accessibility bar (established 7a-7i convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error/read-only banners, a `<svelte:head><title>Sub-user — TOWER</title></svelte:head>`, native `confirm()` for delete, the read-only explanatory banner built in from the start (a Fase 7i whole-branch-review lesson: `/settings/branding` and `/settings/locations` both needed this added post-review — get it right here from the beginning).
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `sub-users.ts` — pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/sub-users.ts`
- Test: `Frontend/src/lib/sub-users.test.ts`

**Interfaces:**
- Produces: `validatePassword(password: string): string | null` (returns an error message, or `null` if valid). `isSelf(username: string, sessionUsername: string): boolean`.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/sub-users.test.ts
import { describe, it, expect } from 'vitest';
import { validatePassword, isSelf } from './sub-users';

describe('validatePassword', () => {
	it('rejects a password under 8 characters', () => {
		expect(validatePassword('1234567')).not.toBeNull();
	});

	it('accepts a password of exactly 8 characters', () => {
		expect(validatePassword('12345678')).toBeNull();
	});

	it('accepts a longer password', () => {
		expect(validatePassword('a-much-longer-password')).toBeNull();
	});

	it('rejects an empty password', () => {
		expect(validatePassword('')).not.toBeNull();
	});
});

describe('isSelf', () => {
	it('returns true for an exact username match', () => {
		expect(isSelf('e2e-test-user', 'e2e-test-user')).toBe(true);
	});

	it('returns false for a different username', () => {
		expect(isSelf('e2e-readonly-user', 'e2e-test-user')).toBe(false);
	});

	it('is case-sensitive, matching the backend\'s exact-match comparison (no case-folding in portal_users.rs)', () => {
		expect(isSelf('E2E-Test-User', 'e2e-test-user')).toBe(false);
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/sub-users.test.ts`
Expected: FAIL — `./sub-users` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `sub-users.ts`**

```typescript
// Frontend/src/lib/sub-users.ts
// Pure logic for /settings/sub-users — no fetch, no DOM.

/** Mirrors the backend's own minimum (Backend/crates/api-gateway/src/routes/portal_users.rs::
 * create: `body.password.len() < 8`). */
export function validatePassword(password: string): string | null {
	if (password.length < 8) {
		return 'Password minimal 8 karakter';
	}
	return null;
}

/** "Is this list row the currently logged-in session's own account?" — the frontend has no
 * portal_user id to compare (Frontend/src/routes/(app)/+layout.server.ts's SessionUser type
 * carries only {username, display_name, is_main_account}), so this compares usernames, which
 * are tenant-unique. Exact-match (===), matching the backend's own `username = $2` comparison
 * (Backend/crates/store/src/portal_users.rs::find_by_username) — no case-folding either side. */
export function isSelf(username: string, sessionUsername: string): boolean {
	return username === sessionUsername;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/sub-users.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/sub-users.ts Frontend/src/lib/sub-users.test.ts
git commit -m "feat(frontend): sub-users.ts — pure logic (password validation, self-row detection)"
```

---

### Task 2: `api-sub-users.ts` — typed REST layer

**Files:**
- Create: `Frontend/src/lib/api-sub-users.ts`
- Test: `Frontend/src/lib/api-sub-users.test.ts`

**Interfaces:**
- Consumes: nothing from Task 1 (independent module).
- Produces: `type PortalUser = { id: string; username: string; displayName: string; isMainAccount: boolean; enabled: boolean }`. `type CreateSubUserInput = { username: string; password: string; displayName: string; isMainAccount: boolean }`. `fetchSubUsers(): Promise<PortalUser[]>`. `createSubUser(input: CreateSubUserInput): Promise<PortalUser>`. `deleteSubUser(id: string): Promise<void>`.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/api-sub-users.test.ts
// vi.stubGlobal('fetch', ...) regression guards for this module's load-bearing HTTP details:
// snake_case<->camelCase wire mapping, POST via apiPost, DELETE-with-id-in-path-and-no-body,
// and status-code propagation (409 for duplicate username) via ApiError.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchSubUsers, createSubUser, deleteSubUser } from './api-sub-users';

afterEach(() => {
	vi.unstubAllGlobals();
});

function portalUserWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		id: 'user-1',
		username: 'e2e-sub-user',
		display_name: 'E2E Sub User',
		is_main_account: false,
		enabled: true,
		...overrides
	};
}

describe('fetchSubUsers', () => {
	it('issues a GET to /auth/portal-users and maps every snake_case field to camelCase', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([portalUserWire()]), { status: 200 });
			})
		);
		const users = await fetchSubUsers();
		expect(calledUrl).toBe('/auth/portal-users');
		expect(users).toEqual([
			{ id: 'user-1', username: 'e2e-sub-user', displayName: 'E2E Sub User', isMainAccount: false, enabled: true }
		]);
	});

	it('throws ApiError with the real status code on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
		await expect(fetchSubUsers()).rejects.toMatchObject({ status: 500 });
	});
});

describe('createSubUser', () => {
	it('issues a POST to /auth/portal-users with a snake_case body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify(portalUserWire()), { status: 200 });
			})
		);
		const created = await createSubUser({
			username: 'e2e-sub-user',
			password: 'a-valid-password',
			displayName: 'E2E Sub User',
			isMainAccount: false
		});
		expect(calledUrl).toBe('/auth/portal-users');
		expect(calledInit?.method).toBe('POST');
		expect(JSON.parse(calledInit?.body as string)).toEqual({
			username: 'e2e-sub-user',
			password: 'a-valid-password',
			display_name: 'E2E Sub User',
			is_main_account: false
		});
		expect(created.username).toBe('e2e-sub-user');
	});

	it('throws ApiError with status 409 on a duplicate username', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'already exists' }), { status: 409 }))
		);
		await expect(
			createSubUser({ username: 'dup', password: 'a-valid-password', displayName: 'Dup', isMainAccount: false })
		).rejects.toMatchObject({ status: 409 });
	});
});

describe('deleteSubUser', () => {
	it('issues a DELETE to /auth/portal-users/{id} and does not attempt to parse a body', async () => {
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
		await deleteSubUser('user-1');
		expect(calledUrl).toBe('/auth/portal-users/user-1');
		expect(calledInit?.method).toBe('DELETE');
	});

	it('throws ApiError with status 400 on a self-delete rejection', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'cannot delete your own account' }), { status: 400 }))
		);
		await expect(deleteSubUser('self-id')).rejects.toMatchObject({ status: 400 });
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-sub-users.test.ts`
Expected: FAIL — `./api-sub-users` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `api-sub-users.ts`**

```typescript
// Frontend/src/lib/api-sub-users.ts
// Thin typed REST layer for /settings/sub-users. Wire shape verified directly against
// Backend/crates/api-gateway/src/routes/portal_users.rs (PortalUserSummary/CreatePortalUser) —
// snake_case throughout, no rename_all anywhere in api-gateway.
import { apiPost, ApiError } from './api';

export type PortalUser = {
	id: string;
	username: string;
	displayName: string;
	isMainAccount: boolean;
	enabled: boolean;
};

type PortalUserWire = {
	id: string;
	username: string;
	display_name: string;
	is_main_account: boolean;
	enabled: boolean;
};

function fromWire(wire: PortalUserWire): PortalUser {
	return {
		id: wire.id,
		username: wire.username,
		displayName: wire.display_name,
		isMainAccount: wire.is_main_account,
		enabled: wire.enabled
	};
}

export type CreateSubUserInput = {
	username: string;
	password: string;
	displayName: string;
	isMainAccount: boolean;
};

export async function fetchSubUsers(): Promise<PortalUser[]> {
	const res = await fetch('/auth/portal-users', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch sub-users');
	const wire: PortalUserWire[] = await res.json();
	return wire.map(fromWire);
}

export async function createSubUser(input: CreateSubUserInput): Promise<PortalUser> {
	const wire = await apiPost<PortalUserWire>('/auth/portal-users', {
		username: input.username,
		password: input.password,
		display_name: input.displayName,
		is_main_account: input.isMainAccount
	});
	return fromWire(wire);
}

/** `DELETE /auth/portal-users/{id}` returns `204 No Content` on success — never call
 * `res.json()` on this response, there is no body to parse. */
export async function deleteSubUser(id: string): Promise<void> {
	const res = await fetch(`/auth/portal-users/${id}`, { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to delete sub-user');
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-sub-users.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/api-sub-users.ts Frontend/src/lib/api-sub-users.test.ts
git commit -m "feat(frontend): api-sub-users.ts — typed REST layer for /auth/portal-users"
```

---

### Task 3: `/settings` shell — append "Sub-user" nav entry, correct stale comment

**Files:**
- Modify: `Frontend/src/routes/(app)/settings/+layout.svelte`

**Interfaces:**
- Consumes: nothing new.
- Produces: the `NAV_ITEMS` array Task 4's `/settings/sub-users` page renders inside.

**Current file content** (for exact context — you are modifying this, not creating it fresh):

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

- [ ] **Step 1: Correct the stale comment and append the "Sub-user" entry**

Replace everything from the top comment through the closing `</script>` tag with:

```svelte
<!-- Shared shell for every /settings/* sub-route. NAV_ITEMS is a flat array with a per-item
     mainAccountOnly flag, filtered by data.user.is_main_account. Fase 7j correction: Sub-users
     is OPEN like Branding/Locations (GET /auth/portal-users has no permission gate, only
     POST/DELETE do), NOT main-account-only like Bot — an earlier comment here got this wrong.
     Scales cleanly to Fase 7k (SPX Credentials, also open) without further restructuring. No
     placeholder entries for resources that don't exist yet, matching TopNav.svelte's own
     established convention. -->
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { children, data }: LayoutProps = $props();

	type NavItem = { href: string; label: string; mainAccountOnly?: boolean };

	const ALL_NAV_ITEMS: NavItem[] = [
		{ href: '/settings/branding', label: 'Branding' },
		{ href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
		{ href: '/settings/locations', label: 'Lokasi' },
		{ href: '/settings/sub-users', label: 'Sub-user' }
	];

	const NAV_ITEMS = $derived(ALL_NAV_ITEMS.filter((item) => !item.mainAccountOnly || data.user.is_main_account));
</script>
```

Leave the markup below the `</script>` tag exactly as-is — it already iterates `NAV_ITEMS` correctly and needs no changes.

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings. (`/settings/sub-users` doesn't exist yet until Task 4 — the new nav link's href being momentarily a 404 is expected and harmless.)

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/+layout.svelte"
git commit -m "feat(frontend): /settings shell — append Sub-user nav entry, correct stale RBAC comment"
```

---

### Task 4: `/settings/sub-users/+page.svelte` — page assembly

**Files:**
- Create: `Frontend/src/routes/(app)/settings/sub-users/+page.svelte`

**Interfaces:**
- Consumes: `PortalUser`, `fetchSubUsers`, `createSubUser`, `deleteSubUser` from `$lib/api-sub-users` (Task 2). `validatePassword`, `isSelf` from `$lib/sub-users` (Task 1). `ApiError` from `$lib/api`. `data.user.username`/`data.user.is_main_account` from the ambient `(app)/+layout.server.ts` data.
- Produces: nothing further — leaf page.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/settings/sub-users/+page.svelte -->
<!-- Create/delete management page for the tenant's portal_users. GET /auth/portal-users has no
     permission gate (any authenticated session sees the real list), only POST/DELETE are
     main-account-gated — so this page is edit-gated like /settings/branding and
     /settings/locations, never content-gated like /settings/bot. The backend also enforces a
     self-lockout guard on DELETE (a main account cannot delete their own row) — since the
     frontend has no portal_user id to compare (only username, from /auth/me), that row's delete
     button is disabled via a username match (isSelf), with an inline note explaining why. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import { Eye, EyeOff } from '@lucide/svelte';
	import type { PageProps } from './$types';
	import { fetchSubUsers, createSubUser, deleteSubUser, type PortalUser } from '$lib/api-sub-users';
	import { validatePassword, isSelf } from '$lib/sub-users';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let subUsers = $state<PortalUser[]>([]);
	let username = $state('');
	let password = $state('');
	let displayName = $state('');
	let makeMainAccount = $state(false);
	let showPassword = $state(false);
	let loading = $state(true);
	let creating = $state(false);
	let deletingId = $state<string | null>(null);
	let errorMsg = $state('');

	const passwordError = $derived(password === '' ? null : validatePassword(password));
	const canSubmit = $derived(
		username.trim() !== '' && displayName.trim() !== '' && password !== '' && validatePassword(password) === null
	);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			subUsers = await fetchSubUsers();
		} catch {
			errorMsg = 'Gagal memuat sub-user.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function handleCreate() {
		if (!canSubmit) return;
		creating = true;
		errorMsg = '';
		try {
			const created = await createSubUser({
				username: username.trim(),
				password,
				displayName: displayName.trim(),
				isMainAccount: makeMainAccount
			});
			subUsers = [...subUsers, created];
			username = '';
			password = '';
			displayName = '';
			makeMainAccount = false;
		} catch (err) {
			errorMsg =
				err instanceof ApiError && err.status === 409 ? 'Username ini sudah dipakai.' : 'Gagal membuat sub-user.';
		} finally {
			creating = false;
		}
	}

	async function handleDelete(subUser: PortalUser) {
		if (!confirm(`Hapus akun "${subUser.username}"?`)) return;
		deletingId = subUser.id;
		errorMsg = '';
		try {
			await deleteSubUser(subUser.id);
			subUsers = subUsers.filter((u) => u.id !== subUser.id);
		} catch {
			errorMsg = 'Gagal menghapus sub-user.';
		} finally {
			deletingId = null;
		}
	}
</script>

<svelte:head>
	<title>Sub-user — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else}
		{#if readOnly}
			<div
				role="alert"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
			>
				Hanya akun utama yang dapat mengelola sub-user.
			</div>
		{/if}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}

		<fieldset disabled={readOnly} class="flex flex-col gap-3 border-0 p-0">
			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Username</span>
				<input
					type="text"
					bind:value={username}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Password</span>
				<div class="relative">
					<input
						type={showPassword ? 'text' : 'password'}
						bind:value={password}
						autocomplete="new-password"
						class="w-full min-h-[40px] px-2.5 pr-12 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
					<button
						type="button"
						onclick={() => (showPassword = !showPassword)}
						aria-pressed={showPassword}
						class="absolute inset-y-0 right-0 flex items-center px-3 min-w-[44px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-md"
					>
						<span class="sr-only">{showPassword ? 'Sembunyikan password' : 'Tampilkan password'}</span>
						{#if showPassword}
							<EyeOff size={16} aria-hidden="true" />
						{:else}
							<Eye size={16} aria-hidden="true" />
						{/if}
					</button>
				</div>
				{#if passwordError}
					<span class="text-[11px] text-danger">{passwordError}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nama Tampilan</span>
				<input
					type="text"
					bind:value={displayName}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<label class="flex items-center gap-2">
				<input type="checkbox" bind:checked={makeMainAccount} class="h-4 w-4 accent-accent" />
				<span class="text-[13px] font-body text-text-primary">Jadikan akun utama</span>
			</label>

			<button
				type="button"
				onclick={handleCreate}
				disabled={creating || !canSubmit}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{creating ? 'Membuat…' : 'Buat Sub-user'}
			</button>
		</fieldset>

		{#if subUsers.length === 0}
			<p class="text-[13px] text-text-muted">Belum ada sub-user.</p>
		{:else}
			<ul class="flex flex-col gap-2">
				{#each subUsers as subUser (subUser.id)}
					{@const self = isSelf(subUser.username, data.user.username)}
					<li class="flex items-center justify-between gap-2 rounded-lg border border-border bg-bg-surface p-3">
						<div class="flex flex-col gap-0.5">
							<span class="text-[13px] font-body text-text-primary">
								{subUser.displayName}
								<span class="text-text-muted">({subUser.username})</span>
								{#if subUser.isMainAccount}
									<span
										class="text-[10px] px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-muted uppercase"
									>
										Akun Utama
									</span>
								{/if}
							</span>
							{#if self}
								<span class="text-[11px] text-text-muted">Tidak bisa menghapus akun sendiri.</span>
							{/if}
						</div>
						<button
							type="button"
							disabled={readOnly || self || deletingId === subUser.id}
							onclick={() => handleDelete(subUser)}
							class="min-h-[36px] px-2 text-[11px] text-danger disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							{deletingId === subUser.id ? 'Menghapus…' : 'Hapus'}
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
git add "Frontend/src/routes/(app)/settings/sub-users/+page.svelte"
git commit -m "feat(frontend): /settings/sub-users — page assembly (create form, self-lockout-aware delete)"
```

---

### Task 5: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/settings-sub-users.spec.ts`

**Interfaces:**
- Consumes: the full `/settings` shell (Task 3) + `/settings/sub-users` page (Task 4) built in Tasks 1-4. No new frontend code — this task authors real-stack e2e coverage and runs full verification.

**No new seed users needed.** Reuses `e2e-test-user` (main-account) and `e2e-readonly-user` (non-main-account) from Fase 7a/7d, both already seeded — and this suite is the first to actually list them via the real UI, so they double as the "self-lockout row" fixture for free. **No shared-fixture risk beyond "don't delete the seeded login accounts"**: every sub-user this suite creates gets a `Date.now()`-suffixed unique username and is deleted by the end of its own test, same self-cleaning discipline as Fase 7i's Locations suite. The self-lockout guard itself is an extra safety net against ever accidentally deleting `e2e-test-user` through this UI.

- [ ] **Step 1: Write `Frontend/tests/settings-sub-users.spec.ts`**

```typescript
// Frontend/tests/settings-sub-users.spec.ts
//
// REAL end-to-end proof of Fase 7j's /settings/sub-users page. Same real-stack setup as
// tests/login.spec.ts, tests/settings-locations.spec.ts — real reactor-core on :8081 behind
// Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432). Nothing here is mocked.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d. Every sub-user THIS suite creates gets a unique Date.now()-suffixed
// username and is deleted by the end of its own test — this suite never creates a row it
// doesn't clean up, and never attempts to delete the seeded e2e-test-user/e2e-readonly-user rows
// other suites' logins depend on (the self-lockout guard is an extra safety net for the former).

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /settings/sub-users redirects to /login', async ({ page }) => {
	await page.goto('/settings/sub-users');
	await expect(page).toHaveURL(/\/login/);
});

test('main account sees the Sub-user nav entry, the real list, and their own row has a disabled delete button', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByRole('link', { name: 'Sub-user' })).toBeVisible();

	await page.goto('/settings/sub-users');
	await expect(page.getByText('Tidak bisa menghapus akun sendiri.')).toBeVisible({ timeout: 10_000 });
	const selfRow = page.locator('li', { hasText: 'e2e-test-user' });
	await expect(selfRow.getByRole('button', { name: 'Hapus' })).toBeDisabled();
});

test('creating a sub-user with a valid password persists it, then it can be deleted', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/sub-users');
	await expect(page.getByLabel('Username')).toBeVisible({ timeout: 10_000 });

	const uniqueUsername = `e2e-sub-user-${Date.now()}`;
	await page.getByLabel('Username').fill(uniqueUsername);
	await page.getByLabel('Password').fill('a-valid-password-123');
	await page.getByLabel('Nama Tampilan').fill('E2E Created Sub-user');
	await page.getByRole('button', { name: 'Buat Sub-user' }).click();
	await expect(page.getByText(uniqueUsername)).toBeVisible({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText(uniqueUsername)).toBeVisible({ timeout: 10_000 });

	page.once('dialog', (dialog) => dialog.accept());
	await page.locator('li', { hasText: uniqueUsername }).getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(uniqueUsername)).toBeHidden({ timeout: 10_000 });
});

test('a duplicate username shows the specific 409 message', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/sub-users');
	await expect(page.getByLabel('Username')).toBeVisible({ timeout: 10_000 });

	const uniqueUsername = `e2e-dup-sub-user-${Date.now()}`;
	await page.getByLabel('Username').fill(uniqueUsername);
	await page.getByLabel('Password').fill('a-valid-password-123');
	await page.getByLabel('Nama Tampilan').fill('E2E Dup Sub-user');
	await page.getByRole('button', { name: 'Buat Sub-user' }).click();
	await expect(page.getByText(uniqueUsername)).toBeVisible({ timeout: 10_000 });

	await page.getByLabel('Username').fill(uniqueUsername);
	await page.getByLabel('Password').fill('another-valid-password');
	await page.getByLabel('Nama Tampilan').fill('E2E Dup Sub-user 2');
	await page.getByRole('button', { name: 'Buat Sub-user' }).click();
	await expect(page.getByText('Username ini sudah dipakai.')).toBeVisible({ timeout: 10_000 });

	// Clean up the one real row this test created.
	page.once('dialog', (dialog) => dialog.accept());
	await page.locator('li', { hasText: uniqueUsername }).getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(uniqueUsername)).toBeHidden({ timeout: 10_000 });
});

test('a too-short password shows an inline error and never issues a create request', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/sub-users');
	await expect(page.getByLabel('Username')).toBeVisible({ timeout: 10_000 });

	let postCount = 0;
	await page.route('**/auth/portal-users', (route) => {
		if (route.request().method() === 'POST') postCount++;
		route.continue();
	});

	await page.getByLabel('Username').fill(`e2e-shortpw-${Date.now()}`);
	await page.getByLabel('Password').fill('short');
	await page.getByLabel('Nama Tampilan').fill('E2E Short Password');
	await expect(page.getByText('Password minimal 8 karakter')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Buat Sub-user' })).toBeDisabled();
	expect(postCount).toBe(0);
});

test('non-main-account session sees the real list with disabled create-form and delete controls', async ({
	page
}) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/sub-users');

	const usernameInput = page.getByLabel('Username');
	await expect(usernameInput).toBeVisible({ timeout: 10_000 });
	await expect(usernameInput).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Buat Sub-user' })).toBeDisabled();
});
```

- [ ] **Step 2: Run the new e2e file alone**

Run: `cd Frontend && pnpm exec playwright test tests/settings-sub-users.spec.ts --workers=1`
Expected: all tests pass (a live `reactor-core` + `tower-postgres` stack must already be running).

- [ ] **Step 3: Run the full Playwright suite (regression check)**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across every prior suite plus `settings-sub-users.spec.ts` pass. **If any pre-existing test fails showing a still-on-`/login` symptom, check for the known shared-`reactor-core` login rate-limiter flake (see Fase 7f/7g/7h/7i's own notes) before assuming a regression** — restart `reactor-core` and rerun the failing file alone to confirm.

- [ ] **Step 4: Full backend verification**

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green. This task makes no backend changes, so this is a pure regression check. **Run these as normal foreground commands and wait for their actual output — do not background them and lose track of results** (a repeated hiccup in every prior phase's Task 5 implementer runs — Fase 7f, 7g, 7h, and 7i all hit this).

- [ ] **Step 5: Full frontend verification**

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `sub-users.test.ts`, Task 2's `api-sub-users.test.ts`, plus every pre-existing suite — no regression); production build succeeds.

- [ ] **Step 6: Commit**

```bash
git add Frontend/tests/settings-sub-users.spec.ts
git commit -m "test(fase-7j): /settings/sub-users e2e — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — create form with password show/hide (Task 4), self-lockout-aware delete (Task 4), always-visible nav entry (Task 3), edit-gating with individually-disabled controls plus a from-the-start read-only banner (Task 4). Every "Out of scope" bullet (edit/rename/enable-disable, password strength meter, displaying `enabled`, cross-account main-account-count safeguards) has no corresponding task, and the Global Constraints section explains why.

**Placeholder scan:** no TBD/TODO. Every code block is complete, runnable content.

**Type consistency:** `PortalUser`/`CreateSubUserInput` (Task 2) — `{id, username, displayName, isMainAccount, enabled}` (+`password` on the create-input variant) — is the exact shape threaded unchanged through Task 4's page state and Task 5's e2e assertions. `validatePassword`/`isSelf` (Task 1) signatures match exactly how Task 4 calls them.

**Cross-task dependency ordering:** 1 (pure logic) and 2 (REST layer) are independent of each other → 3 (shell nav append, depends on neither) → 4 (page, depends on 1, 2, and 3's route existing) → 5 (e2e, depends on everything). No task references a later task's output.

**The RBAC-shape correction is the one genuinely new risk in this plan — verified twice, not assumed.** Both this design doc's own brainstorming AND this plan's Global Constraints independently re-read `portal_users.rs` (not just restated a prior, wrong assumption baked into Fase 7h/7i's own code comments) to confirm `GET` is ungated. Task 3 explicitly corrects the stale comment text so a future reader doesn't propagate the same wrong assumption into Fase 7k.

**Self-lockout is the second genuinely new risk, handled by construction, not by hoping the e2e catches it**: `isSelf` is a small, directly unit-tested pure function (Task 1), and Task 4's list rendering calls it per-row rather than trying to special-case "the first row" or any other fragile heuristic — the disabled state and the explanatory note are driven by the exact same boolean.
