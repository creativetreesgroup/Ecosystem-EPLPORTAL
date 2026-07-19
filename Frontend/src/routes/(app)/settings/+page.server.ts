// Bare /settings has no content of its own — same established pattern as the site root
// (Frontend/src/routes/+page.server.ts): always redirect, this time to the one nav entry that
// exists today. Update this redirect if a future sub-phase ever changes what "first" means.
import { redirect } from '@sveltejs/kit';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async () => {
	redirect(307, '/settings/branding');
};
