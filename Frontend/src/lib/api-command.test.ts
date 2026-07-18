import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchSummary } from './api-command';

describe('fetchSummary', () => {
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('maps the wire response to camelCase', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue({
				ok: true,
				json: async () => ({
					incoming_today: 5,
					accepted_auto_today: 2,
					accepted_manual_today: 1,
					taken_by_other_today: 0,
					latency_p99_ms: 210.5
				})
			})
		);
		const result = await fetchSummary();
		expect(result).toEqual({
			incomingToday: 5,
			acceptedAutoToday: 2,
			acceptedManualToday: 1,
			takenByOtherToday: 0,
			latencyP99Ms: 210.5
		});
	});
});
