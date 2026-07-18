// Frontend/tests/price.spec.ts
//
// REAL end-to-end proof of Fase 7e's /price route price list. Same real-stack setup as
// tests/login.spec.ts, tests/command.spec.ts, tests/tickets.spec.ts, tests/rules.spec.ts — real
// reactor-core on :8081 behind Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432),
// real Redis (tower-redis, 127.0.0.1:16379). Nothing here is mocked or stubbed.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d, no re-seeding needed. No route_prices rows are pre-seeded either — every
// test creates its own fixture via the real Save flow, same precedent as rules.spec.ts.
//
// IMPORTANT: PriceRow.svelte's delete flow calls window.confirm() before deleting. Playwright
// auto-DISMISSES confirm() by default (returns false) unless a page.on('dialog', ...) handler is
// registered to accept it first — every test below that deletes a row registers one BEFORE the
// click that triggers the dialog.

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /price redirects to /login', async ({ page }) => {
	await page.goto('/price');
	await expect(page).toHaveURL(/\/login/);
});

test('main account can create a price with a new inline location, and it persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');
	await expect(page.getByRole('heading', { name: 'Daftar Harga' })).toBeVisible();

	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').fill('E2E-CREATE-01');
	await page.getByLabel('Region').fill('Jawa');

	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Jakarta DC');
	await originInput.press('Enter');
	await expect(page.getByText('E2E Jakarta DC', { exact: true })).toBeVisible();

	const destInput = page.getByLabel('Tujuan (maks 5)');
	await destInput.fill('E2E Bandung DC');
	await destInput.press('Enter');
	await expect(page.getByText('E2E Bandung DC', { exact: true })).toBeVisible();

	await page.getByRole('radio', { name: 'TRONTON' }).click();
	await page.getByLabel('Harga (Rp)').fill('150000');

	await page.getByRole('button', { name: 'Simpan' }).click();
	// After a successful save, the "Batal" button (new-row-only) disappears — a reliable signal
	// the row transitioned from new/unsaved to saved, distinct from just "no error shown."
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E-CREATE-01')).toBeVisible({ timeout: 10_000 });

	// Self-cleaning: delete this row so reruns of this suite against the same (non-reset) dev DB
	// don't collide with route_prices' tenant+route_code unique constraint on the next run (same
	// precedent rules.spec.ts already established for its own rule fixtures).
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus harga rute E2E-CREATE-01' }).click();
	await expect(page.getByText('E2E-CREATE-01')).toBeHidden({ timeout: 10_000 });
});

test('editing an existing price persists after save and reload', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	// Create a throwaway fixture for this test, independent of the previous test's row.
	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').fill('E2E-EDIT-01');
	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Surabaya DC');
	await originInput.press('Enter');
	const destInput = page.getByLabel('Tujuan (maks 5)');
	await destInput.fill('E2E Malang DC');
	await destInput.press('Enter');
	await page.getByRole('radio', { name: 'FUSO' }).click();
	await page.getByLabel('Harga (Rp)').fill('200000');
	await page.getByRole('button', { name: 'Simpan' }).click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await page.getByText('E2E-EDIT-01').click();
	await page.getByLabel('Region').fill('Jawa Timur');
	await page.getByRole('button', { name: 'Simpan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan' })).toBeEnabled({ timeout: 10_000 });

	await page.reload();
	await page.getByText('E2E-EDIT-01').click();
	await expect(page.getByLabel('Region')).toHaveValue('Jawa Timur');

	// Self-cleaning: delete this row so reruns of this suite against the same (non-reset) dev DB
	// don't collide with route_prices' tenant+route_code unique constraint on the next run.
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus harga rute E2E-EDIT-01' }).click();
	await expect(page.getByText('E2E-EDIT-01')).toBeHidden({ timeout: 10_000 });
});

test('deleting a price removes it after reload (confirm dialog accepted)', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').fill('E2E-DELETE-01');
	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Medan DC');
	await originInput.press('Enter');
	const destInput = page.getByLabel('Tujuan (maks 5)');
	await destInput.fill('E2E Pekanbaru DC');
	await destInput.press('Enter');
	await page.getByRole('radio', { name: 'CDD LONG' }).click();
	await page.getByLabel('Harga (Rp)').fill('300000');
	await page.getByRole('button', { name: 'Simpan' }).click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E-DELETE-01')).toBeVisible({ timeout: 10_000 });

	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus harga rute E2E-DELETE-01' }).click();
	await expect(page.getByText('E2E-DELETE-01')).toBeHidden({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E-DELETE-01')).toBeHidden({ timeout: 10_000 });
});

test('duplicate route_code on create shows the specific 409 message', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	// First row with a fixed route_code.
	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').first().fill('E2E-DUP-01');
	const originInput1 = page.getByLabel('Asal').first();
	await originInput1.fill('E2E Semarang DC');
	await originInput1.press('Enter');
	const destInput1 = page.getByLabel('Tujuan (maks 5)').first();
	await destInput1.fill('E2E Solo DC');
	await destInput1.press('Enter');
	await page.getByRole('radio', { name: 'ENGKEL' }).first().click();
	await page.getByLabel('Harga (Rp)').first().fill('100000');
	await page.getByRole('button', { name: 'Simpan' }).first().click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	// Second row, SAME route_code — expect a 409 with the specific message, row stays unsaved.
	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').first().fill('E2E-DUP-01');
	const originInput2 = page.getByLabel('Asal').first();
	await originInput2.fill('E2E Yogyakarta DC');
	await originInput2.press('Enter');
	const destInput2 = page.getByLabel('Tujuan (maks 5)').first();
	await destInput2.fill('E2E Magelang DC');
	await destInput2.press('Enter');
	await page.getByRole('radio', { name: 'WINGBOX' }).first().click();
	await page.getByLabel('Harga (Rp)').first().fill('120000');
	await page.getByRole('button', { name: 'Simpan' }).first().click();
	await expect(page.getByText('Kode rute sudah dipakai.')).toBeVisible({ timeout: 10_000 });
	// Still unsaved — "Batal" is still present on this second, still-new row.
	await expect(page.getByRole('button', { name: 'Batal' })).toBeVisible();

	// Self-cleaning: discard the still-unsaved second (duplicate) row first — its "Hapus" button is
	// disabled while unsaved and shares the SAME accessible name as the first row's (both typed
	// "E2E-DUP-01"), so removing it via "Batal" avoids an ambiguous two-match locator — then delete
	// the first, actually-saved row so reruns of this suite against the same (non-reset) dev DB
	// don't collide with route_prices' tenant+route_code unique constraint on the next run.
	await page.getByRole('button', { name: 'Batal' }).click();
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus harga rute E2E-DUP-01' }).click();
	await expect(page.getByText('E2E-DUP-01')).toBeHidden({ timeout: 10_000 });
});

test('search filters the visible list client-side', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/price');

	await page.getByRole('button', { name: '+ Tambah Harga' }).click();
	await page.getByLabel('Kode Rute').first().fill('E2E-FILTER-UNIQUE-01');
	const originInput = page.getByLabel('Asal').first();
	await originInput.fill('E2E Palembang DC');
	await originInput.press('Enter');
	const destInput = page.getByLabel('Tujuan (maks 5)').first();
	await destInput.fill('E2E Jambi DC');
	await destInput.press('Enter');
	await page.getByRole('radio', { name: 'BLINDVAN' }).first().click();
	await page.getByLabel('Harga (Rp)').first().fill('90000');
	await page.getByRole('button', { name: 'Simpan' }).first().click();
	await expect(page.getByRole('button', { name: 'Batal' })).toBeHidden({ timeout: 10_000 });

	await page.getByLabel('Cari').fill('zzz-no-such-route-zzz');
	await expect(page.getByText('E2E-FILTER-UNIQUE-01')).toBeHidden();

	await page.getByLabel('Cari').fill('E2E-FILTER-UNIQUE-01');
	await expect(page.getByText('E2E-FILTER-UNIQUE-01')).toBeVisible();

	// Self-cleaning: delete this row so reruns of this suite against the same (non-reset) dev DB
	// don't collide with route_prices' tenant+route_code unique constraint on the next run.
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus harga rute E2E-FILTER-UNIQUE-01' }).click();
	await expect(page.getByText('E2E-FILTER-UNIQUE-01')).toBeHidden({ timeout: 10_000 });
});

test('non-main-account session sees a read-only view with no edit controls', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/price');
	await expect(page.getByText('Hanya akun utama yang dapat mengubah harga.')).toBeVisible();
	await expect(page.getByRole('button', { name: '+ Tambah Harga' })).toBeHidden();
});
