<!-- Frontend/src/lib/components/TicketsTable.svelte -->
<script lang="ts">
	// Real <table> on desktop (screen readers get real table navigation), stacked cards on
	// narrow viewports — ONE component, ONE source of row data, toggled via Tailwind's `md:`
	// breakpoint rather than two separate components that could drift out of sync.
	import type { TicketDetailRow } from '$lib/tickets';
	import CountdownBadge from './CountdownBadge.svelte';

	let {
		rows,
		onRowClick,
		onAccept
	}: {
		rows: TicketDetailRow[];
		onRowClick: (row: TicketDetailRow) => void;
		onAccept: (row: TicketDetailRow) => void;
	} = $props();

	function statusDotClass(status: TicketDetailRow['status']): string {
		if (status === 'accepted') return 'bg-live';
		if (status === 'failed') return 'bg-danger';
		return 'bg-accent';
	}

	function statusLabel(status: TicketDetailRow['status']): string {
		if (status === 'accepted') return 'Diterima';
		if (status === 'failed') return 'Gagal';
		return 'Pending';
	}

	function formatDate(iso: string): string {
		return new Date(iso).toLocaleString('id-ID', { dateStyle: 'medium', timeStyle: 'short' });
	}

	// Best-effort mapping, NOT independently verified against a captured SPX payload — see the
	// design doc's Open Questions. Single named constant so it's a one-line fix if reversed.
	const TRIP_TYPE_ADHOC = 0;

	function tripTypeLabel(tripType: number | null): string | null {
		if (tripType === null) return null;
		return tripType === TRIP_TYPE_ADHOC ? 'ADHOC' : 'FIX';
	}
</script>

{#if rows.length === 0}
	<div class="p-8 text-center text-[13px] font-body text-text-muted rounded-lg border border-border bg-bg-surface">
		Tidak ada tiket yang cocok dengan filter ini.
	</div>
{:else}
	<!-- Desktop: real table -->
	<table class="hidden md:table w-full text-[12px] font-body border-collapse">
		<caption class="sr-only">Daftar tiket booking</caption>
		<thead>
			<tr class="border-b border-border text-left text-[10px] uppercase tracking-wide text-text-muted">
				<th scope="col" class="py-2 pr-3">ID</th>
				<th scope="col" class="py-2 pr-3">Booking Number</th>
				<th scope="col" class="py-2 pr-3">Route & Vehicle</th>
				<th scope="col" class="py-2 pr-3">Jadwal Booking</th>
				<th scope="col" class="py-2 pr-3">Deadline Bidding</th>
				<th scope="col" class="py-2 pr-3">Tags</th>
				<th scope="col" class="py-2 pr-3">Status</th>
				<th scope="col" class="py-2 pr-3">Accept By</th>
				<th scope="col" class="py-2 pr-3"><span class="sr-only">Aksi</span></th>
			</tr>
		</thead>
		<tbody>
			{#each rows as row (row.id)}
				<tr
					tabindex="0"
					onclick={() => onRowClick(row)}
					onkeydown={(e) => {
						// Guard against the "Terima" button's keydown bubbling up from the nested <td>:
						// without this, pressing Enter on the button would also fire this row's
						// onRowClick (and cancel the button's own native Enter→click activation).
						if (e.target !== e.currentTarget) return;
						if (e.key === 'Enter') {
							onRowClick(row);
						}
					}}
					class="border-b border-border hover:bg-bg-base cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-inset"
				>
					<td class="py-2.5 pr-3 font-mono text-[11px] text-text-muted whitespace-nowrap">
						<div>BK <span class="text-text-primary">{row.bookingNumber}</span></div>
						{#if row.requestId}<div>REQ <span class="text-text-primary">{row.requestId}</span></div>{/if}
						{#if row.onsiteId}<div>OID <span class="text-text-primary">{row.onsiteId}</span></div>{/if}
					</td>
					<td class="py-2.5 pr-3 font-mono text-text-primary">{row.bookingNumber}</td>
					<td class="py-2.5 pr-3 text-text-primary truncate max-w-[220px]">
						<div>{row.route.join(' → ') || '—'}</div>
						{#if row.vehicleType}<div class="text-[10px] text-text-muted">{row.vehicleType}</div>{/if}
					</td>
					<td class="py-2.5 pr-3 font-mono text-text-muted whitespace-nowrap">
						{row.pickupTime ? formatDate(row.pickupTime) : '—'}
					</td>
					<td class="py-2.5 pr-3 whitespace-nowrap">
						<CountdownBadge target={row.deadlineAt} size="lg" />
						<div class="mt-0.5"><CountdownBadge target={row.deadlineAt} size="sm" /></div>
					</td>
					<td class="py-2.5 pr-3">
						<span class="inline-flex flex-wrap gap-1">
							<span class="text-[10px] px-1.5 py-0.5 rounded bg-live/10 text-live uppercase font-semibold">
								{row.bookingType === 'coc' ? 'COC' : 'REG'}
							</span>
							{#if tripTypeLabel(row.tripType)}
								<span class="text-[10px] px-1.5 py-0.5 rounded bg-accent/10 text-accent uppercase font-semibold">
									{tripTypeLabel(row.tripType)}
								</span>
							{/if}
						</span>
					</td>
					<td class="py-2.5 pr-3">
						<span class="inline-flex items-center gap-1.5">
							<span aria-hidden="true" class="w-1.5 h-1.5 rounded-full shrink-0 {statusDotClass(row.status)}"></span>
							<span class="text-text-primary">{statusLabel(row.status)}</span>
						</span>
					</td>
					<td class="py-2.5 pr-3 text-text-muted">—</td>
					<td class="py-2.5 pr-3">
						{#if row.status === 'pending'}
							<button
								type="button"
								disabled={row.accepting}
								onclick={(e) => {
									e.stopPropagation();
									onAccept(row);
								}}
								class="min-h-[44px] min-w-[44px] px-2.5 rounded-md text-[11px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
							>
								{row.accepting ? 'Memproses…' : 'Terima'}
							</button>
						{/if}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>

	<!-- Mobile: stacked cards, same information, visible field labels (column position is lost
	     once collapsed, so labels carry the meaning instead). -->
	<ul class="md:hidden flex flex-col gap-2">
		{#each rows as row (row.id)}
			<li class="p-3 rounded-lg border border-border bg-bg-surface flex flex-col gap-1.5">
				<!-- Non-interactive <li> wraps two SIBLING controls: this role="button" div (opens
				     detail) and the "Terima" button below. Neither nests inside the other — a real
				     <button> inside a role="button" element is an ARIA anti-pattern (ambiguous
				     focus/tab-stop semantics), and it also lets the button's keydown bubble into the
				     outer handler (Enter would get preventDefault'd before the button's native
				     activation runs; Space would double-fire both actions). Keeping them siblings
				     avoids that bug class entirely rather than needing to guard against it. -->
				<div
					role="button"
					tabindex="0"
					onclick={() => onRowClick(row)}
					onkeydown={(e) => {
						if (e.key === 'Enter' || e.key === ' ') {
							e.preventDefault();
							onRowClick(row);
						}
					}}
					class="w-full text-left flex flex-col gap-1.5 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					<div class="flex items-center justify-between">
						<span class="inline-flex items-center gap-1.5 text-[12px]">
							<span aria-hidden="true" class="w-1.5 h-1.5 rounded-full shrink-0 {statusDotClass(row.status)}"></span>
							<span class="text-text-primary">{statusLabel(row.status)}</span>
						</span>
						<span class="font-mono text-[11px] text-text-muted">{row.spxId}</span>
					</div>
					<div class="text-[12px] text-text-primary">{row.route.join(' → ') || '—'}</div>
					<div class="text-[11px] text-text-muted">{row.bookingNumber}</div>
					{#if row.vehicleType}<div class="text-[11px] text-text-muted">{row.vehicleType}</div>{/if}
					{#if row.requestId}<div class="font-mono text-[11px] text-text-muted">REQ <span class="text-text-primary">{row.requestId}</span></div>{/if}
					{#if row.onsiteId}<div class="font-mono text-[11px] text-text-muted">OID <span class="text-text-primary">{row.onsiteId}</span></div>{/if}
					<div class="flex items-center gap-2">
						<CountdownBadge target={row.deadlineAt} size="lg" />
						<span class="text-[10px] px-1.5 py-0.5 rounded bg-live/10 text-live uppercase font-semibold">
							{row.bookingType === 'coc' ? 'COC' : 'REG'}
						</span>
						{#if tripTypeLabel(row.tripType)}
							<span class="text-[10px] px-1.5 py-0.5 rounded bg-accent/10 text-accent uppercase font-semibold">
								{tripTypeLabel(row.tripType)}
							</span>
						{/if}
					</div>
					<div class="font-mono text-[10px] text-text-muted">{row.pickupTime ? formatDate(row.pickupTime) : '—'}</div>
					<div class="flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-text-muted">
						<span>Layanan: {row.serviceType ?? '—'}</span>
						<span>Berat: {row.weight.toFixed(1)} kg</span>
						{#if row.codAmount > 0}<span>COD: {row.codAmount.toLocaleString('id-ID')}</span>{/if}
					</div>
					<div class="font-mono text-[10px] text-text-muted">{formatDate(row.createdAt)}</div>
				</div>
				{#if row.status === 'pending'}
					<button
						type="button"
						disabled={row.accepting}
						onclick={() => onAccept(row)}
						class="mt-1 min-h-[44px] w-full rounded-md text-[12px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{row.accepting ? 'Memproses…' : 'Terima'}
					</button>
				{/if}
			</li>
		{/each}
	</ul>
{/if}
