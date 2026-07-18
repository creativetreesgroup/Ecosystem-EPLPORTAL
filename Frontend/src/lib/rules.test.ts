import { describe, it, expect } from 'vitest';
import {
	newRuleDraft,
	conditionSummary,
	ruleIsEmpty,
	setCocOnly,
	setNonCocOnly,
	isDirty,
	SERVICE_TYPE_OPTIONS,
	SHIFT_TYPE_OPTIONS,
	TRIP_TYPE_OPTIONS,
	type RuleDraft,
	type RulesPageState
} from './rules';

describe('vocabularies', () => {
	it('exposes the 8 canonical service types', () => {
		expect(SERVICE_TYPE_OPTIONS).toEqual([
			'TRONTON',
			'FUSO',
			'CDD LONG',
			'CDE LONG',
			'BLINDVAN',
			'WINGBOX',
			'ENGKEL',
			'40FCL'
		]);
	});

	it('exposes shift types 1=Pagi, 2=Siang, 3=Malam', () => {
		expect(SHIFT_TYPE_OPTIONS).toEqual([
			{ value: 1, label: 'Pagi' },
			{ value: 2, label: 'Siang' },
			{ value: 3, label: 'Malam' }
		]);
	});

	it('exposes trip types 1=Berangkat, 2=Pulang', () => {
		expect(TRIP_TYPE_OPTIONS).toEqual([
			{ value: 1, label: 'Berangkat' },
			{ value: 2, label: 'Pulang' }
		]);
	});
});

describe('newRuleDraft', () => {
	it('creates an empty rule with the given mode, a fresh clientKey, and no server id', () => {
		const rule = newRuleDraft('route');
		expect(rule.mode).toBe('route');
		expect(rule.id).toBeNull();
		expect(rule.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(rule.enabled).toBe(true);
		expect(rule.priority).toBe(0);
		expect(rule.conditions.destinations).toEqual([]);
		expect(rule.conditions.maxAcceptCount).toBe(0);
		expect(rule.conditions.acceptedCount).toBe(0);
	});

	it('two calls produce different clientKeys', () => {
		expect(newRuleDraft('filter').clientKey).not.toBe(newRuleDraft('filter').clientKey);
	});
});

describe('conditionSummary', () => {
	it('booking_id mode: counts the ids', () => {
		const rule = { ...newRuleDraft('booking_id'), conditions: { ...emptyConditionsForTest(), bookingIds: ['A', 'B', 'C'] } };
		expect(conditionSummary(rule)).toBe('3 Booking ID');
	});

	it('booking_id mode: empty list', () => {
		const rule = newRuleDraft('booking_id');
		expect(conditionSummary(rule)).toBe('Belum ada Booking ID');
	});

	it('route mode: origin, destinations, and service types', () => {
		const rule = {
			...newRuleDraft('route'),
			conditions: {
				...emptyConditionsForTest(),
				origin: 'Padang DC',
				destinations: ['Cileungsi DC', 'Bandung DC'],
				serviceTypes: ['TRONTON', 'FUSO']
			}
		};
		expect(conditionSummary(rule)).toBe('Padang DC → Cileungsi DC → Bandung DC · TRONTON, FUSO');
	});

	it('route mode: missing origin/destinations shows em-dash placeholders', () => {
		const rule = newRuleDraft('route');
		expect(conditionSummary(rule)).toBe('— → —');
	});

	it('filter mode: lists active filters', () => {
		const rule = {
			...newRuleDraft('filter'),
			conditions: { ...emptyConditionsForTest(), serviceTypes: ['TRONTON'], maxWeight: 500, cocOnly: true }
		};
		expect(conditionSummary(rule)).toBe('TRONTON · maks 500kg · COC saja');
	});

	it('filter mode: no active filters', () => {
		const rule = newRuleDraft('filter');
		expect(conditionSummary(rule)).toBe('Tanpa filter');
	});
});

describe('ruleIsEmpty', () => {
	it('booking_id mode with no ids is empty (mirrors the backend warning condition)', () => {
		expect(ruleIsEmpty(newRuleDraft('booking_id'))).toBe(true);
	});

	it('route mode with no origin and no destinations is empty', () => {
		expect(ruleIsEmpty(newRuleDraft('route'))).toBe(true);
	});

	it('route mode with only an origin is not empty', () => {
		const rule = { ...newRuleDraft('route'), conditions: { ...emptyConditionsForTest(), origin: 'Padang DC' } };
		expect(ruleIsEmpty(rule)).toBe(false);
	});

	it('filter mode is never considered empty by this check (no origin/id concept applies)', () => {
		expect(ruleIsEmpty(newRuleDraft('filter'))).toBe(false);
	});
});

describe('setCocOnly / setNonCocOnly mutual exclusion', () => {
	it('setCocOnly(true) turns nonCocOnly off', () => {
		const c = { ...emptyConditionsForTest(), nonCocOnly: true };
		const result = setCocOnly(c, true);
		expect(result.cocOnly).toBe(true);
		expect(result.nonCocOnly).toBe(false);
	});

	it('setNonCocOnly(true) turns cocOnly off', () => {
		const c = { ...emptyConditionsForTest(), cocOnly: true };
		const result = setNonCocOnly(c, true);
		expect(result.nonCocOnly).toBe(true);
		expect(result.cocOnly).toBe(false);
	});

	it('setCocOnly(false) does not touch nonCocOnly', () => {
		const c = { ...emptyConditionsForTest(), nonCocOnly: true, cocOnly: true };
		const result = setCocOnly(c, false);
		expect(result.cocOnly).toBe(false);
		expect(result.nonCocOnly).toBe(true);
	});

	it('returns a new object, does not mutate the input', () => {
		const c = emptyConditionsForTest();
		const result = setCocOnly(c, true);
		expect(result).not.toBe(c);
		expect(c.cocOnly).toBe(false);
	});
});

describe('isDirty', () => {
	function state(overrides: Partial<RulesPageState> = {}): RulesPageState {
		return { autoAcceptEnabled: false, rules: [], ...overrides };
	}

	it('false when current equals lastSaved (ignoring clientKey)', () => {
		const rule = newRuleDraft('filter');
		const saved = { ...rule, clientKey: 'different-key-but-same-content' };
		expect(isDirty(state({ rules: [rule] }), state({ rules: [saved] }))).toBe(false);
	});

	it('true when autoAcceptEnabled differs', () => {
		expect(isDirty(state({ autoAcceptEnabled: true }), state({ autoAcceptEnabled: false }))).toBe(true);
	});

	it('true when a rule field differs', () => {
		const a = newRuleDraft('filter');
		const b = { ...a, priority: 5 };
		expect(isDirty(state({ rules: [a] }), state({ rules: [b] }))).toBe(true);
	});

	it('true when rule count differs', () => {
		const a = newRuleDraft('filter');
		expect(isDirty(state({ rules: [a, newRuleDraft('route')] }), state({ rules: [a] }))).toBe(true);
	});

	it('false for two independently-created empty states', () => {
		expect(isDirty(state(), state())).toBe(false);
	});
});

// Local test helper — mirrors rules.ts's private emptyConditions() so fixtures above can spread
// a full, valid RuleConditions without repeating every field at every call site.
function emptyConditionsForTest() {
	return {
		serviceTypes: [],
		maxWeight: null,
		cocOnly: false,
		nonCocOnly: false,
		maxCodAmount: null,
		bookingIds: [],
		origin: '',
		destinations: [],
		bookingType: 'all' as const,
		shiftTypes: [],
		tripTypes: [],
		matchMode: 'strict' as const,
		minDeadlineMin: null,
		maxAcceptCount: 0,
		acceptedCount: 0
	};
}
