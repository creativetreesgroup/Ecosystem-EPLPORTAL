// Frontend/src/lib/api-rules.test.ts
// Tests the pure wire<->domain mapping functions (ruleOutputToDraft/draftToRuleInput) directly,
// plus mock-fetch regression tests for saveSettings/fetchSettings guarding the actual HTTP verb
// each issues (saveSettings uses a raw PUT — apiPost hardcodes POST and would 405) — same
// vi.stubGlobal('fetch', ...) pattern as Frontend/src/lib/api-tickets.test.ts, added for the same
// reason: a load-bearing fetch-call detail needs its own regression test beyond pure-function
// unit tests. fetchSettings/saveSettings are additionally exercised end-to-end against a live
// backend by Frontend/tests/rules.spec.ts (Task 8).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { ruleOutputToDraft, draftToRuleInput, fetchSettings, saveSettings } from './api-rules';
import { newRuleDraft } from './rules';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('ruleOutputToDraft', () => {
	it('maps every snake_case field to its camelCase RuleDraft equivalent', () => {
		const wire = {
			id: 'server-uuid-1',
			name: 'Padang Lane',
			enabled: true,
			priority: 5,
			mode: 'route',
			service_types: ['TRONTON'],
			max_weight: 1000,
			coc_only: false,
			non_coc_only: true,
			max_cod_amount: null,
			booking_ids: [],
			origin: 'Padang DC',
			destinations: ['Cileungsi DC'],
			booking_type: 'reguler',
			shift_types: [1, 2],
			trip_types: [1],
			match_mode: 'flexible',
			min_deadline_min: 30,
			max_accept_count: 10,
			accepted_count: 3
		};
		const draft = ruleOutputToDraft(wire);
		expect(draft.id).toBe('server-uuid-1');
		expect(draft.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(draft.name).toBe('Padang Lane');
		expect(draft.mode).toBe('route');
		expect(draft.conditions.serviceTypes).toEqual(['TRONTON']);
		expect(draft.conditions.maxWeight).toBe(1000);
		expect(draft.conditions.cocOnly).toBe(false);
		expect(draft.conditions.nonCocOnly).toBe(true);
		expect(draft.conditions.origin).toBe('Padang DC');
		expect(draft.conditions.destinations).toEqual(['Cileungsi DC']);
		expect(draft.conditions.bookingType).toBe('reguler');
		expect(draft.conditions.shiftTypes).toEqual([1, 2]);
		expect(draft.conditions.tripTypes).toEqual([1]);
		expect(draft.conditions.matchMode).toBe('flexible');
		expect(draft.conditions.minDeadlineMin).toBe(30);
		expect(draft.conditions.maxAcceptCount).toBe(10);
		expect(draft.conditions.acceptedCount).toBe(3);
	});
});

describe('draftToRuleInput', () => {
	it('maps every camelCase RuleDraft field to its snake_case wire equivalent, omitting id', () => {
		const draft = {
			...newRuleDraft('booking_id'),
			name: 'IDs',
			priority: -3,
			conditions: {
				...newRuleDraft('booking_id').conditions,
				bookingIds: ['SPX1', 'SPX2'],
				maxAcceptCount: 0,
				acceptedCount: 7
			}
		};
		const wire = draftToRuleInput(draft);
		expect(wire).not.toHaveProperty('id');
		expect(wire.name).toBe('IDs');
		expect(wire.priority).toBe(-3);
		expect(wire.mode).toBe('booking_id');
		expect(wire.booking_ids).toEqual(['SPX1', 'SPX2']);
		expect(wire.max_accept_count).toBe(0);
		expect(wire.accepted_count).toBe(7);
	});

	it('round-trips through ruleOutputToDraft . draftToRuleInput back to the same wire shape (minus id)', () => {
		const wire = {
			id: 'x',
			name: 'RT',
			enabled: false,
			priority: 0,
			mode: 'filter',
			service_types: ['FUSO', '40FCL'],
			max_weight: null,
			coc_only: true,
			non_coc_only: false,
			max_cod_amount: 50000,
			booking_ids: [],
			origin: '',
			destinations: [],
			booking_type: 'all',
			shift_types: [],
			trip_types: [2],
			match_mode: 'strict',
			min_deadline_min: null,
			max_accept_count: 0,
			accepted_count: 0
		};
		const { id: _id, ...expected } = wire;
		expect(draftToRuleInput(ruleOutputToDraft(wire))).toEqual(expected);
	});
});

describe('saveSettings', () => {
	it('issues a real PUT (not apiPost\'s POST) to /bookings/settings with the mapped request body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		const fetchMock = vi.fn((url: string, init?: RequestInit) => {
			calledUrl = url;
			calledInit = init;
			return Promise.resolve({
				ok: true,
				json: async () => ({ auto_accept_enabled: true, rules: [] })
			});
		});
		vi.stubGlobal('fetch', fetchMock);

		const draft = {
			...newRuleDraft('booking_id'),
			name: 'Test Rule',
			conditions: { ...newRuleDraft('booking_id').conditions, bookingIds: ['SPX1'] }
		};

		await saveSettings({ autoAcceptEnabled: true, rules: [draft] });

		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(calledUrl).toBe('/bookings/settings');
		expect(calledInit?.method).toBe('PUT');
		const body = JSON.parse(String(calledInit?.body));
		expect(body.auto_accept_enabled).toBe(true);
		expect(body.rules[0].name).toBe('Test Rule');
		expect(body.rules[0].booking_ids).toEqual(['SPX1']);
	});
});

describe('fetchSettings', () => {
	it('issues a GET (no body) to /bookings/settings', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		const fetchMock = vi.fn((url: string, init?: RequestInit) => {
			calledUrl = url;
			calledInit = init;
			return Promise.resolve({
				ok: true,
				json: async () => ({ auto_accept_enabled: false, rules: [] })
			});
		});
		vi.stubGlobal('fetch', fetchMock);

		await fetchSettings();

		expect(fetchMock).toHaveBeenCalledTimes(1);
		expect(calledUrl).toBe('/bookings/settings');
		expect(calledInit?.method ?? 'GET').toBe('GET');
		expect(calledInit?.body).toBeUndefined();
	});
});
