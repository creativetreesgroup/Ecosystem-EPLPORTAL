// Frontend/src/lib/api-rules.ts
// Thin typed REST layer for /rules — no UI logic here. Wire shapes verified directly against
// Backend/crates/api-gateway/src/routes/rules.rs, locations.rs, otp.rs (all snake_case, no
// rename_all anywhere in api-gateway).
import { apiPost, ApiError } from './api';
import {
	type RuleDraft,
	type RulesPageState,
	type RuleMode,
	type BookingType,
	type MatchMode
} from './rules';

type RuleOutputWire = {
	id: string;
	name: string;
	enabled: boolean;
	priority: number;
	mode: string;
	service_types: string[];
	max_weight: number | null;
	coc_only: boolean;
	non_coc_only: boolean;
	max_cod_amount: number | null;
	booking_ids: string[];
	origin: string;
	destinations: string[];
	booking_type: string;
	shift_types: number[];
	trip_types: number[];
	match_mode: string;
	min_deadline_min: number | null;
	max_accept_count: number;
	accepted_count: number;
};

type SettingsResponseWire = {
	auto_accept_enabled: boolean;
	rules: RuleOutputWire[];
	warnings?: string[];
};

/** Exported for Task 8's e2e reference only indirectly — the real export surface is
 * fetchSettings/saveSettings below. Exported at module level (not `function` inside
 * fetchSettings) so api-rules.test.ts can unit-test the mapping without a network call. */
export function ruleOutputToDraft(wire: RuleOutputWire): RuleDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: wire.id,
		name: wire.name,
		enabled: wire.enabled,
		priority: wire.priority,
		mode: wire.mode as RuleMode,
		conditions: {
			serviceTypes: wire.service_types,
			maxWeight: wire.max_weight,
			cocOnly: wire.coc_only,
			nonCocOnly: wire.non_coc_only,
			maxCodAmount: wire.max_cod_amount,
			bookingIds: wire.booking_ids,
			origin: wire.origin,
			destinations: wire.destinations,
			bookingType: wire.booking_type as BookingType,
			shiftTypes: wire.shift_types,
			tripTypes: wire.trip_types,
			matchMode: wire.match_mode as MatchMode,
			minDeadlineMin: wire.min_deadline_min,
			maxAcceptCount: wire.max_accept_count,
			acceptedCount: wire.accepted_count
		}
	};
}

type RuleInputWire = Omit<RuleOutputWire, 'id'>;

export function draftToRuleInput(draft: RuleDraft): RuleInputWire {
	const c = draft.conditions;
	return {
		name: draft.name,
		enabled: draft.enabled,
		priority: draft.priority,
		mode: draft.mode,
		service_types: c.serviceTypes,
		max_weight: c.maxWeight,
		coc_only: c.cocOnly,
		non_coc_only: c.nonCocOnly,
		max_cod_amount: c.maxCodAmount,
		booking_ids: c.bookingIds,
		origin: c.origin,
		destinations: c.destinations,
		booking_type: c.bookingType,
		shift_types: c.shiftTypes,
		trip_types: c.tripTypes,
		match_mode: c.matchMode,
		min_deadline_min: c.minDeadlineMin,
		max_accept_count: c.maxAcceptCount,
		accepted_count: c.acceptedCount
	};
}

function fromSettingsWire(wire: SettingsResponseWire): RulesPageState & { warnings: string[] } {
	return {
		autoAcceptEnabled: wire.auto_accept_enabled,
		rules: wire.rules.map(ruleOutputToDraft),
		warnings: wire.warnings ?? []
	};
}

export async function fetchSettings(): Promise<RulesPageState & { warnings: string[] }> {
	const res = await fetch('/bookings/settings', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch rule settings');
	const wire: SettingsResponseWire = await res.json();
	return fromSettingsWire(wire);
}

/** `apiPost` (Frontend/src/lib/api.ts) hardcodes `method: 'POST'` — the backend route is
 * `PUT /bookings/settings` (Backend/crates/api-gateway/src/routes/rules.rs's `rules_router`:
 * `.route("/settings", get(get_settings).put(put_settings))`), so this cannot use `apiPost`; a
 * POST here would 405. Raw `fetch` with `method: 'PUT'`, same header/credentials/error shape as
 * `apiPost` otherwise. */
export async function saveSettings(state: RulesPageState): Promise<RulesPageState & { warnings: string[] }> {
	const res = await fetch('/bookings/settings', {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({
			auto_accept_enabled: state.autoAcceptEnabled,
			rules: state.rules.map(draftToRuleInput)
		})
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save rule settings');
	const wire: SettingsResponseWire = await res.json();
	return fromSettingsWire(wire);
}

export type LocationItem = { id: string; name: string };

export async function fetchLocations(): Promise<LocationItem[]> {
	const res = await fetch('/locations', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch locations');
	return res.json();
}

export async function createLocation(name: string): Promise<LocationItem> {
	return apiPost<LocationItem>('/locations', { name });
}

export async function requestAaOtp(): Promise<void> {
	await apiPost<{ ok: boolean }>('/auth/request-aa-otp', {});
}

export async function verifyAaOtp(code: string): Promise<void> {
	await apiPost<{ ok: boolean }>('/auth/verify-aa-otp', { code });
}
