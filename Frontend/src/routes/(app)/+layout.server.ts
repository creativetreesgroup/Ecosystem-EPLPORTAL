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

export const load: LayoutServerLoad = async ({ fetch, cookies }) => {
	if (!cookies.get('spx_session')) {
		redirect(307, '/login');
	}
	try {
		const res = await fetch('/auth/me');
		if (!res.ok) {
			redirect(307, '/login');
		}
		const user = await res.json();
		return { user };
	} catch {
		// Network error, timeout, malformed JSON, or redirect from !res.ok check
		// Fail-closed: treat any error as unauthenticated
		redirect(307, '/login');
	}
};
