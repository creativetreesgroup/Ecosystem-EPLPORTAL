// Frontend/src/lib/ws.svelte.ts
// Single shared WebSocket connection to /ws — cookie-authenticated (Fase 7a Task 4's
// ws-hub cookie fix), no ?session= needed; the browser attaches the HttpOnly session cookie
// automatically on same-origin WS handshakes (via Caddy in prod, Vite's proxy in dev — both
// already route /ws to reactor-core, Fase 7a Tasks 2/3).
export type WsStatus = 'connecting' | 'connected' | 'reconnecting' | 'disconnected';

// Matches Backend/crates/ws-hub/src/events.rs's WsEvent enum shape exactly
// (#[serde(tag="type", content="data")]) — ONLY the variants this app actually consumes today
// are typed; unknown "type" values are ignored (forward-compatible with new backend event
// variants this frontend doesn't know about yet).
export type TicketAcceptedData = {
	bookingId: string;
	latencyMs: number;
	autoAccept: boolean;
	rule: string;
	route: string[];
	localDispatchUs: number;
};
export type TowerWsEvent =
	| { type: 'connected'; data: { session: string } }
	| { type: 'ticket_accepted'; data: TicketAcceptedData }
	| { type: 'ticket_rejected'; data: { bookingId: string } }
	| { type: 'tickets_removed'; data: { ids: string[] } };

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15000;

// Exported so the reconnect math can be unit-tested directly (ws.svelte.test.ts) without
// spinning up a real WebSocket/browser context.
export function backoffDelay(attempt: number, base = RECONNECT_BASE_MS, max = RECONNECT_MAX_MS): number {
	return Math.min(base * 2 ** attempt, max);
}

export function createWsStore() {
	let status = $state<WsStatus>('connecting');
	let socket: WebSocket | null = null;
	let reconnectAttempt = 0;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let closedByUs = false;
	const handlers = new Set<(event: TowerWsEvent) => void>();

	function scheduleReconnect() {
		if (closedByUs) return;
		status = 'reconnecting';
		const delay = backoffDelay(reconnectAttempt);
		reconnectAttempt += 1;
		reconnectTimer = setTimeout(connect, delay);
	}

	function connect() {
		const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
		socket = new WebSocket(`${proto}//${location.host}/ws`);
		socket.addEventListener('open', () => {
			status = 'connected';
			reconnectAttempt = 0;
		});
		socket.addEventListener('message', (ev) => {
			let parsed: TowerWsEvent;
			try {
				parsed = JSON.parse(ev.data);
			} catch {
				return;
			}
			for (const h of handlers) h(parsed);
		});
		socket.addEventListener('close', () => {
			if (closedByUs) {
				status = 'disconnected';
				return;
			}
			scheduleReconnect();
		});
		socket.addEventListener('error', () => {
			socket?.close();
		});
	}

	connect();

	return {
		get status() {
			return status;
		},
		onEvent(handler: (event: TowerWsEvent) => void) {
			handlers.add(handler);
			return () => handlers.delete(handler);
		},
		close() {
			closedByUs = true;
			if (reconnectTimer) {
				clearTimeout(reconnectTimer);
				reconnectTimer = null;
			}
			socket?.close();
			status = 'disconnected';
		}
	};
}

export type WsStore = ReturnType<typeof createWsStore>;
