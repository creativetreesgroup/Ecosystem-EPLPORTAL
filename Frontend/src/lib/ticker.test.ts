// Frontend/src/lib/ticker.test.ts
import { describe, it, expect } from 'vitest';
import {
	mergeNewTickets,
	applyAccepted,
	applyRejected,
	applyRemoved,
	markAccepting,
	revertAccepting,
	type TicketRow
} from './ticker';

function row(overrides: Partial<TicketRow> = {}): TicketRow {
	return {
		id: 'row-uuid-1',
		spxId: 'SPX1',
		status: 'pending',
		route: ['Jakarta', 'Bandung'],
		latencyMs: null,
		localDispatchUs: null,
		accepting: false,
		...overrides
	};
}

describe('mergeNewTickets', () => {
	it('prepends genuinely new rows (newest-first) and skips ones already present by spxId', () => {
		const existing = [row({ spxId: 'SPX1' })];
		const incoming = [row({ spxId: 'SPX2' }), row({ spxId: 'SPX1' })];
		const result = mergeNewTickets(existing, incoming);
		expect(result.map((r) => r.spxId)).toEqual(['SPX2', 'SPX1']);
	});

	it('does not mutate the input array', () => {
		const existing = [row({ spxId: 'SPX1' })];
		mergeNewTickets(existing, [row({ spxId: 'SPX2' })]);
		expect(existing).toHaveLength(1);
	});
});

describe('applyAccepted', () => {
	it('updates the matching row to accepted with latency + dispatch metrics', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2' })];
		const result = applyAccepted(rows, {
			bookingId: 'SPX1',
			latencyMs: 312,
			autoAccept: true,
			rule: 'Rule A',
			route: ['Jakarta', 'Bandung'],
			localDispatchUs: 850
		});
		const updated = result.find((r) => r.spxId === 'SPX1');
		expect(updated?.status).toBe('accepted');
		expect(updated?.latencyMs).toBe(312);
		expect(updated?.localDispatchUs).toBe(850);
		expect(updated?.accepting).toBe(false);
	});

	it('leaves other rows untouched', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2', status: 'pending' })];
		const result = applyAccepted(rows, {
			bookingId: 'SPX1',
			latencyMs: 312,
			autoAccept: true,
			rule: 'Rule A',
			route: [],
			localDispatchUs: 850
		});
		expect(result.find((r) => r.spxId === 'SPX2')?.status).toBe('pending');
	});

	it('accepts latencyMs/localDispatchUs of null (manual accept — no measurement available) without coercing to 0', () => {
		const rows = [row({ spxId: 'SPX1' })];
		const result = applyAccepted(rows, { bookingId: 'SPX1', latencyMs: null, localDispatchUs: null });
		const updated = result.find((r) => r.spxId === 'SPX1');
		expect(updated?.status).toBe('accepted');
		expect(updated?.latencyMs).toBeNull();
		expect(updated?.localDispatchUs).toBeNull();
		expect(updated?.accepting).toBe(false);
	});
});

describe('applyRejected', () => {
	it('marks the matching row as taken_by_agency', () => {
		const rows = [row({ spxId: 'SPX1' })];
		const result = applyRejected(rows, 'SPX1');
		expect(result[0].status).toBe('taken_by_agency');
	});
});

describe('applyRemoved', () => {
	it('removes rows whose id is in the removed list', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2' })];
		const result = applyRemoved(rows, ['SPX1']);
		expect(result.map((r) => r.spxId)).toEqual(['SPX2']);
	});
});

describe('markAccepting', () => {
	it('sets accepting true on the matching row only', () => {
		const rows = [row({ spxId: 'SPX1' }), row({ spxId: 'SPX2' })];
		const result = markAccepting(rows, 'SPX1');
		expect(result.find((r) => r.spxId === 'SPX1')?.accepting).toBe(true);
		expect(result.find((r) => r.spxId === 'SPX2')?.accepting).toBe(false);
	});
});

describe('revertAccepting', () => {
	it('sets accepting false on the matching row', () => {
		const rows = [row({ spxId: 'SPX1', accepting: true })];
		const result = revertAccepting(rows, 'SPX1');
		expect(result[0].accepting).toBe(false);
	});
});
