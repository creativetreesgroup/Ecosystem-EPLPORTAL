// Frontend/src/lib/tickets.ts
// Pure logic for the /tickets full-management view — deliberately NOT a reuse of
// $lib/ticker.ts's TicketRow (that type's 3-value status union is correct for /command's
// live-only scope, wrong for this view's full pending|accepted|failed + sub-reason vocabulary).
// Every function returns a NEW array (never mutates), matching ticker.ts's own convention so
// Svelte 5's $state reassignment triggers reactivity correctly.

export type TicketStatus = 'pending' | 'accepted' | 'failed';
export type FailureReason = 'expired' | 'taken_by_other' | 'manual_accept_failed' | null;

export type TicketDetailRow = {
	id: string;
	spxId: string;
	status: TicketStatus;
	failureReason: FailureReason;
	route: string[];
	serviceType: string | null;
	weight: number;
	codAmount: number;
	autoAccepted: boolean;
	createdAt: string;
	/** True while an optimistic accept is in flight for this row. */
	accepting: boolean;
};

export type TicketFilters = {
	status: TicketStatus | null;
	spxId: string;
	from: string | null;
	to: string | null;
};

const PAGE_SIZE_DEFAULT = 50;

/** Maps 1-indexed `page` + `pageSize` to the backend's `limit`/`offset` convention, and only
 * includes filter params that are actually set — an omitted param means "no filter", not an
 * empty-string filter, matching the backend's `Option<T>` query-param semantics. */
export function filtersToQueryString(
	filters: Pick<TicketFilters, 'status' | 'spxId' | 'from' | 'to'>,
	page: number,
	pageSize: number = PAGE_SIZE_DEFAULT
): string {
	const params = new URLSearchParams();
	if (filters.status) params.set('status', filters.status);
	if (filters.spxId) params.set('spx_id', filters.spxId);
	if (filters.from) params.set('from', filters.from);
	if (filters.to) params.set('to', filters.to);
	params.set('limit', String(pageSize));
	params.set('offset', String((page - 1) * pageSize));
	return params.toString();
}

export function markRowAccepting(rows: TicketDetailRow[], id: string): TicketDetailRow[] {
	return rows.map((r) => (r.id === id ? { ...r, accepting: true } : r));
}

export function revertRowAccepting(rows: TicketDetailRow[], id: string): TicketDetailRow[] {
	return rows.map((r) => (r.id === id ? { ...r, accepting: false } : r));
}

export function applyRowAccepted(rows: TicketDetailRow[], id: string): TicketDetailRow[] {
	return rows.map((r) => (r.id === id ? { ...r, status: 'accepted' as const, accepting: false } : r));
}
