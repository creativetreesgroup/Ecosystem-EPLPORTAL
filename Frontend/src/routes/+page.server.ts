// Frontend/src/routes/+page.server.ts
// `/` has no content of its own yet — this plan does not build `/command` (7b's job), and there's
// no way to verify a session server-side without a shared crypto/DB dependency SvelteKit doesn't
// have here, so a session-check-then-branch redirect isn't possible today. Simplest correct choice
// until 7b needs real session-aware routing: always send visitors to `/login`.
import { redirect } from '@sveltejs/kit';

export function load() {
	redirect(307, '/login');
}
