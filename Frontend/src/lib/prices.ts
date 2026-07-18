// Pure logic for the /price route price list — no fetch, no DOM. Wire-format mapping lives in
// api-prices.ts, matching the rules.ts/api-rules.ts split established in Fase 7d.

export type PriceDraft = {
	/** Ephemeral, client-generated — for Svelte {#each} keying only, same discipline as
	 * rules.ts's RuleDraft.clientKey (never sent to the server, never used for list identity). */
	clientKey: string;
	/** The server's Uuid for this row, or null if it has never been saved. Unlike /rules, this
	 * DOES round-trip meaningfully — /price's backend is per-resource CRUD (PUT/DELETE act on a
	 * real id), so a saved row's id is load-bearing, not merely informational. */
	id: string | null;
	routeCode: string;
	region: string;
	origin: string;
	destinations: string[];
	price: number;
	vehicleType: string;
};

export function newPriceDraft(): PriceDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: null,
		routeCode: '',
		region: '',
		origin: '',
		destinations: [],
		price: 0,
		vehicleType: ''
	};
}

const RUPIAH_FORMATTER = new Intl.NumberFormat('id-ID');

export function formatRupiah(amount: number): string {
	return `Rp ${RUPIAH_FORMATTER.format(amount)}`;
}

export function matchesFilter(draft: PriceDraft, query: string): boolean {
	const q = query.trim().toLowerCase();
	if (q === '') return true;
	return (
		draft.routeCode.toLowerCase().includes(q) ||
		draft.region.toLowerCase().includes(q) ||
		draft.origin.toLowerCase().includes(q)
	);
}

/** Mirrors the fields the Save button should gate on client-side, for immediate feedback — the
 * server remains the real validator (destinations 1-5 non-empty, route_code uniqueness via 409). */
export function priceDraftIsValid(draft: PriceDraft): boolean {
	return (
		draft.routeCode.trim() !== '' &&
		draft.origin.trim() !== '' &&
		draft.destinations.length > 0 &&
		draft.vehicleType.trim() !== '' &&
		draft.price > 0
	);
}
