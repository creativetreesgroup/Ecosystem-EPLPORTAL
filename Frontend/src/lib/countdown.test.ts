import { describe, it, expect } from 'vitest';
import { formatCountdown } from './countdown';

describe('formatCountdown', () => {
	it('formats hours/minutes when more than an hour remains', () => {
		const now = Date.parse('2026-07-18T10:00:00Z');
		const target = '2026-07-18T13:34:00Z';
		expect(formatCountdown(target, now)).toEqual({ label: '3h 34m', expired: false });
	});

	it('formats minutes/seconds when under an hour remains', () => {
		const now = Date.parse('2026-07-18T10:00:00Z');
		const target = '2026-07-18T10:01:22Z';
		expect(formatCountdown(target, now)).toEqual({ label: '01:22', expired: false });
	});

	it('marks expired when the target is in the past', () => {
		const now = Date.parse('2026-07-18T10:00:00Z');
		const target = '2026-07-18T09:00:00Z';
		expect(formatCountdown(target, now)).toEqual({ label: '00:00', expired: true });
	});
});
