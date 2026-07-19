// Pure logic for /settings/bot — no fetch, no DOM. `isValidUrlFormat` is deliberately a syntax-
// only check (empty string allowed, both URL fields are optional) — the real security boundary
// (SSRF host-blocklist) is exclusively backend (`is_safe_outbound_url`,
// Backend/crates/api-gateway/src/routes/bot.rs) and is NOT duplicated here; see this plan's
// Global Constraints for why.

export function isValidUrlFormat(value: string): boolean {
	const trimmed = value.trim();
	if (trimmed === '') return true;
	try {
		const url = new URL(trimmed);
		return url.protocol === 'http:' || url.protocol === 'https:';
	} catch {
		return false;
	}
}

/** Mirrors the backend's own first-setup requirement (`Backend/crates/api-gateway/src/routes/
 * bot.rs::put_settings`: a blank `waha_api_key` 400s with "waha_api_key is required on first
 * setup" when no key has ever been configured) as an inline client-side check, so the user gets
 * immediate feedback instead of a round-trip. */
export function apiKeyError(hasExistingKey: boolean, enteredKey: string): string | null {
	if (!hasExistingKey && enteredKey.trim() === '') {
		return 'Wajib diisi (setup pertama)';
	}
	return null;
}
