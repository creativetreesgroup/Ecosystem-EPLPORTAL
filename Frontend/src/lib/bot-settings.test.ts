import { describe, it, expect } from 'vitest';
import { isValidUrlFormat, apiKeyError } from './bot-settings';

describe('isValidUrlFormat', () => {
	it('accepts a well-formed https URL', () => {
		expect(isValidUrlFormat('https://waha.example.com:3000')).toBe(true);
	});

	it('accepts a well-formed http URL', () => {
		expect(isValidUrlFormat('http://127.0.0.1:19999')).toBe(true);
	});

	it('treats an empty string as valid (both fields are optional)', () => {
		expect(isValidUrlFormat('')).toBe(true);
	});

	it('treats a whitespace-only string as valid', () => {
		expect(isValidUrlFormat('   ')).toBe(true);
	});

	it('rejects a malformed string', () => {
		expect(isValidUrlFormat('not a url')).toBe(false);
	});

	it('rejects a non-http(s) scheme', () => {
		expect(isValidUrlFormat('ftp://example.com')).toBe(false);
	});
});

describe('apiKeyError', () => {
	it('returns an error when no key exists yet and the input is blank', () => {
		expect(apiKeyError(false, '')).not.toBeNull();
	});

	it('returns an error when no key exists yet and the input is whitespace-only', () => {
		expect(apiKeyError(false, '   ')).not.toBeNull();
	});

	it('returns null when no key exists yet but a real value is entered', () => {
		expect(apiKeyError(false, 'a-real-key')).toBeNull();
	});

	it('returns null when a key already exists and the input is blank (keep existing)', () => {
		expect(apiKeyError(true, '')).toBeNull();
	});

	it('returns null when a key already exists and a new value is entered (rotation)', () => {
		expect(apiKeyError(true, 'new-key')).toBeNull();
	});
});
