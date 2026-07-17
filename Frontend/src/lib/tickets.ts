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

/** Merges two already-individually-sorted (by created_at desc) row sets into one globally-sorted
 * set, then slices out exactly one page's window. Used by `fetchTickets`'s "all statuses" branch
 * to correctly reconstruct page N of a merge of two backend sources (see api-tickets.ts for why
 * naively applying the same page-N offset to both sources independently is wrong beyond page 1).
 * Generic over any row shape carrying `created_at` so this stays a pure, network-free function
 * that's easy to unit test with plain fixtures. */
export function mergeAndSlicePage<T extends { created_at: string }>(
	live: T[],
	history: T[],
	page: number,
	pageSize: number
): { rows: T[]; hasMore: boolean } {
	const merged = [...live, ...history].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
	const start = (page - 1) * pageSize;
	return { rows: merged.slice(start, start + pageSize), hasMore: merged.length > page * pageSize };
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
