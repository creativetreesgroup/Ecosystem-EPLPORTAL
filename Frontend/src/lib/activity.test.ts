import { describe, it, expect } from 'vitest';
import { outcomeLabel, logTypeLabel, kindLabel, formatTimestamp, formatMicroseconds, formatMilliseconds } from './activity';

describe('outcomeLabel', () => {
	it('maps all 6 known outcome values to Indonesian labels', () => {
		expect(outcomeLabel('accepted')).toBe('Diterima');
		expect(outcomeLabel('rejected')).toBe('Ditolak');
		expect(outcomeLabel('skipped')).toBe('Dilewati');
		expect(outcomeLabel('taken_by_agency')).toBe('Diambil Agensi Lain');
		expect(outcomeLabel('failed')).toBe('Gagal');
		expect(outcomeLabel('agency_dup_unverified')).toBe('Duplikat Agensi (Belum Terverifikasi)');
	});

	it('falls back to the raw value for an unknown outcome (defensive, should never happen given the DB CHECK)', () => {
		expect(outcomeLabel('something_new')).toBe('something_new');
	});
});

describe('logTypeLabel', () => {
	it('maps success and error', () => {
		expect(logTypeLabel('success')).toBe('Berhasil');
		expect(logTypeLabel('error')).toBe('Gagal');
	});
});

describe('kindLabel', () => {
	it('maps all 3 known kinds and null', () => {
		expect(kindLabel('accept')).toBe('Terima Otomatis');
		expect(kindLabel('agency_loss')).toBe('Kalah dari Agensi');
		expect(kindLabel('otp')).toBe('OTP');
		expect(kindLabel(null)).toBe('Lainnya');
	});
});

describe('formatTimestamp', () => {
	it('formats a Date into a readable Indonesian-locale timestamp', () => {
		const d = new Date('2026-07-19T08:30:00Z');
		const result = formatTimestamp(d);
		expect(typeof result).toBe('string');
		expect(result.length).toBeGreaterThan(0);
		// Exact format is locale-dependent (Intl.DateTimeFormat); just confirm it round-trips a
		// real date's year, not a garbage/NaN string.
		expect(result).toContain('2026');
	});
});

describe('formatMicroseconds', () => {
	it('formats a positive value with a µs suffix', () => {
		expect(formatMicroseconds(342)).toBe('342 µs');
	});

	it('formats null as an em-dash', () => {
		expect(formatMicroseconds(null)).toBe('—');
	});
});

describe('formatMilliseconds', () => {
	it('formats a positive value with an ms suffix', () => {
		expect(formatMilliseconds(150)).toBe('150 ms');
	});

	it('formats null as an em-dash', () => {
		expect(formatMilliseconds(null)).toBe('—');
	});
});
