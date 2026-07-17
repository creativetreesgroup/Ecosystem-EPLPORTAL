# Fase 7a (login page + minimal design foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the first piece of TOWER's frontend: a working, accessible login page styled with the validated "Balanced Duo" design tokens (dark+light, anti-flash), plus the two backend/infra fixes this page genuinely depends on to work end-to-end (Caddy routing, WS-auth-cookie).

**Architecture:** `Frontend/src/app.css`'s existing `@theme` placeholder gets filled with semantic `--color-*`/`--font-*`/`--radius-*` custom properties (dark values as the base, `[data-theme="light"]` overrides the same property names — Tailwind v4 utilities compile to `var(--color-*)` under the hood, so this cascades correctly without needing `dark:` variant duplication anywhere). `src/lib/tokens.ts` mirrors the color values in TS for future canvas/JS use (Latency Tape, 7b+). The login page itself uses a plain client-side `fetch()` to the already-shipped `POST /auth/portal-login` (matching this project's existing reference app's own pattern, and the design doc's approved data-flow — not a SvelteKit form action, a deliberate, disclosed choice: see Task 5's own note). Two small, surgical backend fixes make the page's actual network path work: `docker/Caddyfile` gets explicit per-prefix routing (today's `/api/*` rule matches none of api-gateway's real routes), and `ws-hub::ws_handler_with_auth` gains cookie-based session auth alongside its existing `?session=` query param (closing the Fase-6a tracked WS-auth-cookie gap), threaded through from `reactor-core`'s existing `AppState.session_cookie_name`.

**Tech Stack:** SvelteKit 2.69.2 (lockfile-resolved) / Svelte 5.56.4 (runes) / Tailwind v4.3.2 (`@theme`) / Vite 8.1.4 / adapter-node 5.5.7, all already scaffolded — zero new frontend framework dependencies. One new frontend dev dependency (`@playwright/test`, for Task 6's e2e test — nothing in this repo has an e2e test today). One new backend direct dependency (`axum-extra` promoted to `ws-hub`'s `Cargo.toml`, already resolved workspace-wide at 0.12.6 as `api-gateway`'s existing dependency — zero new `Cargo.lock` versions).

## Global Constraints

- Every color/font/radius value used in a `.svelte` file MUST come from a `--color-*`/`--font-*`/`--radius-*` custom property (Tailwind utility class or `var(--...)`) — **no raw hex/rgb literals in component markup**, per the master spec's explicit "no hex mentah di komponen" rule.
- Palette (validated via visual companion, 2026-07-17, "Balanced Duo" — exact values):
  - `--color-bg-base`: dark `#15181c` / light `#f7f6f3`
  - `--color-bg-surface` (cards/elevated): dark `#1c2025` / light `#ffffff`
  - `--color-border`: dark `#262b31` / light `#e4e1da`
  - `--color-text-primary`: dark `#f1f3f5` / light `#1c1e21`
  - `--color-text-muted`: dark `#8b95a1` / light `#5c5952`
  - `--color-accent` (amber — action/warning): dark `#eab308` / light `#b45309` (sharpened for AA on a light background, NOT the same value inverted — see design doc)
  - `--color-live` (teal — data/status-connected): dark `#2dd4bf` / light `#0d9488` (same AA-sharpening rationale)
  - `--color-danger` (error banners — not covered in brainstorming, minimal addition needed for the login page's error state, kept in the same muted/desaturated register as the rest of the palette): dark `#f87171` / light `#dc2626`
  - `--radius-md`: `0.5rem`, `--radius-lg`: `0.75rem` (card corners)
- Fonts: `--font-heading`: `'Space Grotesk', system-ui, sans-serif`; `--font-mono`: `'IBM Plex Mono', ui-monospace, monospace`; `--font-body`: `'Inter', system-ui, sans-serif`. Self-hosted (Task 1 downloads the actual font files — no Google Fonts CDN `<link>`, for FOUT/FOIT control and no third-party network dependency at runtime).
- Theme switch is `data-theme="dark"|"light"` on `<html>`, set by an inline script in `app.html` BEFORE any CSS paints (anti-flash) — reads `localStorage.theme`, falls back to `window.matchMedia('(prefers-color-scheme: light)')`, defaults to dark if neither is available.
- `POST /auth/portal-login`'s real contract (unchanged, Fase 6a): request `{username, password}`, response `{username, display_name, is_main_account}` + `Set-Cookie`. On non-2xx, this plan's UI shows the single generic string **"Username atau password salah"** — never a different message for unknown-username vs wrong-password (matches the backend's own enumeration-timing protection).
- Backend changes in this plan (Tasks 4) must NOT touch `api-gateway`'s `build_router`, CORS, or security-headers layering — `/ws` is deliberately mounted outside all three (see `bin/reactor-core/src/main.rs::app`'s own doc comment) and this plan does not revisit that decision.
- `cargo fmt`/`cargo clippy --workspace --all-targets -- -D warnings`/`cargo test --workspace -- --test-threads=1` must stay clean after every backend-touching task. `pnpm check` (svelte-check) must stay clean after every frontend-touching task.

---

## Task 1: Design tokens — `app.css`, `app.html`, `lib/tokens.ts`

**Files:**
- Modify: `Frontend/src/app.css`
- Modify: `Frontend/src/app.html`
- Create: `Frontend/src/lib/tokens.ts`
- Create: `Frontend/src/lib/fonts/` (self-hosted font files — see Step 1)

**Interfaces:**
- Produces (for Task 5): every `--color-*`/`--font-*`/`--radius-*` custom property listed in Global Constraints, usable as Tailwind utilities (`bg-bg-base`, `text-text-primary`, etc.) or `var(--color-*)`.
- Produces (for future canvas work, 7b+): `lib/tokens.ts`'s exported `TOKENS` object, values byte-identical to `app.css`'s custom properties (single source of truth is `app.css`; `tokens.ts` is a hand-kept mirror — a comment in both files points at the other, so a future edit to one is a visible prompt to check the sibling).

- [ ] **Step 1: Self-host the three font families**

Download (or vendor via `pnpm add`) `Space Grotesk` (weight 700 only — this plan uses it for headings, one weight is sufficient today, add more weights in a later sub-phase if a design need arises), `IBM Plex Mono` (weight 400+600), `Inter` (weight 400+500+700) as `.woff2` files into `Frontend/src/lib/fonts/`. Use `@fontsource/space-grotesk`, `@fontsource/ibm-plex-mono`, `@fontsource/inter` (npm packages that ship pre-built self-hostable `.woff2` + CSS — the standard, maintained way to self-host Google Fonts without a CDN, avoids hand-downloading font binaries into the repo):

```bash
cd Frontend && pnpm add @fontsource/space-grotesk @fontsource/ibm-plex-mono @fontsource/inter
```

- [ ] **Step 2: Import the font faces and fill `@theme`**

```css
/* Frontend/src/app.css */
@import 'tailwindcss';

/* Space Grotesk 700 (headings), IBM Plex Mono 400/600 (data/mono text), Inter 400/500/700
   (body/UI) — self-hosted via @fontsource, no CDN. */
@import '@fontsource/space-grotesk/700.css';
@import '@fontsource/ibm-plex-mono/400.css';
@import '@fontsource/ibm-plex-mono/600.css';
@import '@fontsource/inter/400.css';
@import '@fontsource/inter/500.css';
@import '@fontsource/inter/700.css';

/* Fase 7 "TOWER" design tokens — "Balanced Duo" (graphite + amber + teal), validated via
   visual-companion brainstorming 2026-07-17 (see
   Docs/superpowers/specs/2026-07-17-fase-7a-login-design-foundation-design.md). Dark values are
   the base (this app's primary aesthetic is a dark "Command Center" console); [data-theme="light"]
   below overrides the SAME custom-property names — Tailwind v4 utilities compile to
   `var(--color-*)`, so every `bg-bg-base`/`text-text-primary`/etc. utility automatically follows
   whichever theme is active with zero `dark:`-variant duplication anywhere in component markup.
   Keep this block and Frontend/src/lib/tokens.ts's TOKENS object in sync — see that file's own
   comment. */
@theme {
	--color-bg-base: #15181c;
	--color-bg-surface: #1c2025;
	--color-border: #262b31;
	--color-text-primary: #f1f3f5;
	--color-text-muted: #8b95a1;
	--color-accent: #eab308;
	--color-live: #2dd4bf;
	--color-danger: #f87171;

	--radius-md: 0.5rem;
	--radius-lg: 0.75rem;

	--font-heading: 'Space Grotesk', system-ui, sans-serif;
	--font-mono: 'IBM Plex Mono', ui-monospace, monospace;
	--font-body: 'Inter', system-ui, sans-serif;
}

/* Light-mode overrides — amber/teal are DELIBERATELY different values from the dark tokens
   above (sharpened/darkened), not simply reused: the dark-mode amber (#eab308) and teal
   (#2dd4bf) both fail WCAG 2.2 AA's 4.5:1 text-contrast ratio against a light background.
   Verified with a real contrast-ratio tool (not eyeballed) as part of this task's own
   verification step. */
[data-theme='light'] {
	--color-bg-base: #f7f6f3;
	--color-bg-surface: #ffffff;
	--color-border: #e4e1da;
	--color-text-primary: #1c1e21;
	--color-text-muted: #5c5952;
	--color-accent: #b45309;
	--color-live: #0d9488;
	--color-danger: #dc2626;
}

body {
	background-color: var(--color-bg-base);
	color: var(--color-text-primary);
	font-family: var(--font-body);
}

/* prefers-reduced-motion: disable/shrink every transition and animation globally, rather than
   auditing each component individually — the cheapest correct way to honor this preference
   project-wide as more components are added in later sub-phases. */
@media (prefers-reduced-motion: reduce) {
	*, *::before, *::after {
		animation-duration: 0.01ms !important;
		animation-iteration-count: 1 !important;
		transition-duration: 0.01ms !important;
		scroll-behavior: auto !important;
	}
}
```

- [ ] **Step 3: Anti-flash theme script in `app.html`**

Read the CURRENT `Frontend/src/app.html` first (Fase-0 scaffold default) — modify it to add the script, keep everything else (SvelteKit's `%sveltekit.head%`/`%sveltekit.body%` placeholders) exactly as-is:

```html
<!doctype html>
<html lang="id">
	<head>
		<meta charset="utf-8" />
		<link rel="icon" href="%sveltekit.assets%/favicon.svg" />
		<meta name="viewport" content="width=device-width, initial-scale=1" />
		<script>
			// Anti-flash: set data-theme on <html> BEFORE any CSS paints, so the browser never
			// renders one theme then swaps to another. localStorage wins if the user has
			// explicitly chosen before; otherwise fall back to the OS preference; otherwise dark
			// (this app's primary aesthetic). Deliberately synchronous, no async/defer — it MUST
			// run before first paint.
			(function () {
				try {
					var stored = localStorage.getItem('theme');
					var theme =
						stored === 'light' || stored === 'dark'
							? stored
							: window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches
								? 'light'
								: 'dark';
					document.documentElement.setAttribute('data-theme', theme);
				} catch (e) {
					document.documentElement.setAttribute('data-theme', 'dark');
				}
			})();
		</script>
		%sveltekit.head%
	</head>
	<body data-sveltekit-preload-data="hover">
		<div style="display: contents">%sveltekit.body%</div>
	</body>
</html>
```

- [ ] **Step 4: `lib/tokens.ts` — TS mirror**

```typescript
// Frontend/src/lib/tokens.ts
// Hand-kept mirror of Frontend/src/app.css's @theme block — for canvas/JS contexts (Latency
// Tape, 7b+) that can't reference CSS custom properties directly. Values MUST stay byte-identical
// to app.css; that file's own top comment points back here. No automated sync exists (deliberately
// simple for two small token sets — revisit if this drifts in practice across future sub-phases).
export const TOKENS = {
	dark: {
		bgBase: '#15181c',
		bgSurface: '#1c2025',
		border: '#262b31',
		textPrimary: '#f1f3f5',
		textMuted: '#8b95a1',
		accent: '#eab308',
		live: '#2dd4bf',
		danger: '#f87171'
	},
	light: {
		bgBase: '#f7f6f3',
		bgSurface: '#ffffff',
		border: '#e4e1da',
		textPrimary: '#1c1e21',
		textMuted: '#5c5952',
		accent: '#b45309',
		live: '#0d9488',
		danger: '#dc2626'
	}
} as const;

export type ThemeName = keyof typeof TOKENS;
```

- [ ] **Step 5: Verify contrast ratios with a real tool**

Run (no new dependency — `npx` fetches a one-off CLI):
```bash
npx wcag-contrast-checker "#b45309" "#f7f6f3"
npx wcag-contrast-checker "#0d9488" "#f7f6f3"
npx wcag-contrast-checker "#1c1e21" "#f7f6f3"
```
Expected: all three ratios ≥ 4.5:1 (normal text AA). If `wcag-contrast-checker` isn't a real/reliable npm package by the time this runs, compute the ratio by hand using the WCAG relative-luminance formula (both colors' sRGB → linear → relative luminance → `(L1+0.05)/(L2+0.05)`) and show the arithmetic in the task report — do not simply assert compliance without a computed number.

- [ ] **Step 6: Run frontend checks**

```bash
cd Frontend && pnpm install && pnpm check
```
Expected: `pnpm install` succeeds (adds the 3 `@fontsource/*` packages), `svelte-check` reports 0 errors (the scaffold has no components yet to break).

- [ ] **Step 7: Commit**

```bash
git add Frontend/src/app.css Frontend/src/app.html Frontend/src/lib/tokens.ts \
        Frontend/package.json Frontend/pnpm-lock.yaml
git commit -m "feat(frontend): Balanced Duo design tokens — dark/light @theme, anti-flash, self-hosted fonts"
```

---

## Task 2: `docker/Caddyfile` — real per-prefix routing

**Files:**
- Modify: `docker/Caddyfile`

**Interfaces:**
- Consumes: nothing new — `api-gateway`'s already-shipped route mount points (`/healthz`, `/auth`, `/bookings`, `/prices`, `/locations`, `/bot`, `/branding`, `/q`, `/accept`) plus `ws-hub`'s `/ws` (all confirmed via `grep -rn '\.nest(' Backend/crates/api-gateway/src/lib.rs` and `bin/reactor-core/src/main.rs`'s `ws_router` merge, both read during this plan's own research).

- [ ] **Step 1: Read the current Caddyfile in full**

Read `docker/Caddyfile` (19 lines, already summarized in this plan's own research above) — confirm the exact current `handle_path`/`reverse_proxy` block shape and the `header_up X-Forwarded-For {remote_host}` security comment before editing, so the fix preserves that directive exactly (it is load-bearing for `SmartIpKeyExtractor`'s rate-limit trust invariant — do not drop or reorder it).

- [ ] **Step 2: Replace the single `/api/*` rule with explicit per-prefix matching**

```caddyfile
# docker/Caddyfile
# Security note (Fase 6a Task 8 review finding): `header_up X-Forwarded-For
# {remote_host}` below OVERWRITES the header with Caddy's real, non-spoofable
# observed peer address (`{remote_host}` is the documented shorthand for
# `{http.request.remote.host}`) instead of Caddy's bare `reverse_proxy`
# default, which only AUGMENTS/appends to any client-supplied value. Without
# this override, a client could set `X-Forwarded-For: <anything>` and have it
# forwarded as `<anything>, <real-ip>` — `reactor-core`'s
# `middleware/rate_limit.rs` login rate limiter keys on the LEFTMOST value via
# `tower_governor::SmartIpKeyExtractor`, so an unsanitized header would let an
# attacker rotate it per request for an unlimited rate-limit budget, defeating
# the limiter entirely. Do not remove this directive without also revisiting
# `rate_limit.rs`'s trust-invariant doc comment.
#
# Fase 7a fix (this file's rule previously only matched `/api/*`, which no
# route in `api-gateway`/`ws-hub` has ever actually used — every real mount
# point is listed explicitly below instead, matching Fase 8's own documented
# routing scheme ("label routing /api /auth /ws -> reactor-core, else web")
# so the VPS Traefik overlay can mirror this list verbatim with "Nol
# perubahan kode", per the master spec's own promise for that swap).
:80 {
	@backend path /healthz /auth/* /bookings/* /prices /prices/* /locations /locations/* \
		/bot/* /branding /branding/* /q/* /accept/* /ws

	handle @backend {
		reverse_proxy tower-reactor-core:8081 {
			header_up X-Forwarded-For {remote_host}
		}
	}

	handle {
		reverse_proxy tower-web:3000 {
			header_up X-Forwarded-For {remote_host}
		}
	}
}
```

**Note on `/prices`/`/branding` (bare, no trailing segment):** both are mounted in `api-gateway` as exact routes with real handlers at the bare path (`GET /prices`, `GET /branding`) AND have sub-verbs at the same bare path (`POST/PUT/DELETE /prices`, `PUT /branding`) — Caddy's `path` matcher needs the bare path listed explicitly (`/prices`) in addition to the wildcard (`/prices/*`, for any future nested route) since a bare `/prices/*` glob does NOT match the exact string `/prices` with no trailing slash. Verify this matches Caddy's actual documented `path` matcher semantics before treating this task as done — if wrong, the bare-path routes 404 through the edge even though they work when hit directly against `reactor-core`'s dev port.

- [ ] **Step 3: Verify via `docker compose up` end-to-end**

Run (Postgres/Redis dev containers already up from prior sub-phases; this brings up the full stack including Caddy):
```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
docker compose -f docker/docker-compose.yml up -d --build
sleep 5
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1/healthz   # expect 200, via Caddy -> reactor-core
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1/branding  # expect 200, via Caddy -> reactor-core
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1/          # expect 200, via Caddy -> tower-web (SvelteKit)
```
Expected: all three return `200` (the first two prove the new `@backend` matcher works for a real un-authed GET; the third proves the fallback `handle` block to `tower-web` still works, unaffected by this change). If `tower-web`'s image doesn't build yet (Task 1-5's frontend code may not be containerized in this exact task), it's acceptable for the third check to fail with a container-startup error rather than a Caddy-routing error — read the actual failure and confirm it's NOT a 404/wrong-service-reached before treating it as an acceptable pre-existing gap; if it IS a Caddy-routing problem, that's this task's own bug, fix it.

- [ ] **Step 4: Commit**

```bash
git add docker/Caddyfile
git commit -m "fix(docker): route real backend prefixes through Caddy (previous /api/* rule matched nothing)"
```

---

## Task 3: Vite dev proxy (local `pnpm dev` without Docker)

**Files:**
- Modify: `Frontend/vite.config.ts`

**Interfaces:**
- Consumes: same prefix list as Task 2's Caddyfile — kept as one literal list duplicated in two config languages (Caddyfile syntax vs. JS array), each file's own comment points at the other as the source-of-truth-for-the-LIST (not full DRY across two different config languages, but the two lists must never silently drift — this task's own commit message says so explicitly for anyone diffing later).

- [ ] **Step 1: Add the proxy config**

```typescript
// Frontend/vite.config.ts
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

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
	}
});
```

- [ ] **Step 2: Verify**

With `reactor-core` running locally on port 8081 (or skip this live check if it's not running in this task's environment and note that in the report — the config's correctness is what this task verifies, not a live end-to-end call, which Task 6 covers properly):
```bash
cd Frontend && pnpm dev &
sleep 3
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:5173/healthz
kill %1
```
Expected: `200` if `reactor-core` was reachable on 8081, or a clear connection-refused error (not a SvelteKit 404) if it wasn't — a 404 here would mean the proxy config itself is wrong, which IS this task's bug to fix.

- [ ] **Step 3: Commit**

```bash
git add Frontend/vite.config.ts
git commit -m "feat(frontend): vite dev proxy to reactor-core (local dev without Docker)"
```

---

## Task 4: WS-auth-cookie fix — `ws-hub` accepts the session cookie

**Files:**
- Modify: `Backend/crates/ws-hub/Cargo.toml`
- Modify: `Backend/crates/ws-hub/src/hub.rs`
- Modify: `Backend/bin/reactor-core/src/main.rs`
- Test: `Backend/crates/ws-hub/tests/session_validated_ws.rs` (extend existing file)

**Interfaces:**
- Consumes: `axum_extra::extract::CookieJar` (new direct dependency, already resolved workspace-wide at 0.12.6 as `api-gateway`'s dependency — same "promote to direct edge" pattern this project has used repeatedly, e.g. Fase 6e's `hmac`/`uuid` promotions).
- Produces: `ws_router_with_auth`'s signature gains one new parameter (`cookie_name: Arc<str>`); `ws_handler_with_auth` falls back to the cookie when `?session=` is empty.

- [ ] **Step 1: Add `axum-extra` to `ws-hub`**

```toml
# Backend/crates/ws-hub/Cargo.toml — add to [dependencies]
axum-extra = { version = "0.12.6", features = ["cookie"] }
```

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && cargo tree -p ws-hub -i axum-extra` — confirm it resolves to `0.12.6` (the already-present version) with no new `Cargo.lock` `[[package]]` entry, only a new dependency-graph edge.

- [ ] **Step 2: Read the current `ws_handler_with_auth`/`ws_router_with_auth` in full**

Read `Backend/crates/ws-hub/src/hub.rs`'s current `ws_handler_with_auth` (takes `State((hub, validator)): State<(Arc<Hub>, SessionValidator)>`) and `ws_router_with_auth` (builds that 2-tuple state) — already summarized in this plan's own research above; confirm nothing has shifted before editing.

- [ ] **Step 3: Extend the state tuple and the handler**

```rust
// Backend/crates/ws-hub/src/hub.rs — modify ws_handler_with_auth and ws_router_with_auth
use axum_extra::extract::CookieJar;

/// Validated upgrade path. Rejects with `401 Unauthorized` BEFORE
/// `ws.on_upgrade` ever runs for a missing/empty/invalid/expired session —
/// the WS handshake never completes in that case, rather than accepting
/// the connection and immediately closing it.
///
/// **Fase 7a addition:** the session token can now come from EITHER the
/// `?session=` query param (unchanged, kept for test/tooling convenience —
/// `session_validated_ws.rs`'s existing tests still use it directly) OR the
/// `cookie_name`-named `HttpOnly` cookie a real browser sends automatically
/// (closing the Fase-6a tracked gap: a browser has no way to read an
/// `HttpOnly` cookie's value to construct a `?session=` query string itself
/// — see the shared design doc's tracked note). The query param wins if
/// BOTH are present (keeps existing test behavior byte-identical); the
/// cookie is used ONLY when the query param is empty.
pub async fn ws_handler_with_auth(
    ws: WebSocketUpgrade,
    State((hub, validator, cookie_name)): State<(Arc<Hub>, SessionValidator, Arc<str>)>,
    Query(mut q): Query<WsQuery>,
    jar: CookieJar,
) -> Response {
    if q.session.is_empty() {
        if let Some(cookie) = jar.get(&*cookie_name) {
            q.session = cookie.value().to_string();
        }
    }
    if q.session.is_empty() || !(validator)(q.session.clone()).await {
        return (StatusCode::UNAUTHORIZED, "invalid session").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, hub, q))
}

/// Same `/ws` route shape as `ws_router` (existing, unchanged, no-auth) but
/// requiring `validator` to confirm a session (from `?session=` OR the
/// `cookie_name` cookie) is valid before the handshake completes.
/// Authentication only — no `is_main_account`/RBAC check here.
pub fn ws_router_with_auth(
    hub: Arc<Hub>,
    validator: SessionValidator,
    cookie_name: Arc<str>,
) -> Router {
    Router::new()
        .route("/ws", get(ws_handler_with_auth))
        .with_state((hub, validator, cookie_name))
}
```

- [ ] **Step 4: Update the one call site**

```rust
// Backend/bin/reactor-core/src/main.rs — inside fn app(state: AppState)
fn app(state: AppState) -> Router {
    let ws_router = ws_hub::ws_router_with_auth(
        state.ws_hub.clone(),
        session_validator(state.poller.pool.clone()),
        state.session_cookie_name.clone(),
    );
    api_gateway::build_router(state).merge(ws_router)
}
```

`AppState.session_cookie_name` is already `Arc<str>` (confirmed: `Backend/crates/api-gateway/src/state.rs`) — `.clone()` is a cheap `Arc` bump, matching the existing `state.poller.pool.clone()` call right above it.

- [ ] **Step 5: Write the failing test**

Read `Backend/crates/ws-hub/tests/session_validated_ws.rs`'s existing test(s) in full first (it already seeds a real `tenants`/`portal_users`/`portal_sessions` row and connects via `tokio-tungstenite` — reuse that exact setup, do not duplicate it). Add:

```rust
// Backend/crates/ws-hub/tests/session_validated_ws.rs — append
#[tokio::test]
async fn cookie_only_session_with_no_query_param_upgrades_successfully() {
    // Reuse this file's existing seed-a-real-session helper (read the file to find its exact
    // name — every prior test in this file already does this) to get a genuine plaintext token
    // and its owning tenant/user, exactly like the existing `valid_session_upgrade_succeeds...`
    // test does, but WITHOUT appending `?session=<token>` to the connect URL this time.
    //
    // Connect via `tokio_tungstenite::connect_async` with a plain `ws://127.0.0.1:<port>/ws`
    // URL (no query string at all) and an explicit `Cookie: <cookie_name>=<token>` header on the
    // handshake request (tokio-tungstenite's `IntoClientRequest` + manually inserting a header
    // via `http::Request::builder()...header("Cookie", format!("{cookie_name}={token}"))...` —
    // check this file's existing imports/helpers for the exact request-building pattern already
    // established, match it rather than inventing a new one).
    //
    // Assert the upgrade succeeds (matches this file's existing success-path assertion shape —
    // e.g. reading the `connected` greeting event) using the cookie ALONE, proving a real browser
    // (which sends cookies automatically, never a hand-constructed query string) can now
    // authenticate this route.
}

#[tokio::test]
async fn bogus_cookie_with_no_query_param_is_rejected_before_upgrade() {
    // Mirror this file's existing `bogus_session_rejected_before_handshake` test, but supply the
    // bogus token via a `Cookie:` header instead of `?session=` — assert the SAME pre-upgrade
    // 401 rejection (not a 101 that then immediately closes).
}
```

Fill in the actual test bodies against the real helpers/imports already in this file — the plan's own author did not have full visibility into this test file's exact helper function names while writing this task, so this is deliberately a shape-and-intent specification, not verbatim code (unlike every other step in this plan, which IS complete runnable code) — read the file first, then write real, complete test bodies matching its established conventions exactly.

- [ ] **Step 6: Run the tests, then full crate + workspace verification**

Run: `cargo test -p ws-hub -- --test-threads=1` — all tests in `session_validated_ws.rs` pass, including the 2 new ones.
Run: `cargo test -p reactor-core -- --test-threads=1` (or `cargo build -p reactor-core` if this bin crate has no dedicated test suite — confirm which) — the `fn app` call-site change compiles and any existing coverage still passes.
Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — this task changes a public fn signature (`ws_router_with_auth`) with exactly one call site; a full workspace check confirms nothing else references the old 2-arg signature.

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/ws-hub/Cargo.toml Backend/crates/ws-hub/src/hub.rs \
        Backend/bin/reactor-core/src/main.rs Backend/crates/ws-hub/tests/session_validated_ws.rs \
        Backend/Cargo.lock
git commit -m "feat(ws-hub): accept session token from HttpOnly cookie, not just ?session= (closes Fase-6a tracked gap)"
```

---

## Task 5: Login page

**Files:**
- Create: `Frontend/src/lib/api.ts`
- Create: `Frontend/src/routes/login/+page.svelte`
- Create: `Frontend/src/routes/+page.server.ts` (redirect `/` → `/login` — see Step 1's note)

**Interfaces:**
- Consumes: Task 1's design tokens (Tailwind utilities / `var(--color-*)`), `GET /branding` (Fase 6d, existing, public), `POST /auth/portal-login` (Fase 6a, existing).

- [ ] **Step 1: Root route redirect**

The Fase-0 scaffold's `src/routes/+page.svelte` is a placeholder with no real content. This plan does not build `/command` yet (7b's job) — so `/` needs to send a visitor somewhere real today. Read the current `Frontend/src/routes/+page.svelte` first, then replace the route's behavior with a server-side redirect to `/login` (simplest correct choice — no session-check-then-branch logic yet, since there's no way to verify a session server-side without a shared crypto/DB dependency SvelteKit doesn't have; that arrives when 7b needs real session-aware routing):

```typescript
// Frontend/src/routes/+page.server.ts
import { redirect } from '@sveltejs/kit';

export function load() {
	redirect(307, '/login');
}
```

Delete `Frontend/src/routes/+page.svelte`'s scaffold content (no component needed at `/` once its `load` always redirects — SvelteKit still requires SOME `+page.svelte` to exist for the route to be valid; keep it as an empty component, e.g. `<!-- redirects server-side, see +page.server.ts -->`, not deleted entirely).

- [ ] **Step 2: `lib/api.ts` — thin fetch helper**

```typescript
// Frontend/src/lib/api.ts
// Thin fetch wrapper for calling reactor-core. Paths are RELATIVE — in production this goes
// through Caddy (docker/Caddyfile's @backend matcher, Task 2), in local `pnpm dev` through
// Vite's proxy (vite.config.ts, Task 3). Never construct an absolute backend URL here; that
// would bypass both routing layers and reintroduce the CORS problem they exist to avoid.
export class ApiError extends Error {
	constructor(
		public status: number,
		message: string
	) {
		super(message);
	}
}

export async function apiPost<T>(path: string, body: unknown): Promise<T> {
	const res = await fetch(path, {
		method: 'POST',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!res.ok) {
		throw new ApiError(res.status, 'request failed');
	}
	return res.json();
}
```

- [ ] **Step 3: `routes/login/+page.svelte`**

```svelte
<!-- Frontend/src/routes/login/+page.svelte -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { apiPost, ApiError } from '$lib/api';

	let username = $state('');
	let password = $state('');
	let showPassword = $state(false);
	let loading = $state(false);
	let errorMsg = $state('');

	let siteName = $state('TOWER');
	let brandTag = $state('');

	$effect(() => {
		// Public, no-session branding — best-effort, a fetch failure just keeps the defaults
		// above rather than blocking the page (this is decoration, not a requirement to log in).
		fetch('/branding')
			.then((r) => (r.ok ? r.json() : null))
			.then((b) => {
				if (b) {
					siteName = b.site_name || siteName;
					brandTag = b.brand_tag || '';
				}
			})
			.catch(() => {});
	});

	const canSubmit = $derived(username.trim().length > 0 && password.length > 0);

	async function login() {
		if (!canSubmit || loading) return;
		loading = true;
		errorMsg = '';
		try {
			await apiPost('/auth/portal-login', { username: username.trim(), password });
			await goto('/command');
		} catch (e) {
			errorMsg =
				e instanceof ApiError ? 'Username atau password salah' : 'Tidak dapat menghubungi server. Coba lagi.';
		} finally {
			loading = false;
		}
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && canSubmit) login();
	}
</script>

<svelte:head>
	<title>{siteName} — Masuk</title>
</svelte:head>

<div class="min-h-screen flex items-center justify-center p-4 bg-bg-base">
	<div class="w-full max-w-[380px]">
		<div class="text-center mb-8">
			<div
				class="w-14 h-14 rounded-lg bg-accent/15 border border-accent/30 flex items-center justify-center mx-auto mb-3"
			>
				<span class="font-heading font-bold text-accent text-lg">T</span>
			</div>
			<div class="flex items-center justify-center gap-2">
				<h1 class="font-heading text-[22px] font-bold text-text-primary tracking-tight">{siteName}</h1>
				{#if brandTag}
					<span class="px-2 py-0.5 rounded-md text-[12px] font-bold tracking-wide bg-accent text-bg-base"
						>{brandTag}</span
					>
				{/if}
			</div>
		</div>

		<div class="rounded-lg border border-border bg-bg-surface overflow-hidden">
			<div class="px-5 py-3.5 border-b border-border">
				<span class="font-body text-[10px] font-bold text-text-muted uppercase tracking-[0.12em]"
					>Masuk ke Portal</span
				>
			</div>

			<div class="p-5 space-y-4">
				{#if errorMsg}
					<div
						role="alert"
						aria-live="polite"
						class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border"
						style="background:color-mix(in srgb, var(--color-danger) 10%, transparent); color:var(--color-danger); border-color:color-mix(in srgb, var(--color-danger) 30%, transparent)"
					>
						{errorMsg}
					</div>
				{/if}

				<div class="space-y-1.5">
					<label for="login-username" class="block text-[11px] font-semibold text-text-muted uppercase tracking-widest font-body"
						>Username</label
					>
					<input
						id="login-username"
						type="text"
						bind:value={username}
						onkeydown={onKeydown}
						placeholder="Username portal"
						autocomplete="username"
						spellcheck="false"
						class="w-full min-h-[44px] px-3 py-2.5 rounded-lg text-[14px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</div>

				<div class="space-y-1.5">
					<label for="login-password" class="block text-[11px] font-semibold text-text-muted uppercase tracking-widest font-body"
						>Password</label
					>
					<div class="relative">
						<input
							id="login-password"
							type={showPassword ? 'text' : 'password'}
							bind:value={password}
							onkeydown={onKeydown}
							placeholder="••••••••••"
							autocomplete="current-password"
							class="w-full min-h-[44px] px-3 pr-12 py-2.5 rounded-lg text-[14px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						/>
						<button
							type="button"
							onclick={() => (showPassword = !showPassword)}
							aria-pressed={showPassword}
							class="absolute inset-y-0 right-0 flex items-center px-3 min-w-[44px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-lg"
						>
							<span class="sr-only">{showPassword ? 'Sembunyikan password' : 'Tampilkan password'}</span>
							<span aria-hidden="true" class="text-[11px] font-body">{showPassword ? 'Sembunyikan' : 'Tampilkan'}</span>
						</button>
					</div>
				</div>

				<button
					type="button"
					onclick={login}
					disabled={!canSubmit || loading}
					class="w-full min-h-[44px] py-2.5 rounded-lg text-[13px] font-bold font-body transition-opacity bg-accent text-bg-base hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					{loading ? 'Memverifikasi…' : 'Masuk ke Portal'}
				</button>
			</div>
		</div>
	</div>
</div>
```

- [ ] **Step 4: Run `pnpm check`**

```bash
cd Frontend && pnpm check
```
Expected: 0 errors. `color-mix(in srgb, ...)` and Tailwind arbitrary/utility classes referencing the `@theme` tokens (`bg-bg-base`, `text-text-primary`, `border-border`, `bg-accent`, etc.) must resolve — if any utility class doesn't compile (e.g. `bg-bg-base` not recognized), check Task 1's exact `--color-*` naming against Tailwind v4's real namespace-to-utility mapping (re-verify against installed `node_modules/tailwindcss` once `pnpm install` has actually run, per this plan's own research caveat) and adjust either the token names or the class names to match — do not silently fall back to inline hex.

- [ ] **Step 5: Commit**

```bash
git add Frontend/src/lib/api.ts Frontend/src/routes/login/+page.svelte \
        Frontend/src/routes/+page.svelte Frontend/src/routes/+page.server.ts
git commit -m "feat(frontend): login page (Centered Card layout, Balanced Duo tokens)"
```

---

## Task 6: E2E test + final verification

**Files:**
- Modify: `Frontend/package.json`
- Create: `Frontend/playwright.config.ts`
- Create: `Frontend/tests/login.spec.ts`

**Interfaces:** none new — this task only adds test tooling and a real end-to-end test exercising every prior task's work together.

- [ ] **Step 1: Add Playwright**

```bash
cd Frontend && pnpm add -D @playwright/test && pnpm exec playwright install --with-deps chromium
```

- [ ] **Step 2: Config**

```typescript
// Frontend/playwright.config.ts
import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: 'tests',
	webServer: {
		command: 'pnpm dev',
		url: 'http://127.0.0.1:5173',
		reuseExistingServer: !process.env.CI
	},
	use: {
		baseURL: 'http://127.0.0.1:5173'
	}
});
```

- [ ] **Step 3: Write the e2e test**

Requires `reactor-core` running locally on port 8081 with a seeded `portal_users` row (Postgres/Redis dev containers already up from prior sub-phases) — this test genuinely exercises the full path: SvelteKit dev server → Vite proxy (Task 3) → real `reactor-core` → real Postgres, not a mock.

```typescript
// Frontend/tests/login.spec.ts
import { test, expect } from '@playwright/test';

// This test needs a REAL portal_users row to log in with. Seed one directly via the store crate
// before running (a one-off `cargo run --bin` helper or a `psql` insert using
// `spx_client::crypto::password::hash_password` — check `Backend/crates/store/src/lib.rs`'s own
// test helpers for the exact insert shape already established, e2e-test setup should mirror it,
// not invent a new one). Document the exact seed command in this file's own top comment once
// written — this plan's author did not have a ready-made seed script to reference verbatim.

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
	await page.keyboard.press('Tab');
	await page.keyboard.type('e2e-test-user');
	await page.keyboard.press('Tab');
	await page.keyboard.type('correct-horse-battery-staple');
	await page.keyboard.press('Enter');
	await expect(page).toHaveURL(/\/command/);
});
```

- [ ] **Step 4: Run the e2e suite**

```bash
cd Frontend && pnpm exec playwright test
```
Expected: all 4 tests pass. If `/command` doesn't exist yet (Task 5's disclosed, deliberate gap — 7b's job), the first/last tests' `toHaveURL(/\/command/)` assertion should still pass (the REDIRECT itself is what's being tested, not that `/command` renders a real page — SvelteKit's default 404 page still has that URL) — if this assertion fails for a different reason, investigate before assuming it's the known gap.

- [ ] **Step 5: Full backend + frontend verification**

```bash
# Backend (from repo root)
cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && unset DATABASE_URL && export REDIS_URL="redis://127.0.0.1:16379"
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check

# Frontend
cd ../Frontend
pnpm check
pnpm build
```
Expected: 0 failures/warnings across all of the above.

- [ ] **Step 6: Commit**

```bash
git add Frontend/package.json Frontend/pnpm-lock.yaml Frontend/playwright.config.ts Frontend/tests/login.spec.ts
git commit -m "test(fase-7a): login page e2e (Playwright) — full workspace + frontend verification"
```

---

## Self-Review Notes (writing-plans skill, run by the plan author before handoff)

**Spec coverage:** every element of the approved design doc (`Docs/superpowers/specs/2026-07-17-fase-7a-login-design-foundation-design.md`) has a task — Balanced Duo tokens + anti-flash + self-hosted fonts (Task 1), Caddyfile fix (Task 2), dev proxy (Task 3, a design-doc detail not literally in the "Struktur file" list but required by the doc's own "Alur data" section), WS-auth-cookie fix (Task 4), the Centered Card login page itself (Task 5), e2e proof + full verification (Task 6).

**Placeholder scan:** two steps are deliberately marked as shape-not-verbatim (Task 4 Step 5's test bodies, Task 6 Step 3's seed-command comment) because this plan's author could not read `ws-hub/tests/session_validated_ws.rs`'s exact existing helper names, or find a ready-made e2e seed script, while writing this plan — both are disclosed explicitly with the precise requirement stated, matching the established convention from Fase 6e's plan (Task 2/3's similar disclosed gaps, which the actual implementers then closed by reading the real files first). Every other step is complete, runnable code.

**Type/name consistency:** `--color-*` token names are used identically across Task 1 (`app.css` definition), Task 1 (`tokens.ts` mirror, camelCase JS equivalents of the same values), and Task 5 (Tailwind utility classes / `var()` calls in the login page). `ws_router_with_auth`'s new 3rd parameter (`cookie_name: Arc<str>`) is threaded identically through Task 4's `hub.rs` definition and its one call site in `main.rs`. The `BACKEND_PREFIXES` list (Task 3) and the Caddyfile's `@backend path` list (Task 2) enumerate the same 9 prefixes — verified by hand, cross-referenced against `api-gateway/src/lib.rs`'s real `.nest(...)` calls plus `ws-hub`'s `/ws`.

**Cross-task dependency order:** Task 1 (tokens) is independent of everything else and must land before Task 5 (login page consumes its classes). Task 2 (Caddyfile) and Task 3 (Vite proxy) are independent of each other and of every other task — both could run in parallel with Tasks 1/4, but this plan sequences them serially per subagent-driven-development's own rule (never parallel implementers). Task 4 (WS-auth) is fully independent of Tasks 1-3/5 — it's pure backend, included in this plan because of its product/sequencing rationale (see design doc), not a code dependency. Task 5 depends on Task 1 (tokens) and, for full network functionality, Tasks 2/3 (routing) — though Task 5's own `pnpm check` step doesn't require a live backend, only Task 6's e2e test does. Task 6 depends on every prior task being genuinely done (it's the integration proof). This ordering (1 → {2,3} → 4 → 5 → 6, with 4 movable earlier/later relative to 2/3/5 without breaking anything) is a valid topological sort.
