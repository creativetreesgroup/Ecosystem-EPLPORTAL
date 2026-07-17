// Frontend/tests/login.spec.ts
//
// REAL end-to-end proof of Fase 7a's login path: SvelteKit dev server (`pnpm dev`, started by
// `playwright.config.ts`'s `webServer`) -> Vite dev proxy (Task 3, `vite.config.ts`'s
// `server.proxy`) -> real `reactor-core` (bin/reactor-core, run standalone, NOT via Docker — the
// Docker build image lacks a Rust toolchain, a separate tracked gap; the host has the full
// toolchain, same as Task 3's own verification) -> real Postgres (`tower-postgres`, 127.0.0.1:15432)
// + real Redis (`tower-redis`, 127.0.0.1:16379). Nothing here is mocked or stubbed.
//
// --- Prerequisites before running `pnpm exec playwright test` ---
//
// 1. `tower-postgres`/`tower-redis` dev containers already running (`docker ps`).
// 2. `reactor-core` running locally on port 8081, pointed at the SAME tenant seeded below:
//
//      cd Backend && export PATH="$HOME/.cargo/bin:$PATH"
//      DATABASE_URL="postgres://app_role:app_role_dev_only@127.0.0.1:15432/tower" \
//      REDIS_URL="redis://127.0.0.1:16379" \
//      TENANT_SLUG="tower-dev" \
//      COOKIE_SECURE=false \
//      CORS_ALLOWED_ORIGINS="http://127.0.0.1:5173" \
//      cargo run --bin reactor-core
//
//    (`COOKIE_SECURE=false` is required for the browser to accept/send the session cookie at all
//    over plain HTTP local dev — see `state.rs`'s `cookie_secure` field doc comment.
//    `TENANT_SLUG=tower-dev` reuses the dev tenant Task 2/3 already seeded into `tower-postgres`
//    — id `e03ac22f-729b-436f-a112-08aab5022614`, slug `tower-dev` — rather than creating a new
//    one; it had zero `portal_users`/`agency_credentials` rows before this task, so seeding a
//    fresh `portal_users` row into it (below) didn't collide with anything.)
//
// 3. A `portal_users` row for the credentials this suite logs in with
//    (`e2e-test-user` / `correct-horse-battery-staple`). This plan's own brief flagged that no
//    ready-made seed script exists in this codebase (`Backend/bin/` has only `reactor-core` and
//    `auth-sidecar`, no seed binary) and asked for the lowest-friction real solution, not an
//    invented convention. What was actually done, mirroring the INSERT shape already established
//    by `Backend/crates/api-gateway/tests/auth_routes.rs`'s own `insert_portal_user` helper
//    (`spx_client::crypto::password::hash_password` -> raw `INSERT INTO portal_users`, run as the
//    `tower` superuser so RLS never needs a tenant-scoped transaction for a one-off insert):
//
//      a) Computed a real argon2id PHC hash of `correct-horse-battery-staple` via a throwaway,
//         NEVER-committed `examples/hash_pw.rs` added temporarily under `Backend/crates/spx-client`
//         (deleted immediately after use — `git status` shows no trace of it):
//
//           fn main() {
//               let pw = std::env::args().nth(1).expect("usage: hash_pw <password>");
//               println!("{}", spx_client::crypto::password::hash_password(&pw).unwrap());
//           }
//
//         Run via: `cargo run -p spx-client --example hash_pw -- "correct-horse-battery-staple"`
//         Produced: `$argon2id$v=19$m=19456,t=2,p=1$wpqXhXebq5sOx4tdhOFnJQ$rGCnrcQzZfOaFihhvFRi/nskuDjEYSdvlOHZOdaiw7Y`
//
//      b) Inserted the row directly via `psql` (as the `tower` superuser, same role/password
//         `store`'s own `test_database_url()` default and `Docker/docker-compose.yml`'s
//         `POSTGRES_USER` use — bypasses RLS the same way `store/src/lib.rs`'s own doc comment
//         describes superusers doing, appropriate for a one-off seed insert outside any test's
//         own transaction):
//
//           PGPASSWORD=tower_dev_only psql -h 127.0.0.1 -p 15432 -U tower -d tower -c "
//             INSERT INTO portal_users (tenant_id, username, password_hash, display_name, is_main_account)
//             VALUES ('e03ac22f-729b-436f-a112-08aab5022614', 'e2e-test-user',
//                      '\$argon2id\$v=19\$m=19456,t=2,p=1\$wpqXhXebq5sOx4tdhOFnJQ\$rGCnrcQzZfOaFihhvFRi/nskuDjEYSdvlOHZOdaiw7Y',
//                      'Fase 7a E2E Test User', true);
//           "
//
//    This row is real, persistent Postgres state (not cleaned up automatically by this spec file
//    itself — re-running this suite against the same dev database does not need to re-seed).
//
// 4. `pnpm exec playwright test` — `playwright.config.ts`'s own `webServer` starts `pnpm dev` for
//    you; nothing else to start manually on the frontend side.

import { test, expect } from '@playwright/test';

test('login with valid credentials sets a session cookie and redirects', async ({ page }) => {
	await page.goto('/login');
	await page.getByLabel('Username').fill('e2e-test-user');
	await page.getByLabel('Password').fill('correct-horse-battery-staple');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
	const cookies = await page.context().cookies();
	expect(cookies.some((c) => c.name === 'spx_session' && c.httpOnly)).toBe(true);
});

test('login with wrong password shows the generic error message', async ({ page }) => {
	await page.goto('/login');
	await page.getByLabel('Username').fill('e2e-test-user');
	await page.getByLabel('Password').fill('definitely-wrong');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page.getByRole('alert')).toHaveText('Username atau password salah');
});

test('login with an unknown username shows the SAME generic error message', async ({ page }) => {
	await page.goto('/login');
	await page.getByLabel('Username').fill('no-such-user-at-all');
	await page.getByLabel('Password').fill('anything');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page.getByRole('alert')).toHaveText('Username atau password salah');
});

test('keyboard-only walkthrough: tab to username, type, tab to password, type, Enter submits', async ({
	page
}) => {
	await page.goto('/login');
	// Wait for hydration to finish before driving pure keyboard events: unlike `.fill()`/`.click()`
	// (used by the other tests), raw `keyboard.press`/`keyboard.type` calls run back-to-back with no
	// actionability retries in between, so on a cold `vite dev` server they can outrun SvelteKit's
	// client-side hydration — the DOM inputs happily accept the typed text either way, but the
	// `bind:value` listeners that sync it into Svelte's `$state` (which drives `canSubmit`, i.e. the
	// submit button's `disabled` attribute) aren't attached yet, so Enter's implicit-submit finds no
	// enabled default button. `networkidle` reliably lands after hydration's module fetches settle.
	await page.waitForLoadState('networkidle');
	await page.keyboard.press('Tab');
	await page.keyboard.type('e2e-test-user');
	await page.keyboard.press('Tab');
	await page.keyboard.type('correct-horse-battery-staple');
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/command/);
});
