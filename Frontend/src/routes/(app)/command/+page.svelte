<!-- Frontend/src/routes/(app)/command/+page.svelte -->
<!-- Command page: Latency Tape + live Ticket Ticker + KPI widget row. Real-time updates arrive over
     the shared WS connection (context set by (app)/+layout.svelte, Task 4); genuinely-new pending
     tickets are detected by polling `/bookings/live` every LIVE_POLL_INTERVAL_MS, since the backend
     never wires a `new_tickets` WS event (disclosed Fase-5 gap, out of scope here). Both the initial
     fetch, the poll, and the WS "ticket_accepted" path all funnel through ticker.ts's pure
     mergeNewTickets/applyAccepted/applyRejected/applyRemoved — one merge path, not two.

     The KPI widget row (Task 8/10/13) is a separate, supplementary data source (`/bookings/summary`,
     polled every SUMMARY_POLL_INTERVAL_MS and refreshed on ticket_accepted) — its failures are
     silently swallowed (see loadSummary) so a summary outage never blocks the live ticket list above. -->
<script lang="ts">
	import { getContext, onMount, onDestroy } from 'svelte';
	import type { WsStore, TowerWsEvent } from '$lib/ws.svelte';
	import { fetchLiveBookings } from '$lib/api-bookings';
	import { fetchSummary, type CommandSummary } from '$lib/api-command';
	import { fetchTickets } from '$lib/api-tickets';
	import { EMPTY_TICKET_FILTERS, type TicketFilters } from '$lib/tickets';
	import { mergeNewTickets, applyAccepted, applyRejected, applyRemoved, type TicketRow } from '$lib/ticker';
	import TicketTicker from '$lib/components/TicketTicker.svelte';
	import LatencyTape from '$lib/components/LatencyTape.svelte';
	import StatCard from '$lib/components/StatCard.svelte';

	const ws = getContext<WsStore>('ws');

	let rows = $state<TicketRow[]>([]);
	let dispatchSamples = $state<number[]>([]);
	let errorMsg = $state('');
	const MAX_SAMPLES = 200;

	let summary = $state<CommandSummary | null>(null);
	type WidgetKey = 'incoming' | 'taken' | 'auto' | 'manual';
	let activeWidget = $state<WidgetKey>('incoming');

	function widgetFilter(key: WidgetKey): TicketFilters {
		if (key === 'incoming') return { ...EMPTY_TICKET_FILTERS, status: 'pending' };
		if (key === 'taken') return { ...EMPTY_TICKET_FILTERS, status: 'failed', acceptReason: 'taken_by_other' };
		if (key === 'auto') return { ...EMPTY_TICKET_FILTERS, status: 'accepted', autoAccepted: true };
		return { ...EMPTY_TICKET_FILTERS, status: 'accepted', autoAccepted: false };
	}

	async function loadSummary() {
		try {
			summary = await fetchSummary();
		} catch {
			// Summary is a supplementary widget row — a fetch failure here must not block the
			// existing live-ticket-list functionality below it, so this is a silent no-op retry
			// on the next poll tick rather than a page-blocking error banner.
		}
	}

	function handleWsEvent(event: TowerWsEvent) {
		if (event.type === 'ticket_accepted') {
			rows = applyAccepted(rows, event.data);
			dispatchSamples = [...dispatchSamples, event.data.localDispatchUs].slice(-MAX_SAMPLES);
			loadSummary();
		} else if (event.type === 'ticket_rejected') {
			rows = applyRejected(rows, event.data.bookingId);
		} else if (event.type === 'tickets_removed') {
			rows = applyRemoved(rows, event.data.ids);
		}
	}

	// Disclosed fallback for detecting genuinely new pending tickets — see file header.
	const LIVE_POLL_INTERVAL_MS = 20_000;
	const SUMMARY_POLL_INTERVAL_MS = 10_000;
	let pollTimer: ReturnType<typeof setInterval> | undefined;
	let summaryTimer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		fetchLiveBookings()
			.then((initial) => {
				rows = mergeNewTickets(rows, initial);
				errorMsg = '';
			})
			.catch(() => {
				errorMsg = 'Gagal memuat tiket terbaru. Mencoba lagi...';
			});
		loadSummary();
		const unsubscribe = ws.onEvent(handleWsEvent);
		pollTimer = setInterval(() => {
			fetchLiveBookings()
				.then((fresh) => {
					rows = mergeNewTickets(rows, fresh);
					errorMsg = '';
				})
				.catch(() => {
					errorMsg = 'Gagal memuat tiket terbaru. Mencoba lagi...';
				});
		}, LIVE_POLL_INTERVAL_MS);
		summaryTimer = setInterval(loadSummary, SUMMARY_POLL_INTERVAL_MS);
		return () => {
			unsubscribe();
		};
	});

	onDestroy(() => {
		if (pollTimer) clearInterval(pollTimer);
		if (summaryTimer) clearInterval(summaryTimer);
	});
</script>

<svelte:head>
	<title>Command — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	{#if errorMsg}
		<div
			role="alert"
			aria-live="polite"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
		>
			{errorMsg}
		</div>
	{/if}

	{#if summary?.latencyP99Ms === null}
		<div class="rounded-lg border border-border bg-bg-surface p-4 text-center text-[12px] text-text-muted">
			Belum ada data auto-accept hari ini.
		</div>
	{:else}
		<LatencyTape samples={dispatchSamples} fallbackMs={summary?.latencyP99Ms ?? undefined} />
	{/if}

	<div class="grid grid-cols-2 sm:grid-cols-4 gap-2.5">
		<StatCard
			label="Tiket Masuk"
			value={summary ? String(summary.incomingToday) : '—'}
			active={activeWidget === 'incoming'}
			onclick={() => (activeWidget = 'incoming')}
		/>
		<StatCard
			label="Close (Agency Lain)"
			value={summary ? String(summary.takenByOtherToday) : '—'}
			active={activeWidget === 'taken'}
			onclick={() => (activeWidget = 'taken')}
		/>
		<StatCard
			label="Accept by Bot"
			value={summary ? String(summary.acceptedAutoToday) : '—'}
			active={activeWidget === 'auto'}
			onclick={() => (activeWidget = 'auto')}
		/>
		<StatCard
			label="Diambil Operator"
			value={summary ? String(summary.acceptedManualToday) : '—'}
			active={activeWidget === 'manual'}
			onclick={() => (activeWidget = 'manual')}
		/>
	</div>

	{#if activeWidget === 'incoming'}
		<TicketTicker bind:rows />
	{:else}
		{#await fetchTickets(widgetFilter(activeWidget), 1)}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:then result}
			<ul class="flex flex-col gap-2">
				{#each result.rows as row (row.id)}
					<li class="p-3 rounded-lg border border-border bg-bg-surface text-[12px] text-text-primary">
						{row.bookingNumber} — {row.route.join(' → ') || '—'}
					</li>
				{:else}
					<li class="p-3 text-[12px] text-text-muted">Tidak ada tiket di kategori ini.</li>
				{/each}
			</ul>
		{:catch}
			<p class="text-[12px] text-danger">Gagal memuat daftar.</p>
		{/await}
	{/if}
</div>
