// Pure logic for /settings/branding — no fetch, no DOM. Every limit below MUST match
// Backend/crates/api-gateway/src/branding.rs exactly (re-verified against that file while
// writing this plan). Wire-format mapping lives in api-branding.ts, matching the established
// $lib "logic vs. REST layer" split from prior phases.

export const TITLE_MAX = 60;
export const SUBTITLE_MAX = 160;
export const SITE_NAME_MAX = 60;
export const BRAND_TAG_MAX = 20;
export const IMAGE_MAX_BYTES = 5 * 1024 * 1024;
export const ALLOWED_IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/webp'];

export type BrandingFormErrors = {
	title?: string;
	subtitle?: string;
	siteName?: string;
	brandTag?: string;
};

export function validateBrandingForm(form: {
	title: string;
	subtitle: string;
	siteName: string;
	brandTag: string;
}): BrandingFormErrors {
	const errors: BrandingFormErrors = {};

	const title = form.title.trim();
	if (!title) {
		errors.title = 'Judul wajib diisi';
	} else if (title.length > TITLE_MAX) {
		errors.title = `Judul maksimal ${TITLE_MAX} karakter`;
	}

	if (form.subtitle.trim().length > SUBTITLE_MAX) {
		errors.subtitle = `Subjudul maksimal ${SUBTITLE_MAX} karakter`;
	}

	// Blank site_name is allowed here — the backend falls back to its own default at save time
	// (Backend/crates/api-gateway/src/branding.rs::validate_and_normalize), it is not an error.
	if (form.siteName.trim().length > SITE_NAME_MAX) {
		errors.siteName = `Nama situs maksimal ${SITE_NAME_MAX} karakter`;
	}

	if (form.brandTag.trim().length > BRAND_TAG_MAX) {
		errors.brandTag = `Brand tag maksimal ${BRAND_TAG_MAX} karakter`;
	}

	return errors;
}

/** Returns an error message, or `null` if the file passes. Checked entirely from `File.type`/
 * `File.size` — no image is ever read or sent until this returns `null`. */
export function validateImageFile(file: File): string | null {
	if (!ALLOWED_IMAGE_TYPES.includes(file.type)) {
		return 'Format harus PNG, JPEG, atau WEBP';
	}
	if (file.size === 0) {
		return 'File tidak boleh kosong';
	}
	if (file.size > IMAGE_MAX_BYTES) {
		return 'Ukuran gambar maksimal 5MB';
	}
	return null;
}

/** `File.arrayBuffer()` + `btoa`, NOT `FileReader` — both are available in real browsers AND in
 * Node's test environment (no jsdom/happy-dom needed), see this plan's Global Constraints. */
export async function fileToDataUri(file: File): Promise<string> {
	const buffer = await file.arrayBuffer();
	const bytes = new Uint8Array(buffer);
	let binary = '';
	for (const byte of bytes) {
		binary += String.fromCharCode(byte);
	}
	const base64 = btoa(binary);
	return `data:${file.type};base64,${base64}`;
}
