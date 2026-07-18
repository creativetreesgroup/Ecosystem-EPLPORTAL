// Frontend/src/routes/(app)/+layout.server.ts
// Session gate for every route inside the (app) group (/command and later sub-fases). The
// cookies.get(...) pre-check avoids an unnecessary fetch round-trip for the common no-session
// case; the fetch('/auth/me') call is the real authoritative check — SvelteKit's server-side
// `fetch` in a `load` function forwards the incoming request's cookies automatically to
// same-origin requests, no manual header-copying needed. Cookie name and response shape verified
// against Backend/crates/api-gateway/src/routes/auth.rs (session_auth-gated `me` handler,
// LoginResponse { username, display_name, is_main_account }) and its SESSION_COOKIE_NAME
// ("spx_session", Backend/.env.example).
import { redirect } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

// Matches LoginResponse (Backend/crates/api-gateway/src/routes/auth.rs) — kept here rather than
// `unknown` so descendant +page.svelte files (Fase 7d's /rules is the first) get a typed
// `data.user` from SvelteKit's automatic ancestor-load merge, no per-page cast needed.
type SessionUser = { username: string; display_name: string; is_main_account: boolean };

export const load: LayoutServerLoad = async ({ fetch, cookies }) => {
	if (!cookies.get('spx_session')) {
		redirect(307, '/login');
	}

	// Only the calls that can genuinely throw unexpectedly (network error, timeout, malformed
	// JSON) live in this try. The !res.ok check below runs outside it on purpose: redirect()
	// throws internally, and a redirect() call sitting inside this try would get swallowed by
	// its own catch below instead of propagating to SvelteKit.
	let res: Response;
	let user: SessionUser;
	try {
		res = await fetch('/auth/me');
		user = await res.json();
	} catch {
		// Fail-closed: treat any error as unauthenticated
		redirect(307, '/login');
	}

	if (!res.ok) {
		redirect(307, '/login');
	}

	return { user };
};
