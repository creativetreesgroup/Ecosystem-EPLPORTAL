// Frontend/src/lib/ticker.ts
// Pure delta-merge logic for the Live Ticket Ticker — deliberately separate from
// TicketTicker.svelte (Task 6) per the master spec's "logic merge/optimistic di helper $lib
// teruji" requirement. Every function takes the current rows array and returns a NEW array
// (never mutates its input) so Svelte 5's $state reassignment triggers reactivity correctly.
export type TicketRow = {
	spxId: string;
	status: 'pending' | 'accepted' | 'taken_by_agency';
	route: string[];
	latencyMs: number | null;
	localDispatchUs: number | null;
	/** True while an optimistic accept is in flight for this row. */
	accepting: boolean;
};

export function mergeNewTickets(rows: TicketRow[], incoming: TicketRow[]): TicketRow[] {
	const knownIds = new Set(rows.map((r) => r.spxId));
	const genuinelyNew = incoming.filter((r) => !knownIds.has(r.spxId));
	return [...genuinelyNew, ...rows];
}

export function applyAccepted(
	rows: TicketRow[],
	// Shape matches ws.svelte.ts's TicketAcceptedData (the real WS event payload); only
	// bookingId/latencyMs/localDispatchUs are used here, the rest are accepted-and-ignored
	// so callers can pass the full event data without a cast.
	data: {
		bookingId: string;
		latencyMs: number;
		localDispatchUs: number;
		autoAccept?: boolean;
		rule?: string;
		route?: string[];
	}
): TicketRow[] {
	return rows.map((r) =>
		r.spxId === data.bookingId
			? { ...r, status: 'accepted' as const, latencyMs: data.latencyMs, localDispatchUs: data.localDispatchUs, accepting: false }
			: r
	);
}

export function applyRejected(rows: TicketRow[], bookingId: string): TicketRow[] {
	return rows.map((r) => (r.spxId === bookingId ? { ...r, status: 'taken_by_agency' as const, accepting: false } : r));
}

export function applyRemoved(rows: TicketRow[], ids: string[]): TicketRow[] {
	const removeSet = new Set(ids);
	return rows.filter((r) => !removeSet.has(r.spxId));
}

/** Optimistic accept: mark a row as "in flight" the instant the user clicks, before the server responds. */
export function markAccepting(rows: TicketRow[], spxId: string): TicketRow[] {
	return rows.map((r) => (r.spxId === spxId ? { ...r, accepting: true } : r));
}

/** Revert an optimistic accept that the server rejected (409/500/network failure). */
export function revertAccepting(rows: TicketRow[], spxId: string): TicketRow[] {
	return rows.map((r) => (r.spxId === spxId ? { ...r, accepting: false } : r));
}
