// Pure logic for the /rules Rule Builder — no fetch, no DOM. Wire-format mapping (snake_case
// <-> these camelCase types) lives in api-rules.ts, matching the tickets.ts/api-tickets.ts split
// already established in Fase 7c.

export type RuleMode = 'booking_id' | 'route' | 'filter';
export type BookingType = 'all' | 'spxid' | 'reguler';
export type MatchMode = 'strict' | 'flexible';

export type RuleConditions = {
	serviceTypes: string[];
	maxWeight: number | null;
	cocOnly: boolean;
	nonCocOnly: boolean;
	maxCodAmount: number | null;
	bookingIds: string[];
	origin: string;
	destinations: string[];
	bookingType: BookingType;
	shiftTypes: number[];
	tripTypes: number[];
	matchMode: MatchMode;
	minDeadlineMin: number | null;
	maxAcceptCount: number;
	/** Server-maintained running counter — read-only in the UI, round-tripped on save. */
	acceptedCount: number;
};

export type RuleDraft = {
	/** Ephemeral, client-generated — for Svelte {#each} keying only. NEVER sent to the server and
	 * NEVER equal to `id` (see Global Constraints: server ids don't round-trip on save). */
	clientKey: string;
	/** The server's Uuid for this rule, or null if it has never been saved. Present only for
	 * traceability/debugging — no code path may key off this for list identity; use clientKey. */
	id: string | null;
	name: string;
	enabled: boolean;
	priority: number;
	mode: RuleMode;
	conditions: RuleConditions;
};

export type RulesPageState = {
	autoAcceptEnabled: boolean;
	rules: RuleDraft[];
};

export const SERVICE_TYPE_OPTIONS: string[] = [
	'TRONTON',
	'FUSO',
	'CDD LONG',
	'CDE LONG',
	'BLINDVAN',
	'WINGBOX',
	'ENGKEL',
	'40FCL'
];

export const SHIFT_TYPE_OPTIONS: { value: number; label: string }[] = [
	{ value: 1, label: 'Pagi' },
	{ value: 2, label: 'Siang' },
	{ value: 3, label: 'Malam' }
];

export const TRIP_TYPE_OPTIONS: { value: number; label: string }[] = [
	{ value: 1, label: 'Berangkat' },
	{ value: 2, label: 'Pulang' }
];

function emptyConditions(): RuleConditions {
	return {
		serviceTypes: [],
		maxWeight: null,
		cocOnly: false,
		nonCocOnly: false,
		maxCodAmount: null,
		bookingIds: [],
		origin: '',
		destinations: [],
		bookingType: 'all',
		shiftTypes: [],
		tripTypes: [],
		matchMode: 'strict',
		minDeadlineMin: null,
		maxAcceptCount: 0,
		acceptedCount: 0
	};
}

export function newRuleDraft(mode: RuleMode): RuleDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: null,
		name: '',
		enabled: true,
		priority: 0,
		mode,
		conditions: emptyConditions()
	};
}

export function conditionSummary(rule: RuleDraft): string {
	const c = rule.conditions;
	if (rule.mode === 'booking_id') {
		return c.bookingIds.length > 0 ? `${c.bookingIds.length} Booking ID` : 'Belum ada Booking ID';
	}
	if (rule.mode === 'route') {
		const route = [c.origin || '—', ...(c.destinations.length > 0 ? c.destinations : ['—'])].join(' → ');
		return c.serviceTypes.length > 0 ? `${route} · ${c.serviceTypes.join(', ')}` : route;
	}
	// filter mode
	const parts: string[] = [];
	if (c.serviceTypes.length > 0) parts.push(c.serviceTypes.join(', '));
	if (c.maxWeight !== null) parts.push(`maks ${c.maxWeight}kg`);
	if (c.maxCodAmount !== null) parts.push(`maks COD Rp${c.maxCodAmount}`);
	if (c.cocOnly) parts.push('COC saja');
	if (c.nonCocOnly) parts.push('Non-COC saja');
	return parts.length > 0 ? parts.join(' · ') : 'Tanpa filter';
}

/** Mirrors core_domain::sanitize_accept_rules's two "kosong" (empty) warning conditions
 * (Backend/crates/core-domain/src/rule.rs) — a client-side early hint, not a save-blocker;
 * the server remains the source of truth and still warns/adjusts on save regardless. */
export function ruleIsEmpty(rule: RuleDraft): boolean {
	if (rule.mode === 'booking_id') return rule.conditions.bookingIds.length === 0;
	if (rule.mode === 'route') return rule.conditions.origin === '' && rule.conditions.destinations.length === 0;
	return false;
}

export function setCocOnly(c: RuleConditions, value: boolean): RuleConditions {
	return { ...c, cocOnly: value, nonCocOnly: value ? false : c.nonCocOnly };
}

export function setNonCocOnly(c: RuleConditions, value: boolean): RuleConditions {
	return { ...c, nonCocOnly: value, cocOnly: value ? false : c.cocOnly };
}

/** Deep-equal comparison ignoring `clientKey` (ephemeral, regenerated every load — must never
 * cause a false "dirty" reading) but INCLUDING `id` (a rule going from saved `id: "..."` to a
 * freshly-added `id: null` IS a real content difference). Rule order matters (an unsaved reorder
 * is a real edit). */
export function isDirty(current: RulesPageState, lastSaved: RulesPageState): boolean {
	if (current.autoAcceptEnabled !== lastSaved.autoAcceptEnabled) return true;
	if (current.rules.length !== lastSaved.rules.length) return true;
	return current.rules.some((rule, i) => {
		const other = lastSaved.rules[i];
		const { clientKey: _a, ...ruleRest } = rule;
		const { clientKey: _b, ...otherRest } = other;
		return JSON.stringify(ruleRest) !== JSON.stringify(otherRest);
	});
}
