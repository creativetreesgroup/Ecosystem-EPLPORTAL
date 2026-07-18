// Frontend/src/lib/api-rules.test.ts
// No network here — these test the pure wire<->domain mapping functions in isolation by
// exporting them for testing. fetchSettings/saveSettings themselves are exercised for real by
// Frontend/tests/rules.spec.ts (Task 8) against a live backend; a mocked-fetch unit test of a
// thin wrapper would just re-assert its own mock, providing no real coverage (same reasoning
// TicketFilterBar's sibling api-tickets.ts module was NOT given its own mock-fetch unit tests).
import { describe, it, expect } from 'vitest';
import { ruleOutputToDraft, draftToRuleInput } from './api-rules';
import { newRuleDraft } from './rules';

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
