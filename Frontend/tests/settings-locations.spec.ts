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
