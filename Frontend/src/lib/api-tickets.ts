// Frontend/src/lib/api-tickets.ts
// Thin typed REST layer for /tickets — no UI logic here.
import { ApiError } from './api';
import { acceptBooking } from './api-bookings';
import { filtersToQueryString, type TicketDetailRow, type TicketFilters, type FailureReason } from './tickets';

export { acceptBooking };

// Wire shape of BookingListItem (snake_case — no rename_all anywhere in api-gateway, confirmed
// by reading Backend/crates/api-gateway/src/routes/bookings.rs directly). Only the fields this
// module reads are declared; extra JSON fields (account_id, rule_matched, ...) are ignored.
type BookingListItemWire = {
	id: string;
	spx_id: string;
	status: string;
	service_type: string | null;
	weight: number;
	cod_amount: number;
	auto_accepted: boolean;
	created_at: string;
	route: string[];
};

function failureReasonFromRaw(status: string, raw: Record<string, unknown> | undefined): FailureReason {
	if (status !== 'failed' || !raw) return null;
	const reason = raw['drift_reason'] ?? raw['accept_reason'];
	if (reason === 'expired' || reason === 'taken_by_other' || reason === 'manual_accept_failed') return reason;
	return null;
}

function toDetailRow(item: BookingListItemWire, failureReason: FailureReason = null): TicketDetailRow {
	return {
		id: item.id,
		spxId: item.spx_id,
		status: item.status as TicketDetailRow['status'],
		failureReason,
		route: item.route,
		serviceType: item.service_type,
		weight: item.weight,
		codAmount: item.cod_amount,
		autoAccepted: item.auto_accepted,
		createdAt: item.created_at,
		accepting: false
	};
}

const PAGE_SIZE = 50;

/** Routes to /live or /history (or both, merged) based on the status filter, per the design
 * doc's data-flow decision — /tickets stays a browse/search surface backed by the existing
 * two endpoints rather than a new merged one. Fetches one extra row beyond pageSize to compute
 * `hasMore` without a separate count query.
 *
 * `failureReason` is always null here — `BookingListItem` (the /live and /history wire shape)
 * does not carry `raw_data`, so a list row cannot derive a specific failure sub-reason; only
 * `fetchBookingDetail` (below) can, via `BookingDetail.raw_data`. Disclosed scope simplification
 * from the plan, not a bug — do not add a backend field to fix this. */
export async function fetchTickets(
	filters: TicketFilters,
	page: number
): Promise<{ rows: TicketDetailRow[]; hasMore: boolean }> {
	const qs = filtersToQueryString(filters, page, PAGE_SIZE + 1);

	async function fetchOne(path: string): Promise<BookingListItemWire[]> {
		const res = await fetch(`${path}?${qs}`, { credentials: 'include' });
		if (!res.ok) throw new ApiError(res.status, `failed to fetch ${path}`);
		return res.json();
	}

	let items: BookingListItemWire[];
	if (filters.status === 'pending') {
		items = await fetchOne('/bookings/live');
	} else if (filters.status === 'accepted' || filters.status === 'failed') {
		items = await fetchOne('/bookings/history');
	} else {
		const [live, history] = await Promise.all([fetchOne('/bookings/live'), fetchOne('/bookings/history')]);
		items = [...live, ...history].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
	}

	const hasMore = items.length > PAGE_SIZE;
	const pageItems = items.slice(0, PAGE_SIZE);
	return { rows: pageItems.map((item) => toDetailRow(item, null)), hasMore };
}

// Wire shape of BookingDetail (snake_case, includes raw_data for failureReason derivation).
type BookingDetailWire = {
	id: string;
	spx_id: string;
	status: string;
	raw_data: Record<string, unknown>;
	is_coc: boolean;
	service_type: string | null;
	weight: number;
	cod_amount: number;
	auto_accepted: boolean;
	accept_latency_ms: number | null;
	created_at: string;
	updated_at: string;
	route: string[];
};

export async function fetchBookingDetail(
	id: string
): Promise<TicketDetailRow & { updatedAt: string; acceptLatencyMs: number | null; isCoc: boolean }> {
	const res = await fetch(`/bookings/${id}/detail`, { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch booking detail');
	const item: BookingDetailWire = await res.json();
	const failureReason = failureReasonFromRaw(item.status, item.raw_data);
	return {
		...toDetailRow(item, failureReason),
		updatedAt: item.updated_at,
		acceptLatencyMs: item.accept_latency_ms,
		isCoc: item.is_coc
	};
}

export type AuditEvent = {
	id: string;
	ruleId: string | null;
	outcome: string;
	localDispatchUs: number | null;
	acceptE2eMs: number | null;
	createdAt: string;
};

// Wire shape of AcceptEventItem — only the fields this module reads (booking_id, detail are
// ignored, matching this module's own "declare only what's read" convention).
type AcceptEventItemWire = {
	id: string;
	rule_id: string | null;
	outcome: string;
	local_dispatch_us: number | null;
	accept_e2e_ms: number | null;
	created_at: string;
};

export async function fetchAuditTrail(id: string): Promise<AuditEvent[]> {
	const res = await fetch(`/bookings/${id}/audit-trail`, { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch audit trail');
	const items: AcceptEventItemWire[] = await res.json();
	return items.map((e) => ({
		id: e.id,
		ruleId: e.rule_id,
		outcome: e.outcome,
		localDispatchUs: e.local_dispatch_us,
		acceptE2eMs: e.accept_e2e_ms,
		createdAt: e.created_at
	}));
}
