// Pure logic for /settings/sub-users — no fetch, no DOM.

/** Mirrors the backend's own minimum (Backend/crates/api-gateway/src/routes/portal_users.rs::
 * create: `body.password.len() < 8`). */
export function validatePassword(password: string): string | null {
	if (password.length < 8) {
		return 'Password minimal 8 karakter';
	}
	return null;
}

/** "Is this list row the currently logged-in session's own account?" — the frontend has no
 * portal_user id to compare (Frontend/src/routes/(app)/+layout.server.ts's SessionUser type
 * carries only {username, display_name, is_main_account}), so this compares usernames, which
 * are tenant-unique. Exact-match (===), matching the backend's own `username = $2` comparison
 * (Backend/crates/store/src/portal_users.rs::find_by_username) — no case-folding either side. */
export function isSelf(username: string, sessionUsername: string): boolean {
	return username === sessionUsername;
}
