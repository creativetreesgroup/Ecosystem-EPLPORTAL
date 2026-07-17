// Frontend/tests/tickets.spec.ts
//
// REAL end-to-end proof of Fase 7c's /tickets full ticket-management view. Same real-stack setup
// as tests/login.spec.ts and tests/command.spec.ts — read those files' top comments for the full
// prerequisite list (SvelteKit dev server via Playwright's webServer -> Vite dev proxy -> real
// `reactor-core` on :8081, TENANT_SLUG=tower-dev -> real Postgres `tower-postgres`
// (127.0.0.1:15432) + real Redis `tower-redis` (127.0.0.1:16379)). Nothing here is mocked or
// stubbed. Reuses the SAME seeded `e2e-test-user` / `correct-horse-battery-staple` portal_users
// row login.spec.ts already seeded into the `tower-dev` tenant
// (id `e03ac22f-729b-436f-a112-08aab5022614`) — no new user.
//
// --- Additional prerequisite for THIS file: an `accepted` and a `failed` booking ---
//
// /tickets defaults to `filters.status === null` ("semua status"), which `fetchTickets`
// (Frontend/src/lib/api-tickets.ts) resolves by fetching BOTH /bookings/live (defaults to
// `status='pending'` server-side, `store::bookings::list_live`) and /bookings/history (defaults
// to `status IN ('accepted','failed')`, `store::bookings::list_history`) and merging them
// (`mergeAndSlicePage`). command.spec.ts already seeded one `pending` row (still present — see
// `docker ps` / direct psql check, not assumed). Without at least one `accepted` and one `failed`
// row too, the status-filter test below (selecting "pending" and asserting no "Diterima" text
// remains) proves nothing — there'd be no "Diterima" row to have narrowed away in the first
// place.
//
// Schema verified directly against `Backend/crates/store/migrations/0007_bookings.sql` and
// `0020_bookings_account_id.sql` before writing this (not guessed): `bookings` INSERTs
// `(id, tenant_id, account_id, spx_id, status, raw_data)` — `is_coc`/`needs_enrichment` are
// Postgres GENERATED ALWAYS AS ... STORED columns and must NEVER be listed explicitly (Postgres
// rejects an explicit value there). `status` has NO DB CHECK constraint (VARCHAR(32) only) — the
// application-level vocabulary (`pending`/`accepted`/`failed`) is enforced by
// `routes/bookings.rs::parse_status_filter`, not the schema; the values below match that
// vocabulary exactly, same discipline as command.spec.ts's own seed comment. `raw_data` uses the
// same `route_detail_list`/`node_info_list` shape command.spec.ts's seed and
// `bookings_routes.rs`'s own test fixtures use, so `route` (sourced from
// `spx_client::normalize_booking`) renders real stop names instead of "—". Neither new `spx_id`
// starts with `SPXID` — that prefix would flip the generated `is_coc` column true, misrepresenting
// these as cash-on-collect bookings for no reason relevant to this test. `account_id` reuses
// command.spec.ts's `e2e-test-account` string (a plain TEXT column, no FK) — no significance
// beyond consistency with the existing seeded row.
//
// Seeded directly via `psql` (as the `tower` superuser — bypasses RLS for a one-off insert exactly
// like command.spec.ts's and login.spec.ts's own seeds; see migrations/0016_rls_policies.sql's doc
// comment on why a superuser connection is exempt from FORCE ROW LEVEL SECURITY):
//
//   PGPASSWORD=tower_dev_only psql -h 127.0.0.1 -p 15432 -U tower -d tower -c "
//     INSERT INTO bookings (id, tenant_id, account_id, spx_id, status, raw_data)
//     VALUES (
//       gen_random_uuid(),
//       'e03ac22f-729b-436f-a112-08aab5022614',
//       'e2e-test-account',
//       '251778899002',
//       'accepted',
//       '{
//         \"booking_id\": \"778899002\",
//         \"route_detail_list\": [{
//           \"node_info_list\": [
//             {\"name\": \"Jakarta DC\", \"address_info\": {\"l1\": \"DKI Jakarta\", \"l2\": \"Jakarta Selatan\"}},
//             {\"name\": \"Bandung DC\", \"address_info\": {\"l1\": \"Jawa Barat\", \"l2\": \"Bandung\"}}
//           ]
//         }]
//       }'::jsonb
//     );
//     INSERT INTO bookings (id, tenant_id, account_id, spx_id, status, raw_data)
//     VALUES (
//       gen_random_uuid(),
//       'e03ac22f-729b-436f-a112-08aab5022614',
//       'e2e-test-account',
//       '251778899003',
//       'failed',
//       '{
//         \"booking_id\": \"778899003\",
//         \"route_detail_list\": [{
//           \"node_info_list\": [
//             {\"name\": \"Surabaya DC\", \"address_info\": {\"l1\": \"Jawa Timur\", \"l2\": \"Surabaya\"}},
//             {\"name\": \"Malang DC\", \"address_info\": {\"l1\": \"Jawa Timur\", \"l2\": \"Malang\"}}
//           ]
//         }],
//         \"drift_reason\": \"manual_accept_failed\"
//       }'::jsonb
//     );
//   "
//
// These rows are real, persistent Postgres state (not cleaned up automatically by this spec file
// itself — re-running this suite against the same dev database does not need to re-seed).
//
// --- Selectors verified against REAL current markup, not the plan brief's illustrative code ---
//
// Several components changed shape during Tasks 6-7's review rounds:
// - TicketsTable.svelte: desktop rows are `<tr tabindex="0">` inside a real `<table>` (the header
//   row is `getByRole('row').nth(0)`, so the first DATA row is `.nth(1)`, matching the brief).
// - TicketDetailDrawer.svelte: the panel is `<div role="dialog" aria-label="Detail tiket">`, NOT
//   `<aside>` (the brief's sample) — svelte-check's a11y_no_noninteractive_element_to_interactive_role
//   rule flagged `<aside role="dialog">` as a landmark/dialog conflict, so it was changed to a plain
//   `<div>` during Task 7's implementation. `getByRole('dialog', { name: 'Detail tiket' })` still
//   matches via the `aria-label`.
// - TicketFilterBar.svelte: the status `<select>` has `<label for="ticket-filter-status">Status</label>`
//   — `getByLabel('Status')` resolves uniquely (no other `<label>` on the page reads "Status").
// - The "Riwayat Percobaan" (audit trail) heading always renders once `detail` loads, regardless
//   of whether any accept_events rows exist for that booking (the seeded rows above have none —
//   they were inserted directly, not through the real accept flow — so the drawer shows "Belum
//   ada percobaan tercatat." under that heading, which is fine; the test only asserts the heading).

import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page) {
	await page.goto('/login');
	await page.getByLabel('Username').fill('e2e-test-user');
	await page.getByLabel('Password').fill('correct-horse-battery-staple');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /tickets redirects to /login', async ({ page }) => {
	await page.goto('/tickets');
	await expect(page).toHaveURL(/\/login/);
});

test('after login, /tickets shows the seeded bookings in a table', async ({ page }) => {
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeVisible({ timeout: 10_000 });
});

test('filtering by status narrows the visible rows', async ({ page }) => {
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeVisible({ timeout: 10_000 });
	await page.getByLabel('Status').selectOption('pending');
	// After filtering to pending-only, no TABLE ROW should show a "Diterima" (accepted) status
	// label. Scoped to the table specifically — the status filter's own <select> always renders
	// an `<option value="accepted">Diterima</option>` regardless of which option is selected, so
	// an unscoped `page.getByText('Diterima')` would always find that <option> and could never
	// reach count 0 (verified live: this was a false-positive failure against the brief's
	// literal unscoped version, not a real app bug — the page snapshot on that failure showed
	// exactly one table row, the pending one, and the filter's own dropdown as the "Diterima"
	// match).
	await expect(page.getByRole('table').getByText('Diterima')).toHaveCount(0);
});

test('clicking a row opens the detail drawer with audit trail section', async ({ page }) => {
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeVisible({ timeout: 10_000 });
	await page.getByRole('row').nth(1).click();
	await expect(page.getByRole('dialog', { name: 'Detail tiket' })).toBeVisible();
	await expect(page.getByText('Riwayat Percobaan')).toBeVisible();
});

test('narrow viewport collapses the table into cards', async ({ page }) => {
	await page.setViewportSize({ width: 375, height: 800 });
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeHidden({ timeout: 10_000 });
});
