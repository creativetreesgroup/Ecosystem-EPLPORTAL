// Frontend/src/lib/api-activity.ts
// Thin typed REST layer for /activity — no UI logic here. Wire shapes verified directly against
// Backend/crates/api-gateway/src/routes/bookings.rs (spx_log/AcceptEventItem) and routes/bot.rs
// (get_logs/delete_logs) — snake_case throughout, no rename_all anywhere in api-gateway.
import { ApiError } from './api';

const PAGE_SIZE = 20;

export type AcceptEventRow = {
	id: string;
	bookingId: string | null;
	ruleId: string | null;
	outcome: string;
	localDispatchUs: number | null;
	acceptE2eMs: number | null;
	detail: unknown;
	createdAt: Date;
};

type AcceptEventItemWire = {
	id: string;
	booking_id: string | null;
	rule_id: string | null;
	outcome: string;
	local_dispatch_us: number | null;
	accept_e2e_ms: number | null;
	detail: unknown;
	created_at: string;
};

function acceptEventToRow(wire: AcceptEventItemWire): AcceptEventRow {
	return {
		id: wire.id,
		bookingId: wire.booking_id,
		ruleId: wire.rule_id,
		outcome: wire.outcome,
		localDispatchUs: wire.local_dispatch_us,
		acceptE2eMs: wire.accept_e2e_ms,
		detail: wire.detail,
		createdAt: new Date(wire.created_at)
	};
}

/** `GET /bookings/spx-log` supports ONLY `limit`/`offset` — it ignores `status`/`spx_id`/
 * `from`/`to` even though they exist on the backend's shared `ListParams` struct (confirmed by
 * reading the handler directly). `hasMore` uses the same overfetch-by-one technique as
 * `Frontend/src/lib/api-tickets.ts::fetchTickets`: request `PAGE_SIZE + 1`, check whether that
 * many rows actually came back, then slice down to `PAGE_SIZE` before returning — a naive
 * "did this page come back full" check is wrong exactly when the total count is a multiple of
 * `PAGE_SIZE`. */
export async function fetchAcceptEvents(page: number): Promise<{ rows: AcceptEventRow[]; hasMore: boolean }> {
	const offset = (page - 1) * PAGE_SIZE;
	const params = new URLSearchParams({ limit: String(PAGE_SIZE + 1), offset: String(offset) });
	const res = await fetch(`/bookings/spx-log?${params.toString()}`, { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch accept events');
	const items: AcceptEventItemWire[] = await res.json();
	const hasMore = items.length > PAGE_SIZE;
	return { rows: items.slice(0, PAGE_SIZE).map(acceptEventToRow), hasMore };
}

export type BotLogRow = {
	ts: number;
	logType: string;
	kind: string | null;
	bookingId: string | null;
	latencyMs: number | null;
	rule: string | null;
	error: string | null;
};

type BotLogEntryWire = {
	ts: number;
	log_type: string;
	kind: string | null;
	booking_id: string | null;
	latency_ms: number | null;
	rule: string | null;
	error: string | null;
};

export async function fetchBotLogs(): Promise<BotLogRow[]> {
	const res = await fetch('/bot/logs', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch bot logs');
	const items: BotLogEntryWire[] = await res.json();
	return items.map((w) => ({
		ts: w.ts,
		logType: w.log_type,
		kind: w.kind,
		bookingId: w.booking_id,
		latencyMs: w.latency_ms,
		rule: w.rule,
		error: w.error
	}));
}

/** `DELETE /bot/logs` returns `204 No Content` on success — never call `.json()` on this
 * response, there is no body to parse. */
export async function clearBotLogs(): Promise<void> {
	const res = await fetch('/bot/logs', { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to clear bot logs');
}
