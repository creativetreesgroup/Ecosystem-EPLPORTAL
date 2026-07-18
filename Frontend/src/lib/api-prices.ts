// Frontend/src/lib/api-prices.ts
// Thin typed REST layer for /price — no UI logic here. Wire shapes verified directly against
// Backend/crates/api-gateway/src/routes/prices.rs (snake_case, no rename_all anywhere in
// api-gateway). Genuine per-resource CRUD (unlike /rules' replace-all /bookings/settings) — POST
// creates exactly one row and returns it with a real server id; PUT/DELETE act on that id.
import { apiPost, ApiError } from './api';
import { type PriceDraft } from './prices';

type RoutePriceItemWire = {
	id: string;
	route_code: string;
	region: string;
	origin: string;
	destinations: string[];
	price: number;
	vehicle_type: string;
};

type PriceInputWire = Omit<RoutePriceItemWire, 'id'>;

export function priceOutputToDraft(wire: RoutePriceItemWire): PriceDraft {
	return {
		clientKey: crypto.randomUUID(),
		id: wire.id,
		routeCode: wire.route_code,
		region: wire.region,
		origin: wire.origin,
		destinations: wire.destinations,
		price: wire.price,
		vehicleType: wire.vehicle_type
	};
}

export function draftToPriceInput(draft: PriceDraft): PriceInputWire {
	return {
		route_code: draft.routeCode,
		region: draft.region,
		origin: draft.origin,
		destinations: draft.destinations,
		price: draft.price,
		vehicle_type: draft.vehicleType
	};
}

export async function fetchPrices(): Promise<PriceDraft[]> {
	const res = await fetch('/prices', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch prices');
	const items: RoutePriceItemWire[] = await res.json();
	return items.map(priceOutputToDraft);
}

export async function createPrice(draft: PriceDraft): Promise<PriceDraft> {
	const wire = await apiPost<RoutePriceItemWire>('/prices', draftToPriceInput(draft));
	return priceOutputToDraft(wire);
}

/** `apiPost` hardcodes `method: 'POST'` — this is `PUT /prices/{id}`, so it cannot use `apiPost`;
 * raw `fetch` with `method: 'PUT'`, same pattern `api-rules.ts`'s `saveSettings` already
 * established for the identical PUT-vs-apiPost situation. */
export async function updatePrice(id: string, draft: PriceDraft): Promise<PriceDraft> {
	const res = await fetch(`/prices/${id}`, {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(draftToPriceInput(draft))
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to update price');
	const wire: RoutePriceItemWire = await res.json();
	return priceOutputToDraft(wire);
}

/** `DELETE /prices/{id}` returns `204 No Content` on success — never call `res.json()` on this
 * response, there is no body to parse. */
export async function deletePrice(id: string): Promise<void> {
	const res = await fetch(`/prices/${id}`, { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to delete price');
}
