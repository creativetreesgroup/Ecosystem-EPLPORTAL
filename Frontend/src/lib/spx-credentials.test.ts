import { describe, it, expect } from 'vitest';
import {
	validateLabel,
	validateUsername,
	validatePassword,
	duplicateUsernameLabel
} from './spx-credentials';

describe('validateLabel', () => {
	it('rejects empty / whitespace-only', () => {
		expect(validateLabel('')).toBe('Label wajib diisi');
		expect(validateLabel('   ')).toBe('Label wajib diisi');
	});
	it('rejects a label containing a slash (would split the URL path → 404)', () => {
		expect(validateLabel('a/b')).toBe('Label tidak boleh mengandung "/"');
	});
	it('rejects a label longer than 64 chars', () => {
		expect(validateLabel('x'.repeat(65))).toBe('Label maksimal 64 karakter');
	});
	it('accepts a normal label', () => {
		expect(validateLabel('agency1')).toBeNull();
	});
});

describe('validateUsername', () => {
	it('rejects empty / whitespace-only', () => {
		expect(validateUsername('  ')).toBe('Username wajib diisi');
	});
	it('accepts non-empty', () => {
		expect(validateUsername('agency1-user')).toBeNull();
	});
});

describe('validatePassword', () => {
	it('rejects only the empty string (no length floor for SPX credentials)', () => {
		expect(validatePassword('')).toBe('Password wajib diisi');
		expect(validatePassword(' ')).toBeNull();
	});
});

describe('duplicateUsernameLabel', () => {
	const existing = [
		{ label: 'agency1', username: 'Shared-User' },
		{ label: 'agency2', username: 'other' }
	];
	it('flags a case/whitespace-insensitive username clash on a DIFFERENT label', () => {
		expect(duplicateUsernameLabel('  shared-user ', existing, 'agency3')).toBe('agency1');
	});
	it('excludes the same label (overwrite / password rotation is allowed)', () => {
		expect(duplicateUsernameLabel('shared-user', existing, 'agency1')).toBeNull();
	});
	it('returns null when there is no clash', () => {
		expect(duplicateUsernameLabel('brand-new', existing, 'agency3')).toBeNull();
	});
	it('returns null for an empty username', () => {
		expect(duplicateUsernameLabel('   ', existing, 'agency3')).toBeNull();
	});
});
