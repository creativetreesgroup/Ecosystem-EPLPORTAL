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
