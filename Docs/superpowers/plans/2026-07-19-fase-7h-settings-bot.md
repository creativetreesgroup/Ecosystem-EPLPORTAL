# Fase 7h: `/settings/bot` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `/settings/bot`, a form over the already-built `GET/PUT /bot/settings` endpoint (WAHA/n8n bot configuration), and extend the Fase 7g `/settings` shell so the nav entry only appears for main-account sessions. Pure frontend build.

**Architecture:** Unlike Fase 7g's Branding page (edit-gated, always visible), this resource is content-gated: `GET /bot/settings` itself requires main-account, so a non-main-account session never sees the "Bot" nav entry, and a direct-navigation attempt gets a clean "no access" message driven by a real `403` from the fetch. The write-only `waha_api_key` field is always blank on load (the backend never echoes it) with a dynamic placeholder explaining the blank-means-keep-existing / required-on-first-setup semantics.

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-19-fase-7h-settings-bot-design.md` — read it first for full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format is snake_case** — no `#[serde(rename_all)]` anywhere in `api-gateway` (re-verified against `Backend/crates/api-gateway/src/routes/bot.rs` for this plan).
- **Both `GET` and `PUT /bot/settings` require `Permission::ManageBotSettings`** (main-account only) — the one deliberate exception to this crate's "GET = any session" convention. A `GET` from a non-main-account session returns `403`, not `200` with masked data.
- **`waha_api_key` is write-only and never round-tripped.** The response (`BotSettingsResponse`) only ever carries `waha_api_key_set: bool`. A blank/whitespace `waha_api_key` in the `PUT` body means "keep the existing key" — UNLESS no key has ever been configured for this tenant, in which case the backend 400s with `"waha_api_key is required on first setup"`. Client-side, mirror this as an inline validation error (not a round-trip), gated on `waha_api_key_set` from the last fetch/save response.
- **No client-side SSRF validation.** `waha_url`/`webhook_url` get only a basic well-formed-`http`/`https`-URL syntax check client-side (empty string is valid — both fields are optional). The backend's `is_safe_outbound_url` (host-blocklist: localhost, private/link-local IPs, `.internal`/`.local`/`metadata.goog` hostnames, credentials-in-URL) is the sole security boundary — do not attempt to replicate any part of it client-side.
- **No "test connection" feature** — no backend endpoint exists for it. Out of scope.
- **The dev tenant's `waha_settings` row is shared, real state `Frontend/tests/rules.spec.ts`'s OTP arm-flow test depends on** (a genuinely-decryptable API key must exist, and `wa_number` must stay non-empty or `POST /auth/request-aa-otp` 400s with "OTP delivery is not configured for this tenant"). Task 5's e2e tests MUST leave this row's `wa_number`/`waha_url`/`waha_session`/`enabled`/`webhook_url`/`wa_group` values exactly as found after any test that touches them — see Task 5 for the exact restore pattern. Rotating the API key itself to a new test value is safe and needs no restore (nothing else in this codebase ever validates the key's actual content — OTP delivery to `http://127.0.0.1:19999` fails harmlessly regardless of what the key is).
- **`onMount(load)`, never a bare top-level `load()` call** — this app runs SSR (`adapter-node`, no `ssr = false` anywhere) and `fetchBotSettings()`'s relative-path `fetch` has no origin during Node SSR. This exact bug was caught by Fase 7g's Task 4 review — do not reintroduce it.
- **Accessibility bar (established 7a-7g convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error/forbidden banners, `role="status" aria-live="polite"` for save success, a `<svelte:head><title>Bot — TOWER</title></svelte:head>` (Fase 7g's whole-branch review flagged a missing page title as a Minor — get it right from the start this time), "…" ellipsis character for loading/saving text (not "...", another Fase 7g Minor to not repeat).
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `bot-settings.ts` — pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/bot-settings.ts`
- Test: `Frontend/src/lib/bot-settings.test.ts`

**Interfaces:**
- Produces: `isValidUrlFormat(value: string): boolean`. `apiKeyError(hasExistingKey: boolean, enteredKey: string): string | null` (returns an error message, or `null` if valid).

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/bot-settings.test.ts
import { describe, it, expect } from 'vitest';
import { isValidUrlFormat, apiKeyError } from './bot-settings';

describe('isValidUrlFormat', () => {
	it('accepts a well-formed https URL', () => {
		expect(isValidUrlFormat('https://waha.example.com:3000')).toBe(true);
	});

	it('accepts a well-formed http URL', () => {
		expect(isValidUrlFormat('http://127.0.0.1:19999')).toBe(true);
	});

	it('treats an empty string as valid (both fields are optional)', () => {
		expect(isValidUrlFormat('')).toBe(true);
	});

	it('treats a whitespace-only string as valid', () => {
		expect(isValidUrlFormat('   ')).toBe(true);
	});

	it('rejects a malformed string', () => {
		expect(isValidUrlFormat('not a url')).toBe(false);
	});

	it('rejects a non-http(s) scheme', () => {
		expect(isValidUrlFormat('ftp://example.com')).toBe(false);
	});
});

describe('apiKeyError', () => {
	it('returns an error when no key exists yet and the input is blank', () => {
		expect(apiKeyError(false, '')).not.toBeNull();
	});

	it('returns an error when no key exists yet and the input is whitespace-only', () => {
		expect(apiKeyError(false, '   ')).not.toBeNull();
	});

	it('returns null when no key exists yet but a real value is entered', () => {
		expect(apiKeyError(false, 'a-real-key')).toBeNull();
	});

	it('returns null when a key already exists and the input is blank (keep existing)', () => {
		expect(apiKeyError(true, '')).toBeNull();
	});

	it('returns null when a key already exists and a new value is entered (rotation)', () => {
		expect(apiKeyError(true, 'new-key')).toBeNull();
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/bot-settings.test.ts`
Expected: FAIL — `./bot-settings` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `bot-settings.ts`**

```typescript
// Frontend/src/lib/bot-settings.ts
// Pure logic for /settings/bot — no fetch, no DOM. `isValidUrlFormat` is deliberately a syntax-
// only check (empty string allowed, both URL fields are optional) — the real security boundary
// (SSRF host-blocklist) is exclusively backend (`is_safe_outbound_url`,
// Backend/crates/api-gateway/src/routes/bot.rs) and is NOT duplicated here; see this plan's
// Global Constraints for why.

export function isValidUrlFormat(value: string): boolean {
	const trimmed = value.trim();
	if (trimmed === '') return true;
	try {
		const url = new URL(trimmed);
		return url.protocol === 'http:' || url.protocol === 'https:';
	} catch {
		return false;
	}
}

/** Mirrors the backend's own first-setup requirement (`Backend/crates/api-gateway/src/routes/
 * bot.rs::put_settings`: a blank `waha_api_key` 400s with "waha_api_key is required on first
 * setup" when no key has ever been configured) as an inline client-side check, so the user gets
 * immediate feedback instead of a round-trip. */
export function apiKeyError(hasExistingKey: boolean, enteredKey: string): string | null {
	if (!hasExistingKey && enteredKey.trim() === '') {
		return 'Wajib diisi (setup pertama)';
	}
	return null;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/bot-settings.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/bot-settings.ts Frontend/src/lib/bot-settings.test.ts
git commit -m "feat(frontend): bot-settings.ts — pure logic (URL format check, API key validation)"
```

---

### Task 2: `api-bot-settings.ts` — typed REST layer

**Files:**
- Create: `Frontend/src/lib/api-bot-settings.ts`
- Test: `Frontend/src/lib/api-bot-settings.test.ts`

**Interfaces:**
- Consumes: nothing from Task 1 (independent module).
- Produces: `type BotSettings = { enabled: boolean; webhookUrl: string; waNumber: string; waGroup: string; wahaUrl: string; wahaSession: string; wahaApiKeySet: boolean }`. `type BotSettingsInput = BotSettings & { wahaApiKey: string }`. `fetchBotSettings(): Promise<BotSettings>`. `saveBotSettings(input: BotSettingsInput): Promise<BotSettings>`.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/api-bot-settings.test.ts
// vi.stubGlobal('fetch', ...) regression guards for the load-bearing HTTP details this module
// has: PUT (not POST) with an exact snake_case body, full field round-trip mapping, and — the
// one detail unique to this resource — a blank `waha_api_key` must serialize as an actual blank
// STRING in the request body (not be omitted), matching what the backend's "keep existing"
// branch expects to see on the wire.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchBotSettings, saveBotSettings } from './api-bot-settings';

afterEach(() => {
	vi.unstubAllGlobals();
});

function botSettingsWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		enabled: true,
		webhook_url: 'https://n8n.example.com/webhook',
		wa_number: '628111111111',
		wa_group: '',
		waha_url: 'http://127.0.0.1:19999',
		waha_session: 'default',
		waha_api_key_set: true,
		...overrides
	};
}

describe('fetchBotSettings', () => {
	it('issues a GET to /bot/settings and maps every snake_case field to camelCase', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify(botSettingsWire()), { status: 200 });
			})
		);
		const settings = await fetchBotSettings();
		expect(calledUrl).toBe('/bot/settings');
		expect(settings).toEqual({
			enabled: true,
			webhookUrl: 'https://n8n.example.com/webhook',
			waNumber: '628111111111',
			waGroup: '',
			wahaUrl: 'http://127.0.0.1:19999',
			wahaSession: 'default',
			wahaApiKeySet: true
		});
	});

	it('throws ApiError with the real status code on a non-ok response (e.g. 403)', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 403 })));
		await expect(fetchBotSettings()).rejects.toMatchObject({ status: 403 });
	});
});

describe('saveBotSettings', () => {
	it('issues a PUT (not POST) with a snake_case body matching BotSettingsRequest exactly', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify(botSettingsWire()), { status: 200 });
			})
		);
		await saveBotSettings({
			enabled: true,
			webhookUrl: 'https://n8n.example.com/webhook',
			waNumber: '628111111111',
			waGroup: '',
			wahaUrl: 'http://127.0.0.1:19999',
			wahaSession: 'default',
			wahaApiKeySet: true,
			wahaApiKey: ''
		});
		expect(calledUrl).toBe('/bot/settings');
		expect(calledInit?.method).toBe('PUT');
		expect(JSON.parse(calledInit?.body as string)).toEqual(botSettingsWire());
	});

	it('sends a blank waha_api_key as an actual empty string, not omitted, when keeping the existing key', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, init?: RequestInit) => {
				const body = JSON.parse(init?.body as string);
				expect(body).toHaveProperty('waha_api_key', '');
				return new Response(JSON.stringify(botSettingsWire()), { status: 200 });
			})
		);
		await saveBotSettings({
			enabled: true,
			webhookUrl: '',
			waNumber: '',
			waGroup: '',
			wahaUrl: '',
			wahaSession: '',
			wahaApiKeySet: true,
			wahaApiKey: ''
		});
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 400 })));
		await expect(
			saveBotSettings({
				enabled: false,
				webhookUrl: '',
				waNumber: '',
				waGroup: '',
				wahaUrl: '',
				wahaSession: '',
				wahaApiKeySet: false,
				wahaApiKey: ''
			})
		).rejects.toThrow();
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-bot-settings.test.ts`
Expected: FAIL — `./api-bot-settings` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `api-bot-settings.ts`**

```typescript
// Frontend/src/lib/api-bot-settings.ts
// Thin typed REST layer for /settings/bot. Wire shape verified directly against
// Backend/crates/api-gateway/src/routes/bot.rs (BotSettingsResponse/BotSettingsRequest) —
// snake_case throughout, no rename_all anywhere in api-gateway. Distinct resource from
// Frontend/src/lib/api-activity.ts's fetchBotLogs/clearBotLogs (that's /bot/logs, this is
// /bot/settings — same backend `/bot` prefix, different endpoints, no naming collision).
import { ApiError } from './api';

export type BotSettings = {
	enabled: boolean;
	webhookUrl: string;
	waNumber: string;
	waGroup: string;
	wahaUrl: string;
	wahaSession: string;
	wahaApiKeySet: boolean;
};

export type BotSettingsInput = BotSettings & { wahaApiKey: string };

type BotSettingsWire = {
	enabled: boolean;
	webhook_url: string;
	wa_number: string;
	wa_group: string;
	waha_url: string;
	waha_session: string;
	waha_api_key_set: boolean;
};

function fromWire(wire: BotSettingsWire): BotSettings {
	return {
		enabled: wire.enabled,
		webhookUrl: wire.webhook_url,
		waNumber: wire.wa_number,
		waGroup: wire.wa_group,
		wahaUrl: wire.waha_url,
		wahaSession: wire.waha_session,
		wahaApiKeySet: wire.waha_api_key_set
	};
}

export async function fetchBotSettings(): Promise<BotSettings> {
	const res = await fetch('/bot/settings', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch bot settings');
	const wire: BotSettingsWire = await res.json();
	return fromWire(wire);
}

/** `apiPost` (Frontend/src/lib/api.ts) hardcodes `method: 'POST'` — the backend route is
 * `PUT /bot/settings` (Backend/crates/api-gateway/src/routes/bot.rs's `bot_router`), so this
 * cannot use `apiPost`; a POST here would 405. Raw `fetch` with `method: 'PUT'`, same header/
 * credentials/error shape as `apiPost` otherwise. */
export async function saveBotSettings(input: BotSettingsInput): Promise<BotSettings> {
	const res = await fetch('/bot/settings', {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({
			enabled: input.enabled,
			webhook_url: input.webhookUrl,
			wa_number: input.waNumber,
			wa_group: input.waGroup,
			waha_url: input.wahaUrl,
			waha_session: input.wahaSession,
			waha_api_key: input.wahaApiKey
		})
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save bot settings');
	const wire: BotSettingsWire = await res.json();
	return fromWire(wire);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-bot-settings.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/api-bot-settings.ts Frontend/src/lib/api-bot-settings.test.ts
git commit -m "feat(frontend): api-bot-settings.ts — typed REST layer for /bot/settings"
```

---

### Task 3: `/settings` shell — conditional "Bot" nav entry

**Files:**
- Modify: `Frontend/src/routes/(app)/settings/+layout.svelte`

**Interfaces:**
- Consumes: `data.user.is_main_account` (ambient, from `(app)/+layout.server.ts` — same convention already used by `/rules`/`/price`/`/activity`/`/settings/branding`).
- Produces: nothing new — this is the shared shell every `/settings/*` page (including Task 4's `/settings/bot`) renders inside.

**Current file content** (for exact context — you are modifying this, not creating it fresh):

```svelte
<!-- Shared shell for every /settings/* sub-route. The nav array grows by one entry per future
     sub-phase (Bot/WAHA config, Locations, Sub-users, SPX Credentials — Fase 7h-7k) — no
     placeholder entries for resources that don't exist yet, matching TopNav.svelte's own
     established convention of not building UI for not-yet-built surfaces. -->
<script lang="ts">
	import { page } from '$app/state';
	import type { Snippet } from 'svelte';

	let { children }: { children: Snippet } = $props();

	const NAV_ITEMS = [{ href: '/settings/branding', label: 'Branding' }];
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

- [ ] **Step 1: Make `NAV_ITEMS` conditional on `data.user.is_main_account`**

Replace the `<script>` block with:

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
```

Leave the markup below the `</script>` tag exactly as-is — it already iterates `NAV_ITEMS`, no changes needed there.

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings. (`/settings/bot` doesn't exist yet until Task 4 — the `href` being momentarily a 404 for a main-account session is expected and harmless at this point in the plan; svelte-check does not verify route existence.)

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/+layout.svelte"
git commit -m "feat(frontend): /settings shell — hide Bot nav entry for non-main-account sessions"
```

---

### Task 4: `/settings/bot/+page.svelte` — page assembly

**Files:**
- Create: `Frontend/src/routes/(app)/settings/bot/+page.svelte`

**Interfaces:**
- Consumes: `BotSettings`, `fetchBotSettings`, `saveBotSettings` from `$lib/api-bot-settings` (Task 2). `isValidUrlFormat`, `apiKeyError` from `$lib/bot-settings` (Task 1). `ApiError` from `$lib/api` (its `.status` property is used to detect a `403`). `data.user` is available ambiently but this page does NOT need `is_main_account` directly — the nav-hiding (Task 3) and the `403`-driven forbidden state below are the only two RBAC surfaces, matching the "content-gated, not edit-gated" design decision.
- Produces: nothing further — leaf page.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/settings/bot/+page.svelte -->
<!-- Bot/WAHA configuration form. GET/PUT /bot/settings are BOTH Permission::ManageBotSettings-
     gated (main-account only) — unlike /settings/branding, this page is content-gated, not just
     edit-gated: there is no read-only view for non-main-account. The normal path is the "Bot"
     nav entry simply not existing for them (+layout.svelte, Task 3); this page's own forbidden
     state handles the case where a non-main-account session reaches this URL directly anyway —
     fetchBotSettings() genuinely 403s, and that's shown as a clear message, not a raw error. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchBotSettings, saveBotSettings, type BotSettings } from '$lib/api-bot-settings';
	import { isValidUrlFormat, apiKeyError } from '$lib/bot-settings';
	import { ApiError } from '$lib/api';

	let { data: _data }: PageProps = $props();

	function emptySettings(): BotSettings {
		return {
			enabled: false,
			webhookUrl: '',
			waNumber: '',
			waGroup: '',
			wahaUrl: '',
			wahaSession: '',
			wahaApiKeySet: false
		};
	}

	let settings = $state<BotSettings>(emptySettings());
	let lastSaved = $state<BotSettings>(emptySettings());
	let apiKeyInput = $state('');
	let loading = $state(true);
	let saving = $state(false);
	let forbidden = $state(false);
	let errorMsg = $state('');
	let successMsg = $state('');

	const dirty = $derived(JSON.stringify(settings) !== JSON.stringify(lastSaved) || apiKeyInput.trim() !== '');
	const apiKeyErrorMsg = $derived(apiKeyError(settings.wahaApiKeySet, apiKeyInput));
	const wahaUrlErrorMsg = $derived(isValidUrlFormat(settings.wahaUrl) ? null : 'URL tidak valid');
	const webhookUrlErrorMsg = $derived(isValidUrlFormat(settings.webhookUrl) ? null : 'URL tidak valid');
	const hasFormErrors = $derived(
		apiKeyErrorMsg !== null || wahaUrlErrorMsg !== null || webhookUrlErrorMsg !== null
	);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const result = await fetchBotSettings();
			settings = result;
			lastSaved = { ...result };
		} catch (err) {
			if (err instanceof ApiError && err.status === 403) {
				forbidden = true;
			} else {
				errorMsg =
					err instanceof ApiError ? `Gagal memuat pengaturan bot: ${err.message}` : 'Gagal memuat pengaturan bot';
			}
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function handleSave() {
		if (hasFormErrors) return;
		saving = true;
		errorMsg = '';
		successMsg = '';
		try {
			const result = await saveBotSettings({ ...settings, wahaApiKey: apiKeyInput });
			settings = result;
			lastSaved = { ...result };
			apiKeyInput = '';
			successMsg = 'Pengaturan bot tersimpan.';
		} catch (err) {
			errorMsg = err instanceof ApiError ? `Gagal menyimpan: ${err.message}` : 'Gagal menyimpan pengaturan bot';
		} finally {
			saving = false;
		}
	}
</script>

<svelte:head>
	<title>Bot — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else if forbidden}
		<p role="alert" aria-live="polite" class="text-[13px] text-danger">
			Anda tidak memiliki akses ke halaman ini.
		</p>
	{:else}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}
		{#if successMsg}
			<p role="status" aria-live="polite" class="text-[13px] text-accent">{successMsg}</p>
		{/if}

		<label class="flex items-center gap-2">
			<input type="checkbox" bind:checked={settings.enabled} class="h-4 w-4 accent-accent" />
			<span class="text-[13px] font-body text-text-primary">Aktifkan integrasi bot</span>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Webhook URL</span>
			<input
				type="text"
				bind:value={settings.webhookUrl}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			{#if webhookUrlErrorMsg}
				<span class="text-[11px] text-danger">{webhookUrlErrorMsg}</span>
			{/if}
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nomor WhatsApp (OTP)</span>
			<input
				type="text"
				bind:value={settings.waNumber}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Grup WhatsApp</span>
			<input
				type="text"
				bind:value={settings.waGroup}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">WAHA URL</span>
			<input
				type="text"
				bind:value={settings.wahaUrl}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			{#if wahaUrlErrorMsg}
				<span class="text-[11px] text-danger">{wahaUrlErrorMsg}</span>
			{/if}
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">WAHA Session</span>
			<input
				type="text"
				bind:value={settings.wahaSession}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">WAHA API Key</span>
			<input
				type="password"
				bind:value={apiKeyInput}
				placeholder={settings.wahaApiKeySet ? 'Biarkan kosong untuk tidak mengubah' : 'Wajib diisi (setup pertama)'}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			{#if apiKeyErrorMsg}
				<span class="text-[11px] text-danger">{apiKeyErrorMsg}</span>
			{/if}
		</label>

		<button
			type="button"
			onclick={handleSave}
			disabled={saving || !dirty || hasFormErrors}
			class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			{saving ? 'Menyimpan…' : 'Simpan Perubahan'}
		</button>
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/bot/+page.svelte"
git commit -m "feat(frontend): /settings/bot — page assembly (content-gated form, masked API key)"
```

---

### Task 5: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/settings-bot.spec.ts`

**Interfaces:**
- Consumes: the full `/settings` shell (Task 3) + `/settings/bot` page (Task 4) built in Tasks 1-4. No new frontend code — this task authors real-stack e2e coverage and runs full verification.

**No new seed users needed.** Reuses `e2e-test-user` (main-account) and `e2e-readonly-user` (non-main-account) from Fase 7a/7d, both already seeded. **No new data seed needed either** — the dev tenant's `waha_settings` row already exists (seeded by an earlier phase for `rules.spec.ts`'s OTP test) with a genuinely-decryptable API key, non-empty `wa_number`, and `waha_url` pointing at `http://127.0.0.1:19999` (nothing listens there; delivery failures are non-fatal). **This row is shared, real state other suites depend on — read this task's Global Constraints section above before writing any test that saves a change.**

- [ ] **Step 1: Write `Frontend/tests/settings-bot.spec.ts`**

```typescript
// Frontend/tests/settings-bot.spec.ts
//
// REAL end-to-end proof of Fase 7h's /settings/bot page. Same real-stack setup as
// tests/login.spec.ts, tests/settings-branding.spec.ts — real reactor-core on :8081 behind
// Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432). Nothing here is mocked.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d. The dev tenant's site_settings.waha_settings row already exists (seeded
// for rules.spec.ts's OTP arm-flow test) with a genuinely-decryptable API key and a non-empty
// wa_number — Frontend/tests/rules.spec.ts's OTP test depends on this row staying functionally
// intact (wa_number non-empty, in particular, or POST /auth/request-aa-otp 400s). Every test
// below that changes a non-key field restores it afterward; rotating the API key itself needs
// no restore (nothing else in this codebase ever validates the key's actual decrypted content).

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /settings/bot redirects to /login', async ({ page }) => {
	await page.goto('/settings/bot');
	await expect(page).toHaveURL(/\/login/);
});

test('non-main-account session does not see the Bot nav entry, and direct navigation shows a forbidden message', async ({
	page
}) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByRole('link', { name: 'Bot' })).toBeHidden();

	await page.goto('/settings/bot');
	await expect(page.getByText('Anda tidak memiliki akses ke halaman ini.')).toBeVisible({ timeout: 10_000 });
});

test('main account sees the Bot nav entry and the real existing config, with the API key field blank', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByRole('link', { name: 'Bot' })).toBeVisible();

	await page.goto('/settings/bot');
	const waNumberInput = page.getByLabel('Nomor WhatsApp (OTP)');
	await expect(waNumberInput).toBeVisible({ timeout: 10_000 });
	await expect(waNumberInput).not.toHaveValue('');

	const apiKeyInput = page.getByLabel('WAHA API Key');
	await expect(apiKeyInput).toHaveValue('');
	await expect(apiKeyInput).toHaveAttribute('placeholder', 'Biarkan kosong untuk tidak mengubah');
});

test('editing wa_group and saving persists the change, then restores the original value', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/bot');

	const waGroupInput = page.getByLabel('Grup WhatsApp');
	await expect(waGroupInput).toBeVisible({ timeout: 10_000 });
	const originalWaGroup = await waGroupInput.inputValue();

	const testValue = `e2e-test-group-${Date.now()}`;
	await waGroupInput.fill(testValue);
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByText('Pengaturan bot tersimpan.')).toBeVisible({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByLabel('Grup WhatsApp')).toHaveValue(testValue, { timeout: 10_000 });

	// Restore — this row is shared with rules.spec.ts's OTP test.
	await page.getByLabel('Grup WhatsApp').fill(originalWaGroup);
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByText('Pengaturan bot tersimpan.')).toBeVisible({ timeout: 10_000 });
	await page.reload();
	await expect(page.getByLabel('Grup WhatsApp')).toHaveValue(originalWaGroup, { timeout: 10_000 });
});

test('entering a new API key rotates it successfully, leaving every other field untouched', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/bot');

	const waNumberInput = page.getByLabel('Nomor WhatsApp (OTP)');
	await expect(waNumberInput).toBeVisible({ timeout: 10_000 });
	const originalWaNumber = await waNumberInput.inputValue();

	// Deliberately touch ONLY the API key field — every other field stays exactly as loaded, so
	// this save is a safe no-op for wa_number/waha_url/etc. and needs no restore step.
	await page.getByLabel('WAHA API Key').fill(`e2e-rotated-key-${Date.now()}`);
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByText('Pengaturan bot tersimpan.')).toBeVisible({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByLabel('WAHA API Key')).toHaveValue('');
	await expect(page.getByLabel('WAHA API Key')).toHaveAttribute('placeholder', 'Biarkan kosong untuk tidak mengubah');
	await expect(page.getByLabel('Nomor WhatsApp (OTP)')).toHaveValue(originalWaNumber);
});

test('an invalid WAHA URL shows an inline error and never issues a save request', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/bot');
	await expect(page.getByLabel('Nomor WhatsApp (OTP)')).toBeVisible({ timeout: 10_000 });

	let putCount = 0;
	await page.route('**/bot/settings', (route) => {
		if (route.request().method() === 'PUT') putCount++;
		route.continue();
	});

	await page.getByLabel('WAHA URL').fill('not a url');
	await expect(page.getByText('URL tidak valid')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled();

	// Restore before the test ends, since the field is now dirty with an invalid value.
	await page.getByLabel('WAHA URL').fill('http://127.0.0.1:19999');
	expect(putCount).toBe(0);
});
```

- [ ] **Step 2: Run the new e2e file alone**

Run: `cd Frontend && pnpm exec playwright test tests/settings-bot.spec.ts --workers=1`
Expected: all tests pass, IN ORDER (the wa_group and API-key tests share the same backend row — `--workers=1` avoids any cross-test race on that shared state within this file; a live `reactor-core` + `tower-postgres` stack must already be running).

- [ ] **Step 3: Run `rules.spec.ts` to confirm the shared `waha_settings` row is still intact**

Run: `cd Frontend && pnpm exec playwright test tests/rules.spec.ts --workers=1`
Expected: all tests pass, INCLUDING the OTP arm-flow test — this specifically confirms Task 5's restore steps left the shared row in a working state. If this fails with an OTP-request 400 ("OTP delivery is not configured"), the restore step in `settings-bot.spec.ts` did not work — fix it before proceeding, do not just note it as a known flake.

- [ ] **Step 4: Run the full Playwright suite (regression check)**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across every prior suite plus `settings-bot.spec.ts` pass. **If any pre-existing test fails showing a still-on-`/login` symptom, check for the known shared-`reactor-core` login rate-limiter flake (see Fase 7f/7g's own notes) before assuming a regression** — restart `reactor-core` and rerun the failing file alone to confirm.

- [ ] **Step 5: Full backend verification**

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green. This task makes no backend changes, so this is a pure regression check.

- [ ] **Step 6: Full frontend verification**

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `bot-settings.test.ts`, Task 2's `api-bot-settings.test.ts`, plus every pre-existing suite — no regression); production build succeeds.

- [ ] **Step 7: Commit**

```bash
git add Frontend/tests/settings-bot.spec.ts
git commit -m "test(fase-7h): /settings/bot e2e — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — content-gated Bot page (Task 4), conditional nav entry (Task 3), masked write-only API key UI (Task 4), direct-navigation forbidden state (Task 4). Every "Out of scope" bullet (test-connection button, client-side SSRF validation) has no corresponding task, and the Global Constraints section explicitly calls out why not, so no implementer reinvents either.

**Placeholder scan:** no TBD/TODO. Every code block is complete, runnable content.

**Type consistency:** `BotSettings`/`BotSettingsInput` (Task 2) — `{enabled, webhookUrl, waNumber, waGroup, wahaUrl, wahaSession, wahaApiKeySet}` (+`wahaApiKey` on the input variant) — is the exact shape threaded unchanged through Task 4's page state and Task 5's e2e assertions. `isValidUrlFormat`/`apiKeyError` (Task 1) signatures match exactly how Task 4 calls them.

**Cross-task dependency ordering:** 1 (pure logic) and 2 (REST layer) are independent of each other → 3 (shell nav change, depends on neither) → 4 (page, depends on 1, 2, and 3's route existing) → 5 (e2e, depends on everything). No task references a later task's output.

**The shared-row risk is the one genuinely novel complexity in this plan** (Branding/Fase 7g had no equivalent — its resource was purpose-built for this phase, not pre-seeded shared state another suite depends on). Task 5 handles it three ways: (1) the wa_group test explicitly captures-and-restores; (2) the API-key-rotation test is designed to touch ONLY the key field, needing no restore since nothing else in the field set changes; (3) Step 3 explicitly re-runs `rules.spec.ts` as a direct confirmation the row is still healthy, rather than assuming the restore logic worked.

**A genuine judgment call worth flagging to the human if this recurs:** this is the first `/settings/*` sub-phase whose e2e tests must coordinate with another phase's pre-existing shared fixture. If Fase 7i-7k's resources turn out to have similar shared-state dependencies, this restore-and-reverify pattern (capture original → mutate → assert → restore → reverify via the OTHER suite) is worth calling out explicitly as this project's established approach, not reinvented per phase.
