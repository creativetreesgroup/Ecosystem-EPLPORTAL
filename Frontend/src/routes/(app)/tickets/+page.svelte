<!-- Frontend/src/routes/(app)/tickets/+page.svelte -->
<!-- /tickets: filtered/paginated ticket browser + manual accept + detail drawer. Thin
     orchestration layer only — filter/pagination math lives in api-tickets.ts's fetchTickets
     (Task 4, both pagination bugs fixed+tested there), row-state transitions live in tickets.ts
     (Task 3). This is a browse/search surface, not a live feed: WS "ticket_accepted" only
     reconciles a row that happens to already be on the current page (safe no-op otherwise, see
     applyRowAccepted below) — re-filtering/re-paginating is how the user sees current state,
     consistent with /command owning the live-dashboard job instead. -->
<script lang="ts">
	import { getContext, onMount } from 'svelte';
	import type { WsStore, TowerWsEvent } from '$lib/ws.svelte';
	import { fetchTickets, acceptBooking } from '$lib/api-tickets';
	import {
		markRowAccepting,
		revertRowAccepting,
		applyRowAccepted,
		type TicketDetailRow,
		type TicketFilters
	} from '$lib/tickets';
	import TicketFilterBar from '$lib/components/TicketFilterBar.svelte';
	import TicketsTable from '$lib/components/TicketsTable.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import TicketDetailDrawer from '$lib/components/TicketDetailDrawer.svelte';
	import { ApiError } from '$lib/api';

	const ws = getContext<WsStore>('ws');

	let filters = $state<TicketFilters>({ status: null, spxId: '', from: null, to: null });
	let page = $state(1);
	let rows = $state<TicketDetailRow[]>([]);
	let hasMore = $state(false);
	let loading = $state(false);
	let errorMsg = $state('');
	let selectedBookingId = $state<string | null>(null);

	// Monotonic guard against out-of-order responses: same bug class TicketDetailDrawer (Task 7)
	// had to fix with its `requestedId` check — a slow request for a PREVIOUS filters/page landing
	// after a newer one already started must not clobber current state. Rapid pagination clicks or
	// fast filter edits under variable network latency make this a real, not speculative, race.
	let loadSeq = 0;

	async function loadTickets() {
		const seq = ++loadSeq;
		loading = true;
		try {
			const result = await fetchTickets(filters, page);
			if (seq !== loadSeq) return;
			rows = result.rows;
			hasMore = result.hasMore;
			errorMsg = '';
		} catch {
			if (seq !== loadSeq) return;
			errorMsg = 'Gagal memuat daftar tiket. Coba lagi.';
		} finally {
			if (seq === loadSeq) loading = false;
		}
	}

	function handleFiltersChange(next: TicketFilters) {
		filters = next;
		page = 1;
		loadTickets();
	}

	function handlePageChange(next: number) {
		page = next;
		loadTickets();
	}

	async function handleAccept(row: TicketDetailRow) {
		rows = markRowAccepting(rows, row.id);
		errorMsg = '';
		try {
			const result = await acceptBooking(row.id);
			if (!result.ok) {
				rows = revertRowAccepting(rows, row.id);
				errorMsg = result.message;
				return;
			}
			rows = applyRowAccepted(rows, row.id);
		} catch (e) {
			rows = revertRowAccepting(rows, row.id);
			if (e instanceof ApiError && e.status === 409) {
				errorMsg = 'Tiket ini sudah tidak tersedia — mungkin sudah diambil pihak lain.';
			} else if (e instanceof ApiError) {
				errorMsg = 'Server gagal memproses. Coba lagi.';
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		}
	}

	// WS event's `bookingId` field is keyed on spxId (matches ws.svelte.ts's documented wire
	// shape, same as ticker.ts's applyAccepted) — but tickets.ts's applyRowAccepted matches on the
	// row's internal `id`, not spxId (it must, since markRowAccepting/revertRowAccepting/
	// acceptBooking above all key on `row.id` too). So this looks up the row by spxId first to
	// resolve its internal id, then reconciles through that id. No match on the current page ->
	// '' matches nothing -> safe no-op (see file header).
	function handleWsEvent(event: TowerWsEvent) {
		if (event.type === 'ticket_accepted') {
			const matchedId = rows.find((r) => r.spxId === event.data.bookingId)?.id ?? '';
			rows = applyRowAccepted(rows, matchedId);
		}
	}

	onMount(() => {
		loadTickets();
		const unsubscribe = ws.onEvent(handleWsEvent);
		return () => unsubscribe();
	});
</script>

<svelte:head>
	<title>Tickets — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-6xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Tickets</h1>

	<TicketFilterBar {filters} onFiltersChange={handleFiltersChange} />

	{#if errorMsg}
		<div
			role="alert"
			aria-live="polite"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
		>
			{errorMsg}
		</div>
	{/if}

	{#if loading}
		<p class="text-[12px] text-text-muted">Memuat…</p>
	{:else}
		<TicketsTable {rows} onRowClick={(row) => (selectedBookingId = row.id)} onAccept={handleAccept} />
	{/if}

	<Pagination {page} {hasMore} onPageChange={handlePageChange} />
</div>

<TicketDetailDrawer bookingId={selectedBookingId} onClose={() => (selectedBookingId = null)} />
