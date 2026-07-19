// Frontend/playwright.config.ts
import { defineConfig } from '@playwright/test';

// Deviation from the brief's literal snippet: `url`/`baseURL` use `localhost`, not `127.0.0.1`.
// `pnpm dev` (plain `vite dev`, no `--host` flag) binds to `localhost`, which on this machine
// resolves to the IPv6 loopback (`::1`) only — confirmed live (Task 3's report already flagged
// this same nuance for its own curl checks): `curl http://127.0.0.1:5173` gets connection-refused
// (curl exit/HTTP code `000`) while `curl http://localhost:5173` succeeds. Since Playwright's
// `webServer.url` readiness probe would otherwise never connect and time out, `localhost` is used
// here instead — functionally identical for a same-machine e2e run, and avoids touching
// `vite.config.ts` (out of scope for this task) just to force an IPv4 bind.
export default defineConfig({
	testDir: 'tests',
	webServer: {
		command: 'pnpm exec vite dev --port 5176',
		url: 'http://localhost:5176',
		reuseExistingServer: !process.env.CI
	},
	use: {
		baseURL: 'http://localhost:5176'
	}
});
