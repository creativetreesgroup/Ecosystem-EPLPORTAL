// Frontend/tests/activity.spec.ts
//
// REAL end-to-end proof of Fase 7f's /activity page. Same real-stack setup as
// tests/login.spec.ts, tests/command.spec.ts, tests/tickets.spec.ts, tests/rules.spec.ts,
// tests/price.spec.ts — real reactor-core on :8081 behind Vite's dev proxy, real Postgres
// (tower-postgres, 127.0.0.1:15432), real Redis (tower-redis, 127.0.0.1:16379). Nothing here is
// mocked or stubbed.
//
// Reuses e2e-test-user (main-account) and e2e-readonly-user (non-main-account) — both already
// seeded by Fase 7a/7d, no re-seeding needed. accept_events has a ONE-TIME 21-row bulk seed (see
// this task's Step 1) — accept_events is append-only with no UI creation path, unlike bot_log,
// which every test needing an entry seeds fresh for itself via redis-cli (see Step 2's own
// rationale: DELETE /bot/logs clears the whole key, so a shared one-time bot_log seed would
// create a real ordering conflict between the "shows entries" and "clear" tests).

import { test, expect } from '@playwright/test';
import { execFileSync } from 'node:child_process';

const TENANT_ID = 'e03ac22f-729b-436f-a112-08aab5022614';

function seedBotLogEntry(overrides: Partial<Record<string, unknown>> = {}) {
	const entry = {
		ts: Date.now(),
		log_type: 'success',
		kind: 'otp',
		booking_id: null,
		latency_ms: 5000,
		rule: null,
		error: null,
		...overrides
	};
	execFileSync('redis-cli', [
		'-h',
		'127.0.0.1',
		'-p',
		'16379',
		'LPUSH',
		`spx:bot:logs:${TENANT_ID}`,
		JSON.stringify(entry)
	]);
}

async function login(page: import('@playwright/test').Page, username: string, password: string) {
	await page.goto('/login');
	await page.getByLabel('Username').fill(username);
	await page.getByLabel('Password').fill(password);
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /activity redirects to /login', async ({ page }) => {
	await page.goto('/activity');
	await expect(page).toHaveURL(/\/login/);
});

test('Riwayat Keputusan tab loads by default and shows seeded accept_events', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByRole('heading', { name: 'Activity' })).toBeVisible();
	await expect(page.getByRole('tab', { name: 'Riwayat Keputusan' })).toHaveAttribute('aria-selected', 'true');
	await expect(page.getByText('Diterima').first()).toBeVisible({ timeout: 10_000 });
});

test('pagination on Riwayat Keputusan shows genuinely different server-fetched content on page 2', async ({
	page
}) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByText('Diterima').first()).toBeVisible({ timeout: 10_000 });

	// The oldest of the 21 seeded rows (121 µs) must NOT be on page 1 (PAGE_SIZE=20).
	await expect(page.getByText('121 µs')).toBeHidden();

	await page.getByRole('button', { name: 'Halaman berikutnya' }).click();
	await expect(page.getByText('121 µs')).toBeVisible({ timeout: 10_000 });
});

test('expanding an accept_events row reveals its raw JSON detail', async ({ page }) => {
	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByText('Diterima').first()).toBeVisible({ timeout: 10_000 });

	await page.getByText('Diterima').first().click();
	await expect(page.getByText('"seed_marker"')).toBeVisible();
});

test('non-main-account session does not see the Log Bot tab at all', async ({ page }) => {
	await login(page, 'e2e-readonly-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await expect(page.getByRole('tab', { name: 'Log Bot' })).toBeHidden();
});

test('main account sees Log Bot entries and can clear them (confirm dialog accepted)', async ({ page }) => {
	seedBotLogEntry({ kind: 'otp', log_type: 'success' });

	await login(page, 'e2e-test-user', 'correct-horse-battery-staple');
	await page.goto('/activity');
	await page.getByRole('tab', { name: 'Log Bot' }).click();
	// .first(): the shared dev tenant's bot_log already carries real entries from reactor-core's
	// own live activity (including other `kind: 'otp'` rows) by the time e2e suites run against
	// it, so a bare getByText('OTP') hits a strict-mode multi-match violation — .first() only
	// needs SOME row to be visible to prove the tab renders seeded/real entries.
	await expect(page.getByText('OTP').first()).toBeVisible({ timeout: 10_000 });

	// Wait for the actual DELETE /bot/logs response before asserting on the resulting UI state:
	// Playwright's strict-mode locator check throws immediately (not a retried condition) if it
	// still resolves >1 elements, so asserting toBeHidden() right after .click() (which only
	// waits for the click event, not the async handler's awaited network call) races the real
	// clear-then-rerender and fails on the still-populated list rather than timing out.
	const clearResponse = page.waitForResponse(
		(res) => res.request().method() === 'DELETE' && res.url().includes('/bot/logs')
	);
	page.once('dialog', (dialog) => dialog.accept());
	await page.getByRole('button', { name: 'Hapus Log' }).click();
	await clearResponse;
	await expect(page.getByText('OTP')).toBeHidden({ timeout: 10_000 });
	await expect(page.getByRole('button', { name: 'Hapus Log' })).toBeDisabled();
});
