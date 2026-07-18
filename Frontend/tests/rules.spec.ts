// Frontend/tests/rules.spec.ts
//
// REAL end-to-end proof of Fase 7d's /rules Rule Builder. Same real-stack setup as
// tests/login.spec.ts, tests/command.spec.ts, tests/tickets.spec.ts — real reactor-core on
// :8081 behind Vite's dev proxy, real Postgres (tower-postgres, 127.0.0.1:15432), real Redis
// (tower-redis, 127.0.0.1:16379). Nothing here is mocked or stubbed.
//
// Prerequisites (see this task's Steps 1-2 for the exact commands, already run once against the
// dev DB before this file was written):
// - e2e-test-user (main-account, from Fase 7a) — used for all rule-editing + OTP-arm tests.
// - e2e-readonly-user / correct-horse-battery-staple (is_main_account=false) — used for the
//   read-only-view test.
// - tower-dev's site_settings.waha_settings row has a non-empty wa_number, so
//   /auth/request-aa-otp succeeds (200) instead of 400ing.
//
// No accept_rules rows are pre-seeded — /rules' whole purpose is creating rules through the UI,
// so the "load and display" coverage below creates a rule via the real Save flow and reloads the
// page to prove it persisted, rather than hand-crafting an INSERT (accept_rules.route_signature
// is a GENERATED ALWAYS column and must never be inserted explicitly — this suite avoids the
// question entirely by never inserting into accept_rules at all).
//
// The OTP code is generated fresh (random) on every /auth/request-aa-otp call, so it cannot be
// pre-seeded — it's read LIVE mid-test directly from Redis via `redis-cli`, the same
// "read/seed backend state directly" precedent tickets.spec.ts and login.spec.ts already
// established via psql, just via Redis instead of Postgres. `e2e-test-user`'s real
// portal_users.id (looked up directly, see this task's Step 3 preamble) is baked into the Redis
// key this constant builds.

import { test, expect } from '@playwright/test';
import { execFileSync } from 'node:child_process';

const TENANT_ID = 'e03ac22f-729b-436f-a112-08aab5022614';
const E2E_TEST_USER_ID = '0b93247e-2e8d-494a-bcc2-0908389605f0';

function readOtpCodeFromRedis(): string {
	const key = `spx:aa_otp:${TENANT_ID}:${E2E_TEST_USER_ID}`;
	return execFileSync('redis-cli', ['-h', '127.0.0.1', '-p', '16379', 'GET', key]).toString().trim();
}

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /rules redirects to /login', async ({ page }) => {
	await page.goto('/rules');
	await expect(page).toHaveURL(/\/login/);
});

test('main account can create a route-mode rule with a new inline location, save, and it persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/rules');
	await expect(page.getByRole('heading', { name: 'Rule Builder' })).toBeVisible();

	await page.getByRole('button', { name: '+ Rule Rute' }).click();
	// The newly added rule row starts collapsed with no name — expand it (its own clickable
	// header region, not a nested button — see RuleRow.svelte's structural-sibling layout note).
	await page.getByText('Rule tanpa nama').click();

	// getByLabel('Nama') is a substring/case-insensitive match, so it also matches the row's
	// delete button (aria-label "Hapus rule tanpa nama" while unnamed) — .last() picks the real
	// Nama textbox, which the fieldset renders after the header in DOM order (same pattern the
	// next test below already relies on for its own getByLabel('Nama').last()).
	await page.getByLabel('Nama').last().fill('E2E Padang Lane');

	// Origin: type a brand-new location name (nothing in route_locations matches it yet) and
	// press Enter — LocationCombobox's commit() falls through to onCreateLocation, exercising the
	// real POST /locations call, not just a pre-existing pick.
	const originInput = page.getByLabel('Asal');
	await originInput.fill('E2E Padang DC');
	await originInput.press('Enter');
	await expect(page.getByText('E2E Padang DC', { exact: true })).toBeVisible();

	const destInput = page.getByLabel('Tujuan (urut, maks 5)');
	await destInput.fill('E2E Cileungsi DC');
	await destInput.press('Enter');
	await expect(page.getByText('E2E Cileungsi DC', { exact: true })).toBeVisible();

	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	// Reload from scratch — proves the save actually persisted server-side, not just local state.
	await page.reload();
	await expect(page.getByText('E2E Padang Lane')).toBeVisible({ timeout: 10_000 });

	// Self-cleaning: delete this rule so reruns of this suite against the same (non-reset) dev DB
	// don't accumulate duplicate "E2E Padang Lane" rows, which would turn the toBeVisible() locator
	// above into a strict-mode violation on the next run (same pattern as the sibling test below).
	await page.getByText('E2E Padang Lane').click();
	await page.getByRole('button', { name: 'Hapus rule E2E Padang Lane' }).click();
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E Padang Lane')).toBeHidden({ timeout: 10_000 });
});

test('editing and deleting an existing rule persists after save and reload', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/rules');

	// Add a throwaway filter-mode rule specifically for this test (independent of the route-mode
	// rule the previous test created — this suite's tests do not depend on execution order, each
	// creates its own fixture data).
	await page.getByRole('button', { name: '+ Rule Filter' }).click();
	await page.getByText('Rule tanpa nama').last().click();
	const nameInputs = page.getByLabel('Nama');
	await nameInputs.last().fill('E2E Throwaway Filter');
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E Throwaway Filter')).toBeVisible({ timeout: 10_000 });

	// Expand it, delete it, save, reload, confirm it's gone.
	await page.getByText('E2E Throwaway Filter').click();
	await page.getByRole('button', { name: 'Hapus rule E2E Throwaway Filter' }).click();
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByText('E2E Throwaway Filter')).toBeHidden({ timeout: 10_000 });
});

test('non-main-account session sees a read-only view with no edit controls', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/rules');
	await expect(page.getByText('Hanya akun utama yang dapat mengubah rule.')).toBeVisible();
	await expect(page.getByRole('button', { name: '+ Rule Rute' })).toBeHidden();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeHidden();
	// The kill switch is still visible (view is ungated) but must be disabled — AutoAcceptSwitch
	// is a structural sibling of RuleRow's <fieldset>, not nested inside it, so it needs (and
	// Task 5/7 give it) its own explicit `readOnly` prop rather than inheriting the fieldset's
	// disabling for free.
	await expect(page.getByRole('switch', { name: 'Aktifkan atau nonaktifkan Auto-Accept' })).toBeDisabled();
});

test('OTP arm flow: request code, read it from Redis, verify, and auto-accept status persists after reload', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/rules');

	const killSwitch = page.getByRole('switch', { name: 'Aktifkan atau nonaktifkan Auto-Accept' });
	// Skip if already ON from a prior run of this suite against the same dev DB (idempotent
	// re-run safety, matching this suite's other tests' "reload and confirm" pattern rather than
	// assuming a pristine starting state).
	if ((await killSwitch.getAttribute('aria-checked')) === 'true') {
		test.skip();
	}

	await killSwitch.click();
	const dialog = page.getByRole('dialog', { name: 'Verifikasi kode OTP' });
	await expect(dialog).toBeVisible();
	await page.getByRole('button', { name: 'Kirim kode' }).click();
	// getByLabel('Kode OTP') is a substring/case-insensitive match, so it also matches the
	// dialog's own aria-label ("Verifikasi kode OTP") — resolution order between the two isn't
	// stable across separate locator re-queries, so scoping to the dialog and grabbing its one
	// textbox (confirmed there is exactly one inside it) sidesteps the ambiguity entirely instead
	// of relying on `.first()`/`.last()` DOM-order guessing.
	const codeInput = dialog.getByRole('textbox');
	await expect(codeInput).toBeVisible({ timeout: 10_000 });

	const code = readOtpCodeFromRedis();
	expect(code).toMatch(/^\d{6}$/);
	await codeInput.fill(code);
	await page.getByRole('button', { name: 'Verifikasi' }).click();
	await expect(page.getByRole('dialog', { name: 'Verifikasi kode OTP' })).toBeHidden({ timeout: 10_000 });
	await expect(killSwitch).toHaveAttribute('aria-checked', 'true');

	// Arming flips local state to ON, but PUT /bookings/settings is what actually persists it
	// (per the 120s pwverify-proof contract) — Save must happen while that window is open.
	await page.getByRole('button', { name: 'Simpan Perubahan' }).click();
	await expect(page.getByRole('button', { name: 'Simpan Perubahan' })).toBeDisabled({ timeout: 10_000 });

	await page.reload();
	await expect(page.getByRole('switch', { name: 'Aktifkan atau nonaktifkan Auto-Accept' })).toHaveAttribute(
		'aria-checked',
		'true',
		{ timeout: 10_000 }
	);
});
