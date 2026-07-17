<!-- Frontend/src/routes/(app)/command/+page.svelte -->
<!-- Command page: Latency Tape + live Ticket Ticker. Real-time updates arrive over the shared
     WS connection (context set by (app)/+layout.svelte, Task 4); genuinely-new pending tickets
     are detected by polling `/bookings/live` every LIVE_POLL_INTERVAL_MS, since the backend never
     wires a `new_tickets` WS event (disclosed Fase-5 gap, out of scope here). Both the initial
     fetch, the poll, and the WS "ticket_accepted" path all funnel through ticker.ts's pure
     mergeNewTickets/applyAccepted/applyRejected/applyRemoved — one merge path, not two. -->
<script lang="ts">
	import { getContext, onMount, onDestroy } from 'svelte';
	import type { WsStore, TowerWsEvent } from '$lib/ws.svelte';
	import { fetchLiveBookings } from '$lib/api-bookings';
	import { mergeNewTickets, applyAccepted, applyRejected, applyRemoved, type TicketRow } from '$lib/ticker';
	import TicketTicker from '$lib/components/TicketTicker.svelte';
	import LatencyTape from '$lib/components/LatencyTape.svelte';

	const ws = getContext<WsStore>('ws');

	let rows = $state<TicketRow[]>([]);
	let dispatchSamples = $state<number[]>([]);
	const MAX_SAMPLES = 200;

	function handleWsEvent(event: TowerWsEvent) {
		if (event.type === 'ticket_accepted') {
			rows = applyAccepted(rows, event.data);
			dispatchSamples = [...dispatchSamples, event.data.localDispatchUs].slice(-MAX_SAMPLES);
		} else if (event.type === 'ticket_rejected') {
			rows = applyRejected(rows, event.data.bookingId);
		} else if (event.type === 'tickets_removed') {
			rows = applyRemoved(rows, event.data.ids);
		}
	}

	// Disclosed fallback for detecting genuinely new pending tickets — see file header.
	const LIVE_POLL_INTERVAL_MS = 20_000;
	let pollTimer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		fetchLiveBookings().then((initial) => {
			rows = mergeNewTickets(rows, initial);
		});
		const unsubscribe = ws.onEvent(handleWsEvent);
		pollTimer = setInterval(() => {
			fetchLiveBookings().then((fresh) => {
				rows = mergeNewTickets(rows, fresh);
			});
		}, LIVE_POLL_INTERVAL_MS);
		return () => {
			unsubscribe();
		};
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
	});
</script>

<svelte:head>
	<title>Command — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	<LatencyTape samples={dispatchSamples} />
	<TicketTicker bind:rows />
</div>
