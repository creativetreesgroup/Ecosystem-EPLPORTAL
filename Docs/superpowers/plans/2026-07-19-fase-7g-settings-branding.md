# Fase 7g: `/settings` shell + `/settings/branding` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the shared `/settings` shell (secondary nav, reused by four future sub-phases) plus its first real page, `/settings/branding` — a singleton form over the already-built `GET/PUT /branding` endpoint. Pure frontend build.

**Architecture:** `(app)/settings/+layout.svelte` renders a secondary nav (currently one entry: Branding) around `{@render children()}`; bare `/settings` redirects to `/settings/branding` (same `+page.server.ts`-throws-`redirect()` pattern as the site root, `Frontend/src/routes/+page.server.ts`). `/settings/branding/+page.svelte` is a single form: text fields bound directly to a `$state` `Branding` object, two file inputs (logo/favicon) that validate-then-read-to-data-URI entirely client-side before any field changes, one Save button that PUTs the whole object. Non-main-account sessions get the same real data in a `<fieldset disabled>` cascade — identical pattern to `/rules`/`/price`.

**Tech Stack:** SvelteKit 5 (runes), TypeScript, Tailwind v4 (tokens-only), Vitest, Playwright.

**Design doc:** `Docs/superpowers/specs/2026-07-19-fase-7g-settings-branding-design.md` — read it first for full rationale; this plan only restates what's needed to implement each task.

## Global Constraints

- **REST wire format is snake_case** — no `#[serde(rename_all)]` anywhere in `api-gateway` (re-verified against `Backend/crates/api-gateway/src/branding.rs` and `routes/branding.rs` for this plan).
- **`GET /branding` has NO permission gate** — any authenticated session can view current values (it's even public/unauthenticated at the backend, though this phase only reaches it from inside the authenticated `(app)` group). Only `PUT /branding` requires `Permission::ManageBranding` (main-account only). This means the Branding page itself is never content-gated (unlike Fase 7f's Log Bot tab) — it's always visible, just read-only for non-main-account.
- **Client-side validation limits MUST exactly match `Backend/crates/api-gateway/src/branding.rs`**: `title` required, ≤60 chars (`TITLE_MAX`). `subtitle` ≤160 chars (`SUBTITLE_MAX`), blank allowed. `site_name` ≤60 chars (`SITE_NAME_MAX`); blank is allowed client-side — the backend silently falls back to its own default at save time, this is not a client-side error. `brand_tag` ≤20 chars (`BRAND_TAG_MAX`). `logo_data_uri`/`favicon_data_uri`: PNG/JPEG/WEBP only (SVG/ICO rejected — SVG can carry executable script), ≤5MB each (`LOGO_MAX_BYTES`/`FAVICON_MAX_BYTES`, both `5 * 1024 * 1024`).
- **A `File`'s raw byte size (`file.size`) IS the "decoded size" the backend checks** — base64 decoding is lossless, so no size-inflation math is needed client-side; compare `file.size` directly against the 5MB cap.
- **No `FileReader`-based encoding.** This project's Vitest config (`Frontend/vite.config.ts`) has no `jsdom`/`happy-dom` environment configured (Node's default `node` environment lacks a global `FileReader`). Use `File.arrayBuffer()` (standard Web API, available in both real browsers and Node's built-in `File`/`Blob`) plus the global `btoa` (also available in both) to build the data URI — this keeps `fileToDataUri` unit-testable without adding a new test-environment dependency.
- **Bare `/settings` must redirect, not render an empty shell.** Follow `Frontend/src/routes/+page.server.ts`'s exact established pattern: a trivial `+page.svelte` (`<!-- redirects server-side, see +page.server.ts -->`) paired with a `+page.server.ts` whose `load` unconditionally calls `redirect(307, '/settings/branding')`.
- **No placeholder nav entries for Bot/WAHA, Locations, Sub-users, or SPX Credentials.** The shell's nav array lists exactly one entry (Branding) until each resource's own future sub-phase (7h-7k) adds itself — matches `TopNav.svelte`'s own established convention of not building UI for not-yet-built surfaces.
- **Accessibility bar (established 7a-7f convention):** tokens-only styling from `Frontend/src/app.css`'s `@theme` block, `min-h-[44px]` primary tap targets, `focus-visible:ring-2 focus-visible:ring-accent`, `role="alert" aria-live="polite"` error banners, `role="status" aria-live="polite"` for the save-success message, every image preview gets real descriptive `alt` text, native `<fieldset disabled>` cascade for read-only (no per-field `readOnly` prop threading).
- **No backend changes in this plan.** If any task's implementer believes one is needed, that is a plan contradiction — stop and escalate, do not silently add backend code.

---

### Task 1: `branding.ts` — pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/branding.ts`
- Test: `Frontend/src/lib/branding.test.ts`

**Interfaces:**
- Produces: `TITLE_MAX`, `SUBTITLE_MAX`, `SITE_NAME_MAX`, `BRAND_TAG_MAX`, `IMAGE_MAX_BYTES`, `ALLOWED_IMAGE_TYPES: string[]` (constants). `type BrandingFormErrors = { title?: string; subtitle?: string; siteName?: string; brandTag?: string }`. `validateBrandingForm(form: { title: string; subtitle: string; siteName: string; brandTag: string }): BrandingFormErrors`. `validateImageFile(file: File): string | null` (returns an error message, or `null` if valid). `fileToDataUri(file: File): Promise<string>`.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/branding.test.ts
import { describe, it, expect } from 'vitest';
import {
	validateBrandingForm,
	validateImageFile,
	fileToDataUri,
	TITLE_MAX,
	SUBTITLE_MAX,
	SITE_NAME_MAX,
	BRAND_TAG_MAX,
	IMAGE_MAX_BYTES
} from './branding';

describe('validateBrandingForm', () => {
	const valid = { title: 'My Title', subtitle: '', siteName: '', brandTag: '' };

	it('accepts a minimal valid form (title only)', () => {
		expect(validateBrandingForm(valid)).toEqual({});
	});

	it('rejects an empty title', () => {
		expect(validateBrandingForm({ ...valid, title: '' }).title).toBeDefined();
	});

	it('rejects a whitespace-only title', () => {
		expect(validateBrandingForm({ ...valid, title: '   ' }).title).toBeDefined();
	});

	it('rejects a title over the max length', () => {
		expect(validateBrandingForm({ ...valid, title: 'a'.repeat(TITLE_MAX + 1) }).title).toBeDefined();
	});

	it('accepts a title at exactly the max length', () => {
		expect(validateBrandingForm({ ...valid, title: 'a'.repeat(TITLE_MAX) }).title).toBeUndefined();
	});

	it('allows a blank site_name (backend falls back to its own default at save time, not a client error)', () => {
		expect(validateBrandingForm({ ...valid, siteName: '' }).siteName).toBeUndefined();
	});

	it('rejects subtitle/site_name/brand_tag over their max lengths', () => {
		expect(validateBrandingForm({ ...valid, subtitle: 'a'.repeat(SUBTITLE_MAX + 1) }).subtitle).toBeDefined();
		expect(validateBrandingForm({ ...valid, siteName: 'a'.repeat(SITE_NAME_MAX + 1) }).siteName).toBeDefined();
		expect(validateBrandingForm({ ...valid, brandTag: 'a'.repeat(BRAND_TAG_MAX + 1) }).brandTag).toBeDefined();
	});
});

describe('validateImageFile', () => {
	function makeFile(type: string, size: number): File {
		return new File([new Uint8Array(size)], 'test-file', { type });
	}

	it('accepts a valid PNG under the size cap', () => {
		expect(validateImageFile(makeFile('image/png', 1024))).toBeNull();
	});

	it('accepts JPEG and WEBP', () => {
		expect(validateImageFile(makeFile('image/jpeg', 1024))).toBeNull();
		expect(validateImageFile(makeFile('image/webp', 1024))).toBeNull();
	});

	it('rejects an SVG (matches backend: SVG/ICO can carry executable script)', () => {
		expect(validateImageFile(makeFile('image/svg+xml', 1024))).not.toBeNull();
	});

	it('rejects a file over IMAGE_MAX_BYTES', () => {
		expect(validateImageFile(makeFile('image/png', IMAGE_MAX_BYTES + 1))).not.toBeNull();
	});

	it('accepts a file at exactly IMAGE_MAX_BYTES', () => {
		expect(validateImageFile(makeFile('image/png', IMAGE_MAX_BYTES))).toBeNull();
	});
});

describe('fileToDataUri', () => {
	it('encodes a file into a correctly-prefixed base64 data URI, round-tripping the exact bytes', async () => {
		const bytes = new Uint8Array([137, 80, 78, 71]); // PNG magic bytes
		const file = new File([bytes], 'test.png', { type: 'image/png' });
		const uri = await fileToDataUri(file);
		expect(uri.startsWith('data:image/png;base64,')).toBe(true);
		const base64 = uri.slice('data:image/png;base64,'.length);
		expect(Buffer.from(base64, 'base64').equals(Buffer.from(bytes))).toBe(true);
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/branding.test.ts`
Expected: FAIL — `./branding` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `branding.ts`**

```typescript
// Frontend/src/lib/branding.ts
// Pure logic for /settings/branding — no fetch, no DOM. Every limit below MUST match
// Backend/crates/api-gateway/src/branding.rs exactly (re-verified against that file while
// writing this plan). Wire-format mapping lives in api-branding.ts, matching the established
// $lib "logic vs. REST layer" split from prior phases.

export const TITLE_MAX = 60;
export const SUBTITLE_MAX = 160;
export const SITE_NAME_MAX = 60;
export const BRAND_TAG_MAX = 20;
export const IMAGE_MAX_BYTES = 5 * 1024 * 1024;
export const ALLOWED_IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/webp'];

export type BrandingFormErrors = {
	title?: string;
	subtitle?: string;
	siteName?: string;
	brandTag?: string;
};

export function validateBrandingForm(form: {
	title: string;
	subtitle: string;
	siteName: string;
	brandTag: string;
}): BrandingFormErrors {
	const errors: BrandingFormErrors = {};

	const title = form.title.trim();
	if (!title) {
		errors.title = 'Judul wajib diisi';
	} else if (title.length > TITLE_MAX) {
		errors.title = `Judul maksimal ${TITLE_MAX} karakter`;
	}

	if (form.subtitle.trim().length > SUBTITLE_MAX) {
		errors.subtitle = `Subjudul maksimal ${SUBTITLE_MAX} karakter`;
	}

	// Blank site_name is allowed here — the backend falls back to its own default at save time
	// (Backend/crates/api-gateway/src/branding.rs::validate_and_normalize), it is not an error.
	if (form.siteName.trim().length > SITE_NAME_MAX) {
		errors.siteName = `Nama situs maksimal ${SITE_NAME_MAX} karakter`;
	}

	if (form.brandTag.trim().length > BRAND_TAG_MAX) {
		errors.brandTag = `Brand tag maksimal ${BRAND_TAG_MAX} karakter`;
	}

	return errors;
}

/** Returns an error message, or `null` if the file passes. Checked entirely from `File.type`/
 * `File.size` — no image is ever read or sent until this returns `null`. */
export function validateImageFile(file: File): string | null {
	if (!ALLOWED_IMAGE_TYPES.includes(file.type)) {
		return 'Format harus PNG, JPEG, atau WEBP';
	}
	if (file.size > IMAGE_MAX_BYTES) {
		return 'Ukuran gambar maksimal 5MB';
	}
	return null;
}

/** `File.arrayBuffer()` + `btoa`, NOT `FileReader` — both are available in real browsers AND in
 * Node's test environment (no jsdom/happy-dom needed), see this plan's Global Constraints. */
export async function fileToDataUri(file: File): Promise<string> {
	const buffer = await file.arrayBuffer();
	const bytes = new Uint8Array(buffer);
	let binary = '';
	for (const byte of bytes) {
		binary += String.fromCharCode(byte);
	}
	const base64 = btoa(binary);
	return `data:${file.type};base64,${base64}`;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/branding.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/branding.ts Frontend/src/lib/branding.test.ts
git commit -m "feat(frontend): branding.ts — pure logic (validation + file-to-data-URI)"
```

---

### Task 2: `api-branding.ts` — typed REST layer

**Files:**
- Create: `Frontend/src/lib/api-branding.ts`
- Test: `Frontend/src/lib/api-branding.test.ts`

**Interfaces:**
- Consumes: nothing from Task 1 (independent module, per this project's established `activity.ts`/`api-activity.ts` split — the REST layer doesn't import the pure-logic module).
- Produces: `type Branding = { title: string; subtitle: string; siteName: string; brandTag: string; logoDataUri: string | null; faviconDataUri: string | null }`. `fetchBranding(): Promise<Branding>`. `saveBranding(branding: Branding): Promise<Branding>`.

- [ ] **Step 1: Write the failing test**

```typescript
// Frontend/src/lib/api-branding.test.ts
// vi.stubGlobal('fetch', ...) regression guards for the two load-bearing HTTP details this
// module has: PUT (not POST — apiPost hardcodes POST, this route is PUT) with an exact
// snake_case body, and full field round-trip mapping — same precedent as api-activity.ts's own
// fetch-mock guards.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchBranding, saveBranding } from './api-branding';

afterEach(() => {
	vi.unstubAllGlobals();
});

function brandingWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		title: 'My Title',
		subtitle: 'My Subtitle',
		site_name: 'My Site',
		brand_tag: 'TAG',
		logo_data_uri: null,
		favicon_data_uri: null,
		...overrides
	};
}

describe('fetchBranding', () => {
	it('issues a GET to /branding and maps every snake_case field to camelCase', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify(brandingWire()), { status: 200 });
			})
		);
		const branding = await fetchBranding();
		expect(calledUrl).toBe('/branding');
		expect(branding).toEqual({
			title: 'My Title',
			subtitle: 'My Subtitle',
			siteName: 'My Site',
			brandTag: 'TAG',
			logoDataUri: null,
			faviconDataUri: null
		});
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
		await expect(fetchBranding()).rejects.toThrow();
	});
});

describe('saveBranding', () => {
	it('issues a PUT (not POST) with a snake_case body matching BrandingInput exactly', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify(brandingWire()), { status: 200 });
			})
		);
		await saveBranding({
			title: 'My Title',
			subtitle: 'My Subtitle',
			siteName: 'My Site',
			brandTag: 'TAG',
			logoDataUri: null,
			faviconDataUri: null
		});
		expect(calledUrl).toBe('/branding');
		expect(calledInit?.method).toBe('PUT');
		expect(JSON.parse(calledInit?.body as string)).toEqual(brandingWire());
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 403 })));
		await expect(
			saveBranding({ title: 'x', subtitle: '', siteName: '', brandTag: '', logoDataUri: null, faviconDataUri: null })
		).rejects.toThrow();
	});
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd Frontend && pnpm vitest run src/lib/api-branding.test.ts`
Expected: FAIL — `./api-branding` has no exported members (module doesn't exist yet).

- [ ] **Step 3: Implement `api-branding.ts`**

```typescript
// Frontend/src/lib/api-branding.ts
// Thin typed REST layer for /settings/branding. Wire shape verified directly against
// Backend/crates/api-gateway/src/branding.rs (Branding/BrandingInput) — snake_case throughout,
// no rename_all anywhere in api-gateway.
import { ApiError } from './api';

export type Branding = {
	title: string;
	subtitle: string;
	siteName: string;
	brandTag: string;
	logoDataUri: string | null;
	faviconDataUri: string | null;
};

type BrandingWire = {
	title: string;
	subtitle: string;
	site_name: string;
	brand_tag: string;
	logo_data_uri: string | null;
	favicon_data_uri: string | null;
};

function fromWire(wire: BrandingWire): Branding {
	return {
		title: wire.title,
		subtitle: wire.subtitle,
		siteName: wire.site_name,
		brandTag: wire.brand_tag,
		logoDataUri: wire.logo_data_uri,
		faviconDataUri: wire.favicon_data_uri
	};
}

function toWire(branding: Branding): BrandingWire {
	return {
		title: branding.title,
		subtitle: branding.subtitle,
		site_name: branding.siteName,
		brand_tag: branding.brandTag,
		logo_data_uri: branding.logoDataUri,
		favicon_data_uri: branding.faviconDataUri
	};
}

export async function fetchBranding(): Promise<Branding> {
	const res = await fetch('/branding', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch branding');
	const wire: BrandingWire = await res.json();
	return fromWire(wire);
}

/** `apiPost` (Frontend/src/lib/api.ts) hardcodes `method: 'POST'` — the backend route is
 * `PUT /branding` (Backend/crates/api-gateway/src/routes/branding.rs's `branding_router`), so
 * this cannot use `apiPost`; a POST here would 405. Raw `fetch` with `method: 'PUT'`, same
 * header/credentials/error shape as `apiPost` otherwise. */
export async function saveBranding(branding: Branding): Promise<Branding> {
	const res = await fetch('/branding', {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(toWire(branding))
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save branding');
	const wire: BrandingWire = await res.json();
	return fromWire(wire);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd Frontend && pnpm vitest run src/lib/api-branding.test.ts`
Expected: PASS, all tests green.

- [ ] **Step 5: Run svelte-check and commit**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

```bash
git add Frontend/src/lib/api-branding.ts Frontend/src/lib/api-branding.test.ts
git commit -m "feat(frontend): api-branding.ts — typed REST layer for /branding"
```

---

### Task 3: `/settings` shell (secondary nav + bare-route redirect)

**Files:**
- Create: `Frontend/src/routes/(app)/settings/+layout.svelte`
- Create: `Frontend/src/routes/(app)/settings/+page.svelte`
- Create: `Frontend/src/routes/(app)/settings/+page.server.ts`

**Interfaces:**
- Consumes: nothing from Task 1/2 — this task is pure routing/shell, no data fetching.
- Produces: the `{@render children()}` slot every future `/settings/*` sub-route (starting with Task 4's `/settings/branding`) renders into.

- [ ] **Step 1: Write the shell layout**

```svelte
<!-- Frontend/src/routes/(app)/settings/+layout.svelte -->
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

- [ ] **Step 2: Write the bare-route redirect**

```svelte
<!-- Frontend/src/routes/(app)/settings/+page.svelte -->
<!-- redirects server-side, see +page.server.ts -->
```

```typescript
// Frontend/src/routes/(app)/settings/+page.server.ts
// Bare /settings has no content of its own — same established pattern as the site root
// (Frontend/src/routes/+page.server.ts): always redirect, this time to the one nav entry that
// exists today. Update this redirect if a future sub-phase ever changes what "first" means.
import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async () => {
	redirect(307, '/settings/branding');
};
```

- [ ] **Step 3: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings. (`/settings/branding` doesn't exist yet until Task 4 — the `href`/redirect target being momentarily a 404 is expected and harmless at this point in the plan; svelte-check does not verify route existence.)

- [ ] **Step 4: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/+layout.svelte" "Frontend/src/routes/(app)/settings/+page.svelte" "Frontend/src/routes/(app)/settings/+page.server.ts"
git commit -m "feat(frontend): /settings shell — secondary nav + bare-route redirect"
```

---

### Task 4: `/settings/branding/+page.svelte` — page assembly

**Files:**
- Create: `Frontend/src/routes/(app)/settings/branding/+page.svelte`

**Interfaces:**
- Consumes: `Branding`, `fetchBranding`, `saveBranding` from `$lib/api-branding` (Task 2). `validateBrandingForm`, `validateImageFile`, `fileToDataUri`, `TITLE_MAX`, `SUBTITLE_MAX`, `SITE_NAME_MAX`, `BRAND_TAG_MAX` from `$lib/branding` (Task 1). `ApiError` from `$lib/api`. `data.user.is_main_account` from the ambient `(app)/+layout.server.ts` data (same convention as `/rules`/`/price`/`/activity`).
- Produces: nothing further — this is a leaf page.

- [ ] **Step 1: Write the page**

```svelte
<!-- Frontend/src/routes/(app)/settings/branding/+page.svelte -->
<!-- Single-form settings page for the tenant's Branding record. GET /branding has no permission
     gate (any authenticated session sees real current values), only PUT is main-account-gated
     (Permission::ManageBranding) — so this page is never content-gated, unlike Fase 7f's Log Bot
     tab; non-main-account instead gets the same data behind a native <fieldset disabled>
     cascade, identical pattern to /rules'/`/price`'s RuleRow/PriceRow. -->
<script lang="ts">
	import type { PageProps } from './$types';
	import { fetchBranding, saveBranding, type Branding } from '$lib/api-branding';
	import {
		validateBrandingForm,
		validateImageFile,
		fileToDataUri,
		TITLE_MAX,
		SUBTITLE_MAX,
		SITE_NAME_MAX,
		BRAND_TAG_MAX
	} from '$lib/branding';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	function emptyBranding(): Branding {
		return { title: '', subtitle: '', siteName: '', brandTag: '', logoDataUri: null, faviconDataUri: null };
	}

	let branding = $state<Branding>(emptyBranding());
	let lastSaved = $state<Branding>(emptyBranding());
	let loading = $state(true);
	let saving = $state(false);
	let errorMsg = $state('');
	let successMsg = $state('');
	let logoError = $state('');
	let faviconError = $state('');

	const dirty = $derived(JSON.stringify(branding) !== JSON.stringify(lastSaved));
	const formErrors = $derived(validateBrandingForm(branding));
	const hasFormErrors = $derived(Object.keys(formErrors).length > 0);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const result = await fetchBranding();
			branding = result;
			lastSaved = result;
		} catch (err) {
			errorMsg = err instanceof ApiError ? `Gagal memuat branding: ${err.message}` : 'Gagal memuat branding';
		} finally {
			loading = false;
		}
	}

	load();

	async function handleSave() {
		if (hasFormErrors) return;
		saving = true;
		errorMsg = '';
		successMsg = '';
		try {
			const result = await saveBranding(branding);
			branding = result;
			lastSaved = result;
			successMsg = 'Branding tersimpan.';
		} catch (err) {
			errorMsg = err instanceof ApiError ? `Gagal menyimpan: ${err.message}` : 'Gagal menyimpan branding';
		} finally {
			saving = false;
		}
	}

	async function handleLogoSelect(e: Event) {
		logoError = '';
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		const err = validateImageFile(file);
		if (err) {
			logoError = err;
			input.value = '';
			return;
		}
		branding.logoDataUri = await fileToDataUri(file);
		input.value = '';
	}

	async function handleFaviconSelect(e: Event) {
		faviconError = '';
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		const err = validateImageFile(file);
		if (err) {
			faviconError = err;
			input.value = '';
			return;
		}
		branding.faviconDataUri = await fileToDataUri(file);
		input.value = '';
	}
</script>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat...</p>
	{:else}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}
		{#if successMsg}
			<p role="status" aria-live="polite" class="text-[13px] text-accent">{successMsg}</p>
		{/if}
		<fieldset disabled={readOnly} class="flex flex-col gap-4 border-0 p-0">
			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Judul</span>
				<input
					type="text"
					bind:value={branding.title}
					maxlength={TITLE_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.title}
					<span class="text-[11px] text-danger">{formErrors.title}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Subjudul</span>
				<input
					type="text"
					bind:value={branding.subtitle}
					maxlength={SUBTITLE_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.subtitle}
					<span class="text-[11px] text-danger">{formErrors.subtitle}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nama Situs</span>
				<input
					type="text"
					bind:value={branding.siteName}
					maxlength={SITE_NAME_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.siteName}
					<span class="text-[11px] text-danger">{formErrors.siteName}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Brand Tag</span>
				<input
					type="text"
					bind:value={branding.brandTag}
					maxlength={BRAND_TAG_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.brandTag}
					<span class="text-[11px] text-danger">{formErrors.brandTag}</span>
				{/if}
			</label>

			<div class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Logo</span>
				{#if branding.logoDataUri}
					<img
						src={branding.logoDataUri}
						alt="Pratinjau logo situs"
						class="h-16 w-16 object-contain rounded border border-border bg-bg-base"
					/>
				{/if}
				<div class="flex items-center gap-2">
					<input
						type="file"
						accept="image/png,image/jpeg,image/webp"
						onchange={handleLogoSelect}
						aria-label="Unggah logo"
						class="text-[12px] text-text-muted"
					/>
					{#if branding.logoDataUri}
						<button
							type="button"
							onclick={() => (branding.logoDataUri = null)}
							class="text-[11px] text-danger min-h-[36px] px-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							Hapus
						</button>
					{/if}
				</div>
				{#if logoError}
					<span class="text-[11px] text-danger">{logoError}</span>
				{/if}
			</div>

			<div class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Favicon</span>
				{#if branding.faviconDataUri}
					<img
						src={branding.faviconDataUri}
						alt="Pratinjau favicon situs"
						class="h-8 w-8 object-contain rounded border border-border bg-bg-base"
					/>
				{/if}
				<div class="flex items-center gap-2">
					<input
						type="file"
						accept="image/png,image/jpeg,image/webp"
						onchange={handleFaviconSelect}
						aria-label="Unggah favicon"
						class="text-[12px] text-text-muted"
					/>
					{#if branding.faviconDataUri}
						<button
							type="button"
							onclick={() => (branding.faviconDataUri = null)}
							class="text-[11px] text-danger min-h-[36px] px-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							Hapus
						</button>
					{/if}
				</div>
				{#if faviconError}
					<span class="text-[11px] text-danger">{faviconError}</span>
				{/if}
			</div>

			<button
				type="button"
				onclick={handleSave}
				disabled={saving || !dirty || hasFormErrors}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{saving ? 'Menyimpan...' : 'Simpan Perubahan'}
			</button>
		</fieldset>
	{/if}
</div>
```

- [ ] **Step 2: Run svelte-check**

Run: `cd Frontend && pnpm check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 3: Commit**

```bash
git add "Frontend/src/routes/(app)/settings/branding/+page.svelte"
git commit -m "feat(frontend): /settings/branding — page assembly (form, upload/preview, RBAC)"
```

---

### Task 5: E2E tests + final verification

**Files:**
- Create: `Frontend/tests/settings-branding.spec.ts`

**Interfaces:**
- Consumes: the full `/settings` shell + `/settings/branding` page built in Tasks 1-4. No new frontend code — this task authors real-stack e2e coverage and runs full verification.

**No new seed users needed.** Reuses `e2e-test-user` (main-account) and `e2e-readonly-user` (non-main-account) from Fase 7a/7d, both already seeded. **No new data seed needed either** — `Branding` is a per-tenant singleton (`site_settings` row), the test sets its own known values via the UI and asserts they persist, which is safe and idempotent to rerun (each run simply overwrites the row with the same values again).

- [ ] **Step 1: Write `Frontend/tests/settings-branding.spec.ts`**

```typescript
// Frontend/tests/settings-branding.spec.ts
//
// REAL end-to-end proof of Fase 7g's /settings shell + /settings/branding page. Same real-stack
// setup as tests/login.spec.ts, tests/rules.spec.ts, tests/price.spec.ts, tests/activity.spec.ts
// — real reactor-core on :8081 behind Vite's dev proxy, real Postgres (tower-postgres,
// 127.0.0.1:15432), real Redis unused by this page. Nothing here is mocked or stubbed.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d, no re-seeding needed. Branding is a per-tenant singleton with no
// append-only concerns (unlike Fase 7f's accept_events) — this suite sets known values and
// asserts they round-trip, safe to rerun any number of times.

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

// A minimal valid 1x1 transparent PNG, inlined so this suite needs no fixture file on disk.
const TINY_PNG_BASE64 =
	'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=';

test('unauthenticated visit to /settings redirects to /login', async ({ page }) => {
	await page.goto('/settings');
	await expect(page).toHaveURL(/\/login/);
});

test('unauthenticated visit to /settings/branding redirects to /login', async ({ page }) => {
	await page.goto('/settings/branding');
	await expect(page).toHaveURL(/\/login/);
});

test('bare /settings redirects to /settings/branding for an authenticated session', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings');
	await expect(page).toHaveURL(/\/settings\/branding/);
	await expect(page.getByRole('heading', { name: 'Settings' })).toBeVisible();
});

test('main account can edit title/site name, upload a real logo, save, and it persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');

	const titleInput = page.getByLabel('Judul');
	await expect(titleInput).toBeVisible({ timeout: 10_000 });

	const uniqueTitle = `Judul E2E ${Date.now()}`;
	await titleInput.fill(uniqueTitle);
	await page.getByLabel('Nama Situs').fill('Situs E2E');

	await page.getByLabel('Unggah logo').setInputFiles({
		name: 'logo.png',
		mimeType: 'image/png',
		buffer: Buffer.from(TINY_PNG_BASE64, 'base64')
	});
	await expect(page.getByAltText('Pratinjau logo situs')).toBeVisible();

	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByText('Branding tersimpan.')).toBeVisible({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByLabel('Judul')).toHaveValue(uniqueTitle, { timeout: 10_000 });
	await expect(page.getByLabel('Nama Situs')).toHaveValue('Situs E2E');
	await expect(page.getByAltText('Pratinjau logo situs')).toBeVisible();
});

test('non-main-account session sees the real data in a disabled read-only form', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');

	const titleInput = page.getByLabel('Judul');
	await expect(titleInput).toBeVisible({ timeout: 10_000 });
	await expect(titleInput).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled();
});

test('selecting an oversized logo shows an inline error and never issues a save request', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByLabel('Judul')).toBeVisible({ timeout: 10_000 });

	let putCount = 0;
	await page.route('**/branding', (route) => {
		if (route.request().method() === 'PUT') putCount++;
		route.continue();
	});

	const oversized = Buffer.alloc(6 * 1024 * 1024); // 6MB, over the 5MB cap
	await page.getByLabel('Unggah logo').setInputFiles({
		name: 'too-big.png',
		mimeType: 'image/png',
		buffer: oversized
	});

	await expect(page.getByText('Ukuran gambar maksimal 5MB')).toBeVisible();
	await expect(page.getByAltText('Pratinjau logo situs')).toBeHidden();
	expect(putCount).toBe(0);
});

test('selecting a wrong-type file (SVG) shows an inline error and never issues a save request', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByLabel('Judul')).toBeVisible({ timeout: 10_000 });

	let putCount = 0;
	await page.route('**/branding', (route) => {
		if (route.request().method() === 'PUT') putCount++;
		route.continue();
	});

	await page.getByLabel('Unggah logo').setInputFiles({
		name: 'evil.svg',
		mimeType: 'image/svg+xml',
		buffer: Buffer.from('<svg></svg>')
	});

	await expect(page.getByText('Format harus PNG, JPEG, atau WEBP')).toBeVisible();
	expect(putCount).toBe(0);
});
```

- [ ] **Step 2: Run the new e2e file alone**

Run: `cd Frontend && pnpm exec playwright test tests/settings-branding.spec.ts`
Expected: all tests pass (a live `reactor-core` + `tower-postgres` + `tower-redis` stack must already be running — see `tests/login.spec.ts`'s header comment for the exact env a manually-started `reactor-core` needs).

- [ ] **Step 3: Run the full Playwright suite (regression check)**

Run: `cd Frontend && pnpm exec playwright test`
Expected: all tests across every prior suite plus `settings-branding.spec.ts` pass. **If any pre-existing (not `settings-branding.spec.ts`) test fails showing a still-on-`/login` symptom, check for the known shared-`reactor-core` login rate-limiter flake (see `Docs/superpowers/specs/2026-07-19-fase-7f-activity-design.md`'s sibling project-memory note) before assuming a regression** — restart `reactor-core` and rerun the failing file alone to confirm.

- [ ] **Step 4: Full backend verification**

```bash
cd Backend
DATABASE_URL=postgres://tower:tower_dev_only@127.0.0.1:15432/tower REDIS_URL=redis://127.0.0.1:16379 \
  cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

Expected: all green. **Use the `tower` superuser URL for `cargo test`, not `app_role`.** This task makes no backend changes, so this is a pure regression check.

- [ ] **Step 5: Full frontend verification**

```bash
cd Frontend
pnpm check
pnpm vitest run
pnpm build
```

Expected: `svelte-check` 0 errors/0 warnings; all Vitest suites pass (Task 1's `branding.test.ts`, Task 2's `api-branding.test.ts`, plus every pre-existing suite — no regression); production build succeeds.

- [ ] **Step 6: Commit**

```bash
git add Frontend/tests/settings-branding.spec.ts
git commit -m "test(fase-7g): /settings/branding e2e — full workspace + frontend verification"
```

---

## Self-Review Notes (author's own pre-flight check, not a subagent dispatch)

**Spec coverage:** every "In scope" bullet from the design doc maps to a task — shared shell (Task 3), Branding form with all 6 fields (Task 4), read-only view for non-main-account (Task 4's `<fieldset disabled>`), upload-with-preview-and-remove for logo/favicon (Task 4). Every "Out of scope" bullet (Bot/WAHA/Locations/Sub-users/SPX-Credentials pages, placeholder nav entries, public-page consumption, TOWER's own chrome branding) has no corresponding task.

**Placeholder scan:** no TBD/TODO. Every code block is complete, runnable content — no "similar to Task N" references.

**Type consistency:** `Branding` (Task 2) — `{title, subtitle, siteName, brandTag, logoDataUri, faviconDataUri}` — is the exact shape threaded unchanged through Task 4's page state and e2e assertions. `BrandingFormErrors` (Task 1) keys (`title`/`subtitle`/`siteName`/`brandTag`) match exactly what Task 4 reads (`formErrors.title` etc.). `validateImageFile`/`fileToDataUri` signatures (Task 1) match exactly how Task 4 calls them (`(file: File) => string | null` / `(file: File) => Promise<string>`).

**Cross-task dependency ordering:** 1 (pure logic) and 2 (REST layer) are independent of each other (same split as `activity.ts`/`api-activity.ts`) → 3 (shell, depends on neither) → 4 (page, depends on 1, 2, and 3's route structure) → 5 (e2e, depends on everything). No task references a later task's output.

**A genuine, load-bearing environmental gotcha carried forward from Fase 7f:** this project's Vitest config has no `jsdom`/`happy-dom` environment, so `FileReader` (the obvious naive choice for file-to-data-URI encoding) is unavailable in unit tests. Task 1 uses `File.arrayBuffer()` + `btoa` instead — verified working in this repo's actual Node version during this plan's own research (`node -e` round-trip check), not just assumed from documentation.

**Test-value safety:** Task 5's persistence test uses a `Date.now()`-suffixed unique title specifically so reruns never collide with (or falsely appear to confirm) a previous run's leftover value — the assertion genuinely re-derives from what THIS run just saved, not a hardcoded string a stale row could accidentally satisfy.
