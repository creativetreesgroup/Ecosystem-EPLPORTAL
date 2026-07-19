import { ApiError } from './api';

export type Branding = {
	title: string;
	subtitle: string;
	siteName: string;
	brandTag: string;
	logoDataUri: string | null;
	faviconDataUri: string | null;
};

type BrandingWire = {
	title: string;
	subtitle: string;
	site_name: string;
	brand_tag: string;
	logo_data_uri: string | null;
	favicon_data_uri: string | null;
};

function fromWire(wire: BrandingWire): Branding {
	return {
		title: wire.title,
		subtitle: wire.subtitle,
		siteName: wire.site_name,
		brandTag: wire.brand_tag,
		logoDataUri: wire.logo_data_uri,
		faviconDataUri: wire.favicon_data_uri
	};
}

function toWire(branding: Branding): BrandingWire {
	return {
		title: branding.title,
		subtitle: branding.subtitle,
		site_name: branding.siteName,
		brand_tag: branding.brandTag,
		logo_data_uri: branding.logoDataUri,
		favicon_data_uri: branding.faviconDataUri
	};
}

export async function fetchBranding(): Promise<Branding> {
	const res = await fetch('/branding', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch branding');
	const wire: BrandingWire = await res.json();
	return fromWire(wire);
}

/** `apiPost` (Frontend/src/lib/api.ts) hardcodes `method: 'POST'` — the backend route is
 * `PUT /branding` (Backend/crates/api-gateway/src/routes/branding.rs's `branding_router`), so
 * this cannot use `apiPost`; a POST here would 405. Raw `fetch` with `method: 'PUT'`, same
 * header/credentials/error shape as `apiPost` otherwise. */
export async function saveBranding(branding: Branding): Promise<Branding> {
	const res = await fetch('/branding', {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(toWire(branding))
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save branding');
	const wire: BrandingWire = await res.json();
	return fromWire(wire);
}
