// Frontend/src/lib/tickets.test.ts
import { describe, it, expect } from 'vitest';
import {
	filtersToQueryString,
	markRowAccepting,
	revertRowAccepting,
	applyRowAccepted,
	mergeAndSlicePage,
	EMPTY_TICKET_FILTERS,
	type TicketDetailRow,
	type TicketFilters
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
		requestId: null,
		onsiteId: null,
		bookingNumber: 'SPX1',
		vehicleType: null,
		deadlineAt: null,
		pickupTime: null,
		tripType: null,
		bookingType: 'reguler',
		...overrides
	};
}

describe('filtersToQueryString', () => {
	it('omits empty/undefined filters entirely', () => {
		const qs = filtersToQueryString(EMPTY_TICKET_FILTERS, 1, 50);
		expect(qs).toBe('limit=50&offset=0');
	});

	it('includes only the filters that are set, plus computed offset from page', () => {
		const qs = filtersToQueryString({ ...EMPTY_TICKET_FILTERS, status: 'failed', spxId: 'SPX' }, 3, 50);
		expect(qs).toContain('status=failed');
		expect(qs).toContain('spx_id=SPX');
		expect(qs).toContain('limit=50');
		expect(qs).toContain('offset=100');
	});

	it('includes from/to as ISO strings when set', () => {
		const qs = filtersToQueryString(
			{ ...EMPTY_TICKET_FILTERS, from: '2026-07-01T00:00:00Z', to: '2026-07-18T00:00:00Z' },
			1,
			50
		);
		expect(qs).toContain('from=2026-07-01T00%3A00%3A00Z');
		expect(qs).toContain('to=2026-07-18T00%3A00%3A00Z');
	});
});

describe('filtersToQueryString — expanded filters', () => {
	it('includes every new filter field only when set', () => {
		const filters: TicketFilters = {
			status: null,
			spxId: '',
			from: null,
			to: null,
			requestId: 'R1',
			bookingName: '',
			vehicleType: 'TRONTON',
			tripType: 1,
			bookingType: 'coc',
			originStation: null,
			destStation: null,
			weightMin: 10,
			weightMax: null,
			cod: true,
			pickupFrom: null,
			pickupTo: null,
			deadlineFrom: null,
			deadlineTo: null,
			sort: 'deadline_soonest',
			autoAccepted: true,
			acceptReason: null
		};
		const qs = filtersToQueryString(filters, 1, 50);
		const params = new URLSearchParams(qs);
		expect(params.get('request_id')).toBe('R1');
		expect(params.get('booking_name')).toBeNull();
		expect(params.get('vehicle_type')).toBe('TRONTON');
		expect(params.get('trip_type')).toBe('1');
		expect(params.get('booking_type')).toBe('coc');
		expect(params.get('weight_min')).toBe('10');
		expect(params.get('weight_max')).toBeNull();
		expect(params.get('cod')).toBe('true');
		expect(params.get('sort')).toBe('deadline_soonest');
		expect(params.get('auto_accepted')).toBe('true');
	});
});

describe('mergeAndSlicePage', () => {
	// Two sources, each already sorted desc by created_at on its own (matching what /bookings/live
	// and /bookings/history each return), interleaved so neither source's own page-2 window would
	// contain the true globally-sorted page 2 — this is exactly the bug the fix addresses.
	const live = ['10', '08', '06', '04', '02'].map((d) => ({ created_at: `2026-07-${d}` }));
	const history = ['09', '07', '05', '03', '01'].map((d) => ({ created_at: `2026-07-${d}` }));
	// Globally merged+sorted desc: 10,09,08,07,06,05,04,03,02,01

	it('returns the correct globally-sorted window for page 2, not either source own page 2', () => {
		const { rows, hasMore } = mergeAndSlicePage(live, history, 2, 3);
		expect(rows.map((r) => r.created_at)).toEqual(['2026-07-07', '2026-07-06', '2026-07-05']);
		expect(hasMore).toBe(true);
	});

	it('reports hasMore=false once the last page is reached', () => {
		const { rows, hasMore } = mergeAndSlicePage(live, history, 4, 3);
		expect(rows.map((r) => r.created_at)).toEqual(['2026-07-01']);
		expect(hasMore).toBe(false);
	});

	it('page 1 matches the naive concatenation (no regression for the common case)', () => {
		const { rows, hasMore } = mergeAndSlicePage(live, history, 1, 3);
		expect(rows.map((r) => r.created_at)).toEqual(['2026-07-10', '2026-07-09', '2026-07-08']);
		expect(hasMore).toBe(true);
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
