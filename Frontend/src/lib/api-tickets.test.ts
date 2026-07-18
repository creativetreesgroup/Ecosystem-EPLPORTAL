// Frontend/src/lib/api-tickets.test.ts
// Exercises fetchTickets itself (unlike tickets.test.ts's pure-function tests) by mocking global
// fetch — a real regression test for the offset/limit bug: fetchTickets's single-source branch
// must request offset computed from the REAL page size (50), not the inflated overfetch limit
// (51) used for `limit`. A re-derivation of filtersToQueryString in the test body would keep
// passing even if api-tickets.ts's fix were reverted; this test calls the actual fetched code.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchTickets } from './api-tickets';
import { EMPTY_TICKET_FILTERS } from './tickets';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('fetchTickets', () => {
	it('requests offset from the real page size, not the overfetch limit, for a single-source status filter', async () => {
		let calledUrl: string | undefined;
		const fetchMock = vi.fn((url: string) => {
			calledUrl = url;
			return Promise.resolve({ ok: true, json: async () => [] });
		});
		vi.stubGlobal('fetch', fetchMock);

		await fetchTickets({ ...EMPTY_TICKET_FILTERS, status: 'pending' }, 2);

		expect(fetchMock).toHaveBeenCalledTimes(1);
		const params = new URL(String(calledUrl), 'http://localhost').searchParams;
		expect(params.get('offset')).toBe('50'); // (page-1) * real PAGE_SIZE=50 — the bug produced 51
		expect(params.get('limit')).toBe('51'); // overfetch by one for hasMore detection
	});
});
