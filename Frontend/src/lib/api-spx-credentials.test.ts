import { describe, it, expect, vi, afterEach } from 'vitest';
import {
	fetchSpxCredentials,
	saveSpxCredential,
	deleteSpxCredential,
	testSpxLogin
} from './api-spx-credentials';
import { ApiError } from './api';

afterEach(() => vi.unstubAllGlobals());

function stubFetch(response: Partial<Response> & { json?: () => Promise<unknown> }) {
	const fn = vi.fn(async () => response as Response) as ReturnType<typeof vi.fn> & {
		mock: { calls: Array<[string, RequestInit | undefined]> };
	};
	vi.stubGlobal('fetch', fn);
	return fn;
}

describe('fetchSpxCredentials', () => {
	it('GETs the list and returns it as-is', async () => {
		const fn = stubFetch({ ok: true, json: async () => [{ label: 'agency1', username: 'u1' }] });
		const result = await fetchSpxCredentials();
		expect(result).toEqual([{ label: 'agency1', username: 'u1' }]);
		expect(fn).toHaveBeenCalledWith('/auth/spx-credentials', { credentials: 'include' });
	});
	it('throws ApiError with the real status on a non-ok response', async () => {
		stubFetch({ ok: false, status: 500 });
		await expect(fetchSpxCredentials()).rejects.toMatchObject({ status: 500 });
	});
});

describe('saveSpxCredential', () => {
	it('PUTs to an encoded label with a {username,password} body and returns the summary', async () => {
		const fn = stubFetch({ ok: true, json: async () => ({ label: 'a b', username: 'u1' }) });
		const result = await saveSpxCredential('a b', 'u1', 'pw');
		expect(result).toEqual({ label: 'a b', username: 'u1' });
		const [url, init] = fn.mock.calls[0];
		expect(url).toBe('/auth/spx-credentials/a%20b');
		expect(init?.method).toBe('PUT');
		expect(JSON.parse(init?.body as string)).toEqual({ username: 'u1', password: 'pw' });
	});
	it('throws ApiError with status 409 on conflict', async () => {
		stubFetch({ ok: false, status: 409 });
		await expect(saveSpxCredential('a', 'u', 'p')).rejects.toMatchObject({ status: 409 });
	});
});

describe('deleteSpxCredential', () => {
	it('DELETEs the encoded label and never parses a body', async () => {
		const json = vi.fn();
		const fn = stubFetch({ ok: true, status: 204, json });
		await deleteSpxCredential('a b');
		const [url, init] = fn.mock.calls[0];
		expect(url).toBe('/auth/spx-credentials/a%20b');
		expect(init?.method).toBe('DELETE');
		expect(json).not.toHaveBeenCalled();
	});
});

describe('testSpxLogin', () => {
	it('POSTs to the encoded label and returns {ok,tier}', async () => {
		const fn = stubFetch({ ok: true, json: async () => ({ ok: true, tier: 'api' }) });
		const result = await testSpxLogin('a b');
		expect(result).toEqual({ ok: true, tier: 'api' });
		const [url, init] = fn.mock.calls[0];
		expect(url).toBe('/auth/spx-login/a%20b');
		expect(init?.method).toBe('POST');
	});
	it('surfaces a 429 as ApiError with status 429', async () => {
		stubFetch({ ok: false, status: 429 });
		await expect(testSpxLogin('a')).rejects.toBeInstanceOf(ApiError);
		await expect(testSpxLogin('a')).rejects.toMatchObject({ status: 429 });
	});
});
