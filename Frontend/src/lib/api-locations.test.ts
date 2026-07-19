// Frontend/src/lib/api-locations.test.ts
// vi.stubGlobal('fetch', ...) regression guards for this module's load-bearing HTTP details:
// POST (via apiPost) for create, DELETE-with-id-in-path-and-no-body for delete, and status-code
// propagation (409 for duplicate, etc.) via ApiError.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchLocations, createLocation, deleteLocation } from './api-locations';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('fetchLocations', () => {
	it('issues a GET to /locations and returns the list as-is (already camelCase)', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([{ id: 'loc-1', name: 'Jakarta' }]), { status: 200 });
			})
		);
		const locations = await fetchLocations();
		expect(calledUrl).toBe('/locations');
		expect(locations).toEqual([{ id: 'loc-1', name: 'Jakarta' }]);
	});

	it('throws ApiError with the real status code on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
		await expect(fetchLocations()).rejects.toMatchObject({ status: 500 });
	});
});

describe('createLocation', () => {
	it('issues a POST to /locations with the name', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify({ id: 'loc-2', name: 'Bandung' }), { status: 200 });
			})
		);
		const created = await createLocation('Bandung');
		expect(calledUrl).toBe('/locations');
		expect(calledInit?.method).toBe('POST');
		expect(JSON.parse(calledInit?.body as string)).toEqual({ name: 'Bandung' });
		expect(created).toEqual({ id: 'loc-2', name: 'Bandung' });
	});

	it('throws ApiError with status 409 on a duplicate name', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response(JSON.stringify({ error: 'already exists' }), { status: 409 }))
		);
		await expect(createLocation('Jakarta')).rejects.toMatchObject({ status: 409 });
	});
});

describe('deleteLocation', () => {
	it('issues a DELETE to /locations/{id} and does not attempt to parse a body', async () => {
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
		await deleteLocation('loc-1');
		expect(calledUrl).toBe('/locations/loc-1');
		expect(calledInit?.method).toBe('DELETE');
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 404 })));
		await expect(deleteLocation('missing-id')).rejects.toThrow();
	});
});
