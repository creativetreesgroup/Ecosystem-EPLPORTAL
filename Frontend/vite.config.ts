import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vitest/config';

// Same backend path-prefix list as docker/Caddyfile's `@backend` matcher — kept in sync by hand,
// see that file's own comment. Only relevant for `pnpm dev` run OUTSIDE Docker (a containerized
// `docker compose up` run never uses Vite's dev server at all — `adapter-node`'s production build
// runs standalone, fronted by Caddy per Task 2). `reactor-core`'s dev port is 8081, matching
// `bin/reactor-core/src/main.rs`'s `TcpListener::bind("0.0.0.0:8081")`.
const BACKEND_PREFIXES = [
	'/healthz',
	'/auth',
	'/bookings',
	'/prices',
	'/locations',
	'/bot',
	'/branding',
	'/q',
	'/accept',
	'/ws'
];

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	server: {
		proxy: Object.fromEntries(
			BACKEND_PREFIXES.map((prefix) => [
				prefix,
				{ target: 'http://127.0.0.1:8081', changeOrigin: true, ws: prefix === '/ws' }
			])
		)
	},
	// Unit tests (vitest, colocated under src/**) vs e2e specs (Playwright, tests/*.spec.ts,
	// see playwright.config.ts's testDir) use the same *.spec.ts-ish naming; without this,
	// `vitest run`'s default glob also picks up tests/*.spec.ts and crashes on Playwright's
	// test() called outside its own runner.
	test: {
		include: ['src/**/*.{test,spec}.ts']
	}
});
