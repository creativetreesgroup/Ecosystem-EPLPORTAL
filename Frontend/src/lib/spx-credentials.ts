// Frontend/src/lib/spx-credentials.ts
// Pure validation/logic for the /settings/spx-credentials page. No fetch, no DOM.
// The backend validates essentially NOTHING on these fields (label is an
// unvalidated URL path segment; username/password rejected only if empty), so
// the client owns all of it. `duplicateUsernameLabel` guards the poller-boot
// collision: reactor-core keys accounts by username.trim().toLowerCase(), so
// two labels with the same normalized username silently drop one at boot.
// See Docs/superpowers/specs/2026-07-21-fase-7k-settings-spx-credentials-design.md.
import type { SpxCredential } from './api-spx-credentials';

export function validateLabel(label: string): string | null {
	const trimmed = label.trim();
	if (trimmed === '') return 'Label wajib diisi';
	if (label.includes('/')) return 'Label tidak boleh mengandung "/"';
	if (trimmed.length > 64) return 'Label maksimal 64 karakter';
	return null;
}

export function validateUsername(username: string): string | null {
	if (username.trim() === '') return 'Username wajib diisi';
	return null;
}

export function validatePassword(password: string): string | null {
	if (password === '') return 'Password wajib diisi';
	return null;
}

export function duplicateUsernameLabel(
	username: string,
	existing: SpxCredential[],
	currentLabel: string
): string | null {
	const norm = username.trim().toLowerCase();
	if (norm === '') return null;
	const clash = existing.find(
		(c) => c.label !== currentLabel && c.username.trim().toLowerCase() === norm
	);
	return clash ? clash.label : null;
}
