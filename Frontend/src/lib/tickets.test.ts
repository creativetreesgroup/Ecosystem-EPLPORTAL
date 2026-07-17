// Frontend/src/lib/tickets.test.ts
import { describe, it, expect } from 'vitest';
import {
	filtersToQueryString,
	markRowAccepting,
	revertRowAccepting,
	applyRowAccepted,
	type TicketDetailRow
} from './tickets';

function row(overrides: Partial<TicketDetailRow> = {}): TicketDetailRow {
	return {
		id: 'row-uuid-1',
		spxId: 'SPX1',
		status: 'pending',
		failureReason: null,
		route: ['Jakarta', 'Bandung'],
		serviceType: 'Reguler',
		weight: 12.5,
		codAmount: 0,
		autoAccepted: false,
		createdAt: '2026-07-18T00:00:00Z',
		accepting: false,
		...overrides
	};
}

describe('filtersToQueryString', () => {
	it('omits empty/undefined filters entirely', () => {
		const qs = filtersToQueryString({ status: null, spxId: '', from: null, to: null }, 1, 50);
		expect(qs).toBe('limit=50&offset=0');
	});

	it('includes only the filters that are set, plus computed offset from page', () => {
		const qs = filtersToQueryString({ status: 'failed', spxId: 'SPX', from: null, to: null }, 3, 50);
		expect(qs).toContain('status=failed');
		expect(qs).toContain('spx_id=SPX');
		expect(qs).toContain('limit=50');
		expect(qs).toContain('offset=100');
	});

	it('includes from/to as ISO strings when set', () => {
		const qs = filtersToQueryString(
			{ status: null, spxId: '', from: '2026-07-01T00:00:00Z', to: '2026-07-18T00:00:00Z' },
			1,
			50
		);
		expect(qs).toContain('from=2026-07-01T00%3A00%3A00Z');
		expect(qs).toContain('to=2026-07-18T00%3A00%3A00Z');
	});
});

describe('markRowAccepting / revertRowAccepting / applyRowAccepted', () => {
	it('markRowAccepting sets accepting=true only on the matching row, returns a new array', () => {
		const rows = [row({ id: 'a' }), row({ id: 'b' })];
		const result = markRowAccepting(rows, 'a');
		expect(result).not.toBe(rows);
		expect(result.find((r) => r.id === 'a')?.accepting).toBe(true);
		expect(result.find((r) => r.id === 'b')?.accepting).toBe(false);
	});

	it('revertRowAccepting clears accepting on the matching row', () => {
		const rows = [row({ id: 'a', accepting: true })];
		const result = revertRowAccepting(rows, 'a');
		expect(result[0].accepting).toBe(false);
	});

	it('applyRowAccepted sets status=accepted and clears accepting on the matching row', () => {
		const rows = [row({ id: 'a', status: 'pending', accepting: true })];
		const result = applyRowAccepted(rows, 'a');
		expect(result[0].status).toBe('accepted');
		expect(result[0].accepting).toBe(false);
	});

	it('leaves non-matching rows byte-for-byte unchanged (same reference)', () => {
		const untouched = row({ id: 'b' });
		const rows = [row({ id: 'a' }), untouched];
		const result = markRowAccepting(rows, 'a');
		expect(result.find((r) => r.id === 'b')).toBe(untouched);
	});
});
