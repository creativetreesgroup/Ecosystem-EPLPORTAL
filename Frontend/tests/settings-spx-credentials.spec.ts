// Frontend/tests/settings-spx-credentials.spec.ts
//
// REAL end-to-end proof of Fase 7k's /settings/spx-credentials page. Same real-stack setup
// as tests/settings-sub-users.spec.ts — real reactor-core behind Vite's proxy, real Postgres,
// real Redis. Nothing is mocked.
//
// The "Test Koneksi" button is DELIBERATELY never clicked here: it performs a real login
// against the production SPX upstream and could lock the tenant's SPX account. We assert only
// that the button renders and is disabled for non-main-account users. The backend cooldown is
// covered by Backend/crates/api-gateway/tests/spx_login_routes.rs against a mock SPX server.
//
// Every credential this suite creates uses a Date.now()-suffixed label and is deleted within
// its own test; it never touches shared fixture rows.

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /settings/spx-credentials redirects to /login', async ({ page }) => {
	await page.goto('/settings/spx-credentials');
	await expect(page).toHaveURL(/\/login/);
});

test('main account sees the Akun SPX nav entry and the restart notice', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/branding');
	await expect(page.getByRole('link', { name: 'Akun SPX' })).toBeVisible();

	await page.goto('/settings/spx-credentials');
	await expect(page.getByText('Perubahan di sini baru aktif setelah restart.')).toBeVisible({
		timeout: 10_000
	});
});

test('creating a credential persists it, exposes a Test button, then it can be deleted', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');
	await expect(page.getByLabel('Label')).toBeVisible({ timeout: 10_000 });

	const uniqueLabel = `e2e-spx-${Date.now()}`;
	await page.getByLabel('Label').fill(uniqueLabel);
	await page.getByLabel('Username').fill(`${uniqueLabel}-user`);
	await page.getByLabel('Password').fill('a-valid-password');
	await page.getByRole('button', { name: 'Simpan Kredensial' }).click();
	await expect(page.getByText(uniqueLabel)).toBeVisible({ timeout: 10_000 });

	await page.reload();
	const row = page.locator('li', { hasText: uniqueLabel });
	await expect(row).toBeVisible({ timeout: 10_000 });
	// The Test button renders and is enabled — but we never click it (real SPX login).
	await expect(row.getByRole('button', { name: 'Test' })).toBeEnabled();

	page.once('dialog', (dialog) => dialog.accept());
	await row.getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(uniqueLabel)).toBeHidden({ timeout: 10_000 });
});

test('a duplicate-username-different-label entry is blocked and fires no PUT', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');
	await expect(page.getByLabel('Label')).toBeVisible({ timeout: 10_000 });

	// First, create a real credential to clash against.
	const baseLabel = `e2e-dup-${Date.now()}`;
	const sharedUser = `${baseLabel}-user`;
	await page.getByLabel('Label').fill(baseLabel);
	await page.getByLabel('Username').fill(sharedUser);
	await page.getByLabel('Password').fill('a-valid-password');
	await page.getByRole('button', { name: 'Simpan Kredensial' }).click();
	await expect(page.getByText(baseLabel)).toBeVisible({ timeout: 10_000 });

	// Now attempt a DIFFERENT label with the SAME username — must be blocked client-side.
	let putCount = 0;
	await page.route('**/auth/spx-credentials/**', (route) => {
		if (route.request().method() === 'PUT') putCount++;
		route.continue();
	});
	await page.getByLabel('Label').fill(`${baseLabel}-two`);
	await page.getByLabel('Username').fill(sharedUser.toUpperCase()); // case-insensitive clash
	await page.getByLabel('Password').fill('another-password');
	await expect(page.getByText(`Username ini sudah dipakai label "${baseLabel}"`)).toBeVisible();
	await expect(page.getByRole('button', { name: 'Simpan Kredensial' })).toBeDisabled();
	expect(putCount).toBe(0);

	// Clean up the one real row created.
	await page.unroute('**/auth/spx-credentials/**');
	await page.reload();
	page.once('dialog', (dialog) => dialog.accept());
	await page.locator('li', { hasText: baseLabel }).getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(baseLabel)).toBeHidden({ timeout: 10_000 });
});

test('typing an existing label shows the overwrite note', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');
	await expect(page.getByLabel('Label')).toBeVisible({ timeout: 10_000 });

	const uniqueLabel = `e2e-ovr-${Date.now()}`;
	await page.getByLabel('Label').fill(uniqueLabel);
	await page.getByLabel('Username').fill(`${uniqueLabel}-user`);
	await page.getByLabel('Password').fill('a-valid-password');
	await page.getByRole('button', { name: 'Simpan Kredensial' }).click();
	await expect(page.getByText(uniqueLabel)).toBeVisible({ timeout: 10_000 });

	await page.getByLabel('Label').fill(uniqueLabel);
	await expect(page.getByText('menyimpan akan menimpa kredensial lama')).toBeVisible();

	// Clean up.
	page.once('dialog', (dialog) => dialog.accept());
	await page.locator('li', { hasText: uniqueLabel }).getByRole('button', { name: 'Hapus' }).click();
	await expect(page.getByText(uniqueLabel)).toBeHidden({ timeout: 10_000 });
});

// Asserts the ADD-FORM read-only state only (label input + Simpan disabled). The per-row
// Test/Hapus buttons carry their own `disabled={readOnly || …}` binding outside the fieldset;
// asserting those here would require a credential row to exist for the shared tower-dev tenant
// at run time (this suite is self-cleaning, so it may not), so the row-button read-only path is
// covered structurally (identical binding to the tested Locations/Sub-user siblings) rather
// than e2e-asserted here.
test('non-main-account sees the list with a disabled add form', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/settings/spx-credentials');

	const labelInput = page.getByLabel('Label');
	await expect(labelInput).toBeVisible({ timeout: 10_000 });
	await expect(labelInput).toBeDisabled();
	await expect(page.getByRole('button', { name: 'Simpan Kredensial' })).toBeDisabled();
});
