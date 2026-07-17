<!-- Frontend/src/lib/components/TicketTicker.svelte -->
<!-- Compact-row live ticket ticker with optimistic manual-accept. Rows/merge logic live in
     $lib/ticker.ts (pure, unit-tested there) — this component only wires user interaction
     (click "Terima") to those functions and to the accept REST call. -->
<script lang="ts">
	import type { TicketRow } from '$lib/ticker';
	import { markAccepting, revertAccepting, applyAccepted } from '$lib/ticker';
	import { acceptBooking } from '$lib/api-bookings';
	import { ApiError } from '$lib/api';

	let { rows = $bindable() }: { rows: TicketRow[] } = $props();

	let errorMsg = $state('');

	// `id` is the row's internal UUID (POST /bookings/:id/accept's path param); `spxId` is the
	// SPX booking id used to match this row in the rows array (same key ticker.ts's other
	// functions and the WS delta-merge use) — the two are DIFFERENT identifiers, see ticker.ts's
	// TicketRow doc comment. Calling acceptBooking with spxId instead of id would 404/mismatch
	// against the real booking row.
	async function handleAccept(id: string, spxId: string) {
		rows = markAccepting(rows, spxId);
		errorMsg = '';
		try {
			const result = await acceptBooking(id);
			if (!result.ok) {
				rows = revertAccepting(rows, spxId);
				errorMsg = result.message;
				return;
			}
			// Manual accept never triggers a backend ticket_accepted WS event (that only fires for
			// auto-accept dispatch), so there is no authoritative latency measurement to reconcile
			// with here. Confirm the row optimistically but leave latency/localDispatchUs null —
			// honest "we don't know" rather than a fabricated 0ms.
			rows = applyAccepted(rows, { bookingId: spxId, latencyMs: null, localDispatchUs: null });
		} catch (e) {
			rows = revertAccepting(rows, spxId);
			if (e instanceof ApiError && e.status === 409) {
				errorMsg = 'Booking ini sudah tidak tersedia — mungkin sudah diambil pihak lain.';
			} else if (e instanceof ApiError) {
				errorMsg = 'Server gagal memproses. Coba lagi.';
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		}
	}
</script>

<div class="rounded-lg border border-border bg-bg-surface overflow-hidden">
	{#if errorMsg}
		<div
			role="alert"
			aria-live="polite"
			class="px-3 py-2 text-[11px] font-body text-danger border-b border-border bg-danger/10"
		>
			{errorMsg}
		</div>
	{/if}
	<ul class="divide-y divide-border max-h-[420px] overflow-y-auto">
		{#each rows as row (row.spxId)}
			<li class="flex items-center gap-2.5 px-3 py-2 text-[11px] font-body">
				<span
					aria-hidden="true"
					class="w-1.5 h-1.5 rounded-full shrink-0
						{row.status === 'accepted' ? 'bg-live' : row.status === 'taken_by_agency' ? 'bg-text-muted' : 'bg-accent'}"
				></span>
				<span class="font-mono text-text-muted w-24 shrink-0 truncate">{row.spxId}</span>
				<span class="text-text-primary flex-1 truncate">{row.route.join(' → ') || '—'}</span>
				{#if row.status === 'pending'}
					<button
						type="button"
						disabled={row.accepting}
						onclick={() => handleAccept(row.id, row.spxId)}
						class="min-h-[44px] min-w-[44px] px-2.5 rounded-md text-[10px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{row.accepting ? 'Memproses…' : 'Terima'}
					</button>
				{:else if row.status === 'accepted'}
					<span class="font-mono text-live">{row.latencyMs === null ? 'diterima' : `${row.latencyMs}ms`}</span>
				{:else}
					<span class="font-mono text-text-muted">diambil lain</span>
				{/if}
			</li>
		{/each}
	</ul>
</div>
