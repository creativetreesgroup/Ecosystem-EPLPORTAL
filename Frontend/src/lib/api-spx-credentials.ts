// Frontend/src/lib/api-spx-credentials.ts
// Typed REST layer for the tenant's stored SPX agency credentials.
// Wire shape verified against Backend/crates/api-gateway/src/routes/spx_credentials.rs
// (CredentialSummary { label, username }, UpsertCredential { username, password }) and
// routes/spx_login.rs (SpxLoginResult { ok, tier }) — snake_case throughout, no rename_all;
// label/username/ok/tier are identical on the wire and in TS (no case mapping needed).
// The stored password is NEVER returned by the backend in any form — the existence of a
// row IS the "password is set" signal. apiPost hardcodes POST and takes no AbortSignal, so
// PUT/DELETE and the abortable test-login all use raw fetch (same convention as the sibling
// api-*.ts modules). ApiError carries a fixed generic message; pages branch on .status.
import { ApiError } from './api';

export type SpxCredential = { label: string; username: string };
export type SpxLoginResult = { ok: boolean; tier: 'api' | 'form' | null };

export async function fetchSpxCredentials(): Promise<SpxCredential[]> {
	const res = await fetch('/auth/spx-credentials', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch spx credentials');
	return res.json();
}

export async function saveSpxCredential(
	label: string,
	username: string,
	password: string
): Promise<SpxCredential> {
	const res = await fetch(`/auth/spx-credentials/${encodeURIComponent(label)}`, {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ username, password })
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save spx credential');
	return res.json();
}

export async function deleteSpxCredential(label: string): Promise<void> {
	const res = await fetch(`/auth/spx-credentials/${encodeURIComponent(label)}`, {
		method: 'DELETE',
		credentials: 'include'
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to delete spx credential');
	// 204 No Content — deliberately never call res.json().
}

export async function testSpxLogin(label: string, signal?: AbortSignal): Promise<SpxLoginResult> {
	const res = await fetch(`/auth/spx-login/${encodeURIComponent(label)}`, {
		method: 'POST',
		credentials: 'include',
		signal
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to test spx login');
	return res.json();
}
