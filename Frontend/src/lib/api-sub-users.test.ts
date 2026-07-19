// vi.stubGlobal('fetch', ...) regression guards for this module's load-bearing HTTP details:
// snake_case<->camelCase wire mapping, POST via apiPost, DELETE-with-id-in-path-and-no-body,
// and status-code propagation (409 for duplicate username) via ApiError.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchSubUsers, createSubUser, deleteSubUser } from './api-sub-users';

afterEach(() => {
	vi.unstubAllGlobals();
});

function portalUserWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		id: 'user-1',
		username: 'e2e-sub-user',
		display_name: 'E2E Sub User',
		is_main_account: false,
		enabled: true,
		...overrides
	};
}

describe('fetchSubUsers', () => {
	it('issues a GET to /auth/portal-users and maps every snake_case field to camelCase', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([portalUserWire()]), { status: 200 });
			})
		);
		const users = await fetchSubUsers();
		expect(calledUrl).toBe('/auth/portal-users');
		expect(users).toEqual([
			{ id: 'user-1', username: 'e2e-sub-user', displayName: 'E2E Sub User', isMainAccount: false, enabled: true }
		]);
	});

	it('throws ApiError with the real status code on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
		await expect(fetchSubUsers()).rejects.toMatchObject({ status: 500 });
	});
});

describe('createSubUser', () => {
	it('issues a POST to /auth/portal-users with a snake_case body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify(portalUserWire()), { status: 200 });
			})
		);
		const created = await createSubUser({
			username: 'e2e-sub-user',
			password: 'a-valid-password',
			displayName: 'E2E Sub User',
			isMainAccount: false
		});
		expect(calledUrl).toBe('/auth/portal-users');
		expect(calledInit?.method).toBe('POST');
		expect(JSON.parse(calledInit?.body as string)).toEqual({
			username: 'e2e-sub-user',
			password: 'a-valid-password',
			display_name: 'E2E Sub User',
			is_main_account: false
		});
		expect(created.username).toBe('e2e-sub-user');
	});

	it('throws ApiError with status 409 on a duplicate username', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'already exists' }), { status: 409 }))
		);
		await expect(
			createSubUser({ username: 'dup', password: 'a-valid-password', displayName: 'Dup', isMainAccount: false })
		).rejects.toMatchObject({ status: 409 });
	});
});

describe('deleteSubUser', () => {
	it('issues a DELETE to /auth/portal-users/{id} and does not attempt to parse a body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(null, { status: 204 });
			})
		);
		await deleteSubUser('user-1');
		expect(calledUrl).toBe('/auth/portal-users/user-1');
		expect(calledInit?.method).toBe('DELETE');
	});

	it('throws ApiError with status 400 on a self-delete rejection', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'cannot delete your own account' }), { status: 400 }))
		);
		await expect(deleteSubUser('self-id')).rejects.toMatchObject({ status: 400 });
	});
});
