import { describe, it, expect } from 'vitest';
import {
	validateBrandingForm,
	validateImageFile,
	fileToDataUri,
	TITLE_MAX,
	SUBTITLE_MAX,
	SITE_NAME_MAX,
	BRAND_TAG_MAX,
	IMAGE_MAX_BYTES
} from './branding';

describe('validateBrandingForm', () => {
	const valid = { title: 'My Title', subtitle: '', siteName: '', brandTag: '' };

	it('accepts a minimal valid form (title only)', () => {
		expect(validateBrandingForm(valid)).toEqual({});
	});

	it('rejects an empty title', () => {
		expect(validateBrandingForm({ ...valid, title: '' }).title).toBeDefined();
	});

	it('rejects a whitespace-only title', () => {
		expect(validateBrandingForm({ ...valid, title: '   ' }).title).toBeDefined();
	});

	it('rejects a title over the max length', () => {
		expect(validateBrandingForm({ ...valid, title: 'a'.repeat(TITLE_MAX + 1) }).title).toBeDefined();
	});

	it('accepts a title at exactly the max length', () => {
		expect(validateBrandingForm({ ...valid, title: 'a'.repeat(TITLE_MAX) }).title).toBeUndefined();
	});

	it('allows a blank site_name (backend falls back to its own default at save time, not a client error)', () => {
		expect(validateBrandingForm({ ...valid, siteName: '' }).siteName).toBeUndefined();
	});

	it('rejects subtitle/site_name/brand_tag over their max lengths', () => {
		expect(validateBrandingForm({ ...valid, subtitle: 'a'.repeat(SUBTITLE_MAX + 1) }).subtitle).toBeDefined();
		expect(validateBrandingForm({ ...valid, siteName: 'a'.repeat(SITE_NAME_MAX + 1) }).siteName).toBeDefined();
		expect(validateBrandingForm({ ...valid, brandTag: 'a'.repeat(BRAND_TAG_MAX + 1) }).brandTag).toBeDefined();
	});
});

describe('validateImageFile', () => {
	function makeFile(type: string, size: number): File {
		return new File([new Uint8Array(size)], 'test-file', { type });
	}

	it('accepts a valid PNG under the size cap', () => {
		expect(validateImageFile(makeFile('image/png', 1024))).toBeNull();
	});

	it('accepts JPEG and WEBP', () => {
		expect(validateImageFile(makeFile('image/jpeg', 1024))).toBeNull();
		expect(validateImageFile(makeFile('image/webp', 1024))).toBeNull();
	});

	it('rejects an SVG (matches backend: SVG/ICO can carry executable script)', () => {
		expect(validateImageFile(makeFile('image/svg+xml', 1024))).not.toBeNull();
	});

	it('rejects a file over IMAGE_MAX_BYTES', () => {
		expect(validateImageFile(makeFile('image/png', IMAGE_MAX_BYTES + 1))).not.toBeNull();
	});

	it('accepts a file at exactly IMAGE_MAX_BYTES', () => {
		expect(validateImageFile(makeFile('image/png', IMAGE_MAX_BYTES))).toBeNull();
	});
});

describe('fileToDataUri', () => {
	it('encodes a file into a correctly-prefixed base64 data URI, round-tripping the exact bytes', async () => {
		const bytes = new Uint8Array([137, 80, 78, 71]); // PNG magic bytes
		const file = new File([bytes], 'test.png', { type: 'image/png' });
		const uri = await fileToDataUri(file);
		expect(uri.startsWith('data:image/png;base64,')).toBe(true);
		const base64 = uri.slice('data:image/png;base64,'.length);
		expect(Buffer.from(base64, 'base64').equals(Buffer.from(bytes))).toBe(true);
	});
});
