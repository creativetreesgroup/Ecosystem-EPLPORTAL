import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchBranding, saveBranding } from './api-branding';

afterEach(() => {
	vi.unstubAllGlobals();
});

function brandingWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		title: 'My Title',
		subtitle: 'My Subtitle',
		site_name: 'My Site',
		brand_tag: 'TAG',
		logo_data_uri: null,
		favicon_data_uri: null,
		...overrides
	};
}

describe('fetchBranding', () => {
	it('issues a GET to /branding and maps every snake_case field to camelCase', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify(brandingWire()), { status: 200 });
			})
		);
		const branding = await fetchBranding();
		expect(calledUrl).toBe('/branding');
		expect(branding).toEqual({
			title: 'My Title',
			subtitle: 'My Subtitle',
			siteName: 'My Site',
			brandTag: 'TAG',
			logoDataUri: null,
			faviconDataUri: null
		});
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 500 })));
		await expect(fetchBranding()).rejects.toThrow();
	});
});

describe('saveBranding', () => {
	it('issues a PUT (not POST) with a snake_case body matching BrandingInput exactly', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify(brandingWire()), { status: 200 });
			})
		);
		await saveBranding({
			title: 'My Title',
			subtitle: 'My Subtitle',
			siteName: 'My Site',
			brandTag: 'TAG',
			logoDataUri: null,
			faviconDataUri: null
		});
		expect(calledUrl).toBe('/branding');
		expect(calledInit?.method).toBe('PUT');
		expect(JSON.parse(calledInit?.body as string)).toEqual(brandingWire());
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 403 })));
		await expect(
			saveBranding({ title: 'x', subtitle: '', siteName: '', brandTag: '', logoDataUri: null, faviconDataUri: null })
		).rejects.toThrow();
	});
});
