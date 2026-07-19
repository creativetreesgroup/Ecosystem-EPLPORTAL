import { describe, it, expect } from 'vitest';
import { validatePassword, isSelf } from './sub-users';

describe('validatePassword', () => {
	it('rejects a password under 8 characters', () => {
		expect(validatePassword('1234567')).not.toBeNull();
	});

	it('accepts a password of exactly 8 characters', () => {
		expect(validatePassword('12345678')).toBeNull();
	});

	it('accepts a longer password', () => {
		expect(validatePassword('a-much-longer-password')).toBeNull();
	});

	it('rejects an empty password', () => {
		expect(validatePassword('')).not.toBeNull();
	});
});

describe('isSelf', () => {
	it('returns true for an exact username match', () => {
		expect(isSelf('e2e-test-user', 'e2e-test-user')).toBe(true);
	});

	it('returns false for a different username', () => {
		expect(isSelf('e2e-readonly-user', 'e2e-test-user')).toBe(false);
	});

	it('is case-sensitive, matching the backend\'s exact-match comparison (no case-folding in portal_users.rs)', () => {
		expect(isSelf('E2E-Test-User', 'e2e-test-user')).toBe(false);
	});
});
