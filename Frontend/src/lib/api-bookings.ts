// Frontend/src/lib/api-bookings.ts
// Thin typed REST layer for /bookings — no UI logic here (that's TicketTicker.svelte).
import { apiPost, ApiError } from './api';
import type { TicketRow } from './ticker';

// Wire shape of Backend/crates/api-gateway/src/routes/bookings.rs's `BookingListItem`.
// Verified by reading the Rust source directly: `#[derive(Debug, Serialize)]` with NO
// `#[serde(rename_all = "camelCase")]` on the struct, and no such attribute anywhere else in
// the api-gateway crate (grepped) — so serde_json emits the struct's OWN field names verbatim,
// i.e. snake_case (`spx_id`, not `spxId`). This is the OPPOSITE convention from the WS event
// payloads (`Backend/crates/ws-hub/src/events.rs`'s `WsEvent::TicketAccepted`, consumed by
// `ws.svelte.ts`'s `TicketAcceptedData`), which DOES use per-field `#[serde(rename = "...")]`
// to match the reference UI's camelCase protocol — a deliberate, disclosed asymmetry between
// the REST and WS layers, not an inconsistency to "fix". Only the fields this module actually
// reads are declared below; extra JSON fields (account_id, weight, cod_amount, ...) are ignored.
type BookingListItem = {
	id: string;
	spx_id: string;
	status: string;
	route: string[];
};

export async function fetchLiveBookings(): Promise<TicketRow[]> {
	const res = await fetch('/bookings/live', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch live bookings');
	const items: BookingListItem[] = await res.json();
	return items.map((item) => ({
		id: item.id,
		spxId: item.spx_id,
		status: item.status as TicketRow['status'],
		route: item.route,
		latencyMs: null,
		localDispatchUs: null,
		accepting: false
	}));
}

type ManualAcceptResponse = { ok: boolean; reason: string; message: string };

/** `id` must be the booking's internal UUID (`TicketRow.id`), NOT `spxId` — this is what
 * `POST /bookings/:id/accept` expects as its path parameter (see `bookings.rs::accept`). */
export async function acceptBooking(id: string): Promise<ManualAcceptResponse> {
	return apiPost<ManualAcceptResponse>(`/bookings/${id}/accept`, {});
}
