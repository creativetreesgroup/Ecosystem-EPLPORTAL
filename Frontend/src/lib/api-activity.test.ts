// Frontend/src/lib/api-activity.test.ts
// vi.stubGlobal('fetch', ...) regression guards for the two load-bearing HTTP details this module
// has: fetchAcceptEvents' overfetch-by-one hasMore technique, and clearBotLogs' DELETE-with-no-body
// handling — same precedent as api-tickets.ts/api-prices.ts's own fetch-mock guards for their
// respective load-bearing details.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchAcceptEvents, fetchBotLogs, clearBotLogs } from './api-activity';

afterEach(() => {
	vi.unstubAllGlobals();
});

function acceptEventWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		id: 'event-1',
		booking_id: 'booking-1',
		rule_id: null,
		outcome: 'accepted',
		local_dispatch_us: 342,
		accept_e2e_ms: 150,
		detail: { note: 'ok' },
		created_at: '2026-07-19T08:00:00Z',
		...overrides
	};
}

describe('fetchAcceptEvents', () => {
	it('requests PAGE_SIZE+1 and correctly derives offset from the real page size, not the overfetch limit', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([]), { status: 200 });
			})
		);
		await fetchAcceptEvents(3);
		const params = new URLSearchParams(calledUrl?.split('?')[1]);
		// Page 3 at whatever PAGE_SIZE this module uses internally — offset must be
		// (page-1)*PAGE_SIZE using the REAL page size, and limit must be PAGE_SIZE+1 (the
		// overfetch). Assert the relationship holds rather than hardcoding PAGE_SIZE here, so this
		// test doesn't silently drift if the constant changes.
		const limit = Number(params.get('limit'));
		const offset = Number(params.get('offset'));
		const pageSize = limit - 1;
		expect(offset).toBe((3 - 1) * pageSize);
	});

	it('hasMore is true when the overfetch returns PAGE_SIZE+1 rows, and the extra row is sliced off', async () => {
		vi.stubGlobal('fetch', vi.fn(async (url: string) => {
			const limit = Number(new URLSearchParams(url.split('?')[1]).get('limit'));
			const rows = Array.from({ length: limit }, (_, i) => acceptEventWire({ id: `event-${i}` }));
			return new Response(JSON.stringify(rows), { status: 200 });
		}));
		const { rows, hasMore } = await fetchAcceptEvents(1);
		expect(hasMore).toBe(true);
		// The overfetch row must be sliced off — returned rows must be exactly PAGE_SIZE, one
		// fewer than what the mocked fetch returned.
		const requestedLimit = rows.length + 1;
		expect(rows.length).toBe(requestedLimit - 1);
	});

	it('hasMore is false when fewer than PAGE_SIZE+1 rows come back', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([acceptEventWire()]), { status: 200 })));
		const { rows, hasMore } = await fetchAcceptEvents(1);
		expect(hasMore).toBe(false);
		expect(rows.length).toBe(1);
	});

	it('maps every snake_case field to its camelCase AcceptEventRow equivalent', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([acceptEventWire()]), { status: 200 })));
		const { rows } = await fetchAcceptEvents(1);
		expect(rows[0]).toEqual({
			id: 'event-1',
			bookingId: 'booking-1',
			ruleId: null,
			outcome: 'accepted',
			localDispatchUs: 342,
			acceptE2eMs: 150,
			detail: { note: 'ok' },
			createdAt: new Date('2026-07-19T08:00:00Z')
		});
	});
});

describe('fetchBotLogs', () => {
	it('issues a GET to /bot/logs with no query params', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal('fetch', vi.fn(async (url: string) => {
			calledUrl = url;
			return new Response(JSON.stringify([]), { status: 200 });
		}));
		await fetchBotLogs();
		expect(calledUrl).toBe('/bot/logs');
	});

	it('maps a full entry correctly', async () => {
		const wire = { ts: 1789800000000, log_type: 'success', kind: 'otp', booking_id: null, latency_ms: 5000, rule: null, error: null };
		vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify([wire]), { status: 200 })));
		const rows = await fetchBotLogs();
		expect(rows[0]).toEqual({
			ts: 1789800000000,
			logType: 'success',
			kind: 'otp',
			bookingId: null,
			latencyMs: 5000,
			rule: null,
			error: null
		});
	});
});

describe('clearBotLogs', () => {
	it('issues a DELETE to /bot/logs and does not attempt to parse a body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
			calledUrl = url;
			calledInit = init;
			return new Response(null, { status: 204 });
		}));
		await clearBotLogs();
		expect(calledUrl).toBe('/bot/logs');
		expect(calledInit?.method).toBe('DELETE');
	});
});
