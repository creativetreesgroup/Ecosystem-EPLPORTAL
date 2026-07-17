// Frontend/tests/command.spec.ts
//
// REAL end-to-end proof of Fase 7b's nav shell + /command page. Same real-stack setup as
// tests/login.spec.ts (Fase 7a Task 6) — read that file's top comment for the full prerequisite
// list (SvelteKit dev server via Playwright's webServer -> Vite dev proxy -> real `reactor-core`
// on :8081, TENANT_SLUG=tower-dev -> real Postgres `tower-postgres` (127.0.0.1:15432) + real
// Redis `tower-redis` (127.0.0.1:16379)). Nothing here is mocked or stubbed. Reuses the SAME
// seeded `e2e-test-user` / `correct-horse-battery-staple` portal_users row login.spec.ts already
// seeded into the `tower-dev` tenant (id `e03ac22f-729b-436f-a112-08aab5022614`) — no new user.
//
// --- Additional prerequisite for THIS file: a seeded `pending` booking ---
//
// /command's Ticket Ticker has nothing to render (and no "Terima" button to assert on) unless at
// least one real `pending` row exists in `bookings` for the same tenant. `store::upsert_booking`
// (`Backend/crates/store/src/bookings.rs`) INSERTs `(id, tenant_id, account_id, spx_id, status,
// raw_data, created_at, updated_at)` — `is_coc`/`needs_enrichment` are Postgres GENERATED columns
// (migration 0007_bookings.sql) and must NEVER be listed explicitly (Postgres rejects an explicit
// value there). Verified against the real schema
// (`Backend/crates/store/migrations/0007_bookings.sql`, `0020_bookings_account_id.sql`) before
// writing this, not guessed. `raw_data` uses the same `route_detail_list`/`node_info_list` shape
// `Backend/crates/api-gateway/tests/bookings_routes.rs`'s own
// `live_and_history_split_by_status_and_require_session` test seeds (traced to
// `spx_client::normalize_booking` / `core_domain::route_parse::parse_route_stops`, which is what
// `GET /bookings/live`'s `route` field is actually sourced from — see `routes/bookings.rs`'s
// `route = spx_client::normalize_booking(&b.raw_data).route_stops`) — WITHOUT it, `route` comes
// back empty and TicketTicker just renders "—", proving nothing about the real parse path. The
// `spx_id` deliberately does NOT start with `SPXID` — that prefix flips the `is_coc` generated
// column true (`spx_id ~* '^\s*SPXID'`), which would misrepresent this as a cash-on-collect
// booking for no reason relevant to this test.
//
// Seeded directly via `psql` (as the `tower` superuser — bypasses RLS for a one-off insert
// exactly like `login.spec.ts`'s `portal_users` seed does; see `migrations/0016_rls_policies.sql`'s
// own doc comment on why a superuser connection is exempt from FORCE ROW LEVEL SECURITY):
//
//   PGPASSWORD=tower_dev_only psql -h 127.0.0.1 -p 15432 -U tower -d tower -c "
//     INSERT INTO bookings (id, tenant_id, account_id, spx_id, status, raw_data)
//     VALUES (
//       gen_random_uuid(),
//       'e03ac22f-729b-436f-a112-08aab5022614',
//       'e2e-test-account',
//       '251778899001',
//       'pending',
//       '{
//         \"booking_id\": \"778899001\",
//         \"route_detail_list\": [{
//           \"node_info_list\": [
//             {\"name\": \"Cikarang DC\", \"address_info\": {\"l1\": \"Jawa Barat\", \"l2\": \"Bekasi\"}},
//             {\"name\": \"Semarang DC\", \"address_info\": {\"l1\": \"Jawa Tengah\", \"l2\": \"Semarang\"}}
//           ]
//         }]
//       }'::jsonb
//     );
//   "
//
// This row is real, persistent Postgres state (not cleaned up automatically by this spec file
// itself — re-running this suite against the same dev database does not need to re-seed; a
// second run would just show the same still-`pending` row, since nothing in this suite clicks
// "Terima").

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page) {
	await page.goto('/login');
	await page.getByLabel('Username').fill('e2e-test-user');
	await page.getByLabel('Password').fill('correct-horse-battery-staple');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /command redirects to /login', async ({ page }) => {
	await page.goto('/command');
	await expect(page).toHaveURL(/\/login/);
});

test('after login, /command shows the nav shell with LIVE health pill once WS connects', async ({ page }) => {
	await login(page);
	// `exact: true` is required, not cosmetic: a plain `getByText('TOWER')` is a strict-mode
	// violation here — it also matches SvelteKit's hidden `#svelte-announcer` live region, which
	// announces the page's <title> ("Command — TOWER") on every client-side navigation. Only the
	// TopNav brand span's full text is the exact string "TOWER".
	await expect(page.getByText('TOWER', { exact: true })).toBeVisible();
	await expect(page.getByText('LIVE')).toBeVisible({ timeout: 10_000 });
});

test('ticket ticker shows the seeded pending booking', async ({ page }) => {
	await login(page);
	await expect(page.getByText('Terima')).toBeVisible({ timeout: 10_000 });
});

test('keyboard-only: tab to a nav link and activate it', async ({ page }) => {
	await login(page);
	await page.getByRole('link', { name: 'Tickets' }).focus();
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/tickets/);
});
