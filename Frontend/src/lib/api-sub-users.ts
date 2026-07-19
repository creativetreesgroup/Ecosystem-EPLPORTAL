// Thin typed REST layer for /settings/sub-users. Wire shape verified directly against
// Backend/crates/api-gateway/src/routes/portal_users.rs (PortalUserSummary/CreatePortalUser) —
// snake_case throughout, no rename_all anywhere in api-gateway.
import { apiPost, ApiError } from './api';

export type PortalUser = {
	id: string;
	username: string;
	displayName: string;
	isMainAccount: boolean;
	enabled: boolean;
};

type PortalUserWire = {
	id: string;
	username: string;
	display_name: string;
	is_main_account: boolean;
	enabled: boolean;
};

function fromWire(wire: PortalUserWire): PortalUser {
	return {
		id: wire.id,
		username: wire.username,
		displayName: wire.display_name,
		isMainAccount: wire.is_main_account,
		enabled: wire.enabled
	};
}

export type CreateSubUserInput = {
	username: string;
	password: string;
	displayName: string;
	isMainAccount: boolean;
};

export async function fetchSubUsers(): Promise<PortalUser[]> {
	const res = await fetch('/auth/portal-users', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch sub-users');
	const wire: PortalUserWire[] = await res.json();
	return wire.map(fromWire);
}

export async function createSubUser(input: CreateSubUserInput): Promise<PortalUser> {
	const wire = await apiPost<PortalUserWire>('/auth/portal-users', {
		username: input.username,
		password: input.password,
		display_name: input.displayName,
		is_main_account: input.isMainAccount
	});
	return fromWire(wire);
}

/** `DELETE /auth/portal-users/{id}` returns `204 No Content` on success — never call
 * `res.json()` on this response, there is no body to parse. */
export async function deleteSubUser(id: string): Promise<void> {
	const res = await fetch(`/auth/portal-users/${id}`, { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to delete sub-user');
}
