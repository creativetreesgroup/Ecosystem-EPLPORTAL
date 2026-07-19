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

	const titleInput = page.getByLabel('Judul', { exact: true });
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
	await expect(page.getByLabel('Judul', { exact: true })).toHaveValue(uniqueTitle, { timeout: 10_000 });
	await expect(page.getByLabel('Nama Situs')).toHaveValue('Situs E2E');
	await expect(page.getByAltText('Pratinjau logo situs')).toBeVisible();
});

test('non-main-account session sees the real data in a disabled read-only form', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');

	const titleInput = page.getByLabel('Judul', { exact: true });
	await expect(titleInput).toBeVisible({ timeout: 10_000 });
	await expect(titleInput).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled();
});

test('selecting an oversized logo shows an inline error and never issues a save request', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByLabel('Judul', { exact: true })).toBeVisible({ timeout: 10_000 });

	// Branding is a persistent per-tenant singleton, and the "main account can edit..." test above
	// really does save a real logo to it — so this run may load with that preview already showing.
	// Clear it client-side via the page's own "Hapus" affordance (no save issued) so this test's
	// baseline is genuinely "no preview" before asserting the invalid file never produces one.
	const logoRow = page.getByLabel('Unggah logo').locator('..');
	const removeLogoButton = logoRow.getByRole('button', { name: 'Hapus' });
	if (await removeLogoButton.isVisible()) {
		await removeLogoButton.click();
	}
	await expect(page.getByAltText('Pratinjau logo situs')).toBeHidden();

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
	await expect(page.getByLabel('Judul', { exact: true })).toBeVisible({ timeout: 10_000 });

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
