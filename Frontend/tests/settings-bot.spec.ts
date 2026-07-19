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
