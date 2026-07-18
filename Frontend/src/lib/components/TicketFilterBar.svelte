<!-- Frontend/src/lib/components/TicketFilterBar.svelte -->
<!-- Status/SPX-ID/date-range filter row for the /tickets full-management view. Controlled —
     holds no filter state itself, just reflects `filters` and calls `onFiltersChange` with a
     new object (never mutates), matching $lib/tickets.ts's own no-mutation convention. -->
<script lang="ts">
	import { Search, X } from '@lucide/svelte';
	import type { TicketFilters, TicketStatus } from '$lib/tickets';

	let { filters, onFiltersChange }: { filters: TicketFilters; onFiltersChange: (f: TicketFilters) => void } =
		$props();

	const STATUS_OPTIONS: { value: TicketStatus | null; label: string }[] = [
		{ value: null, label: 'Semua status' },
		{ value: 'pending', label: 'Pending' },
		{ value: 'accepted', label: 'Diterima' },
		{ value: 'failed', label: 'Gagal' }
	];

	function updateStatus(e: Event) {
		const value = (e.target as HTMLSelectElement).value || null;
		onFiltersChange({ ...filters, status: value as TicketStatus | null });
	}

	function updateSpxId(e: Event) {
		onFiltersChange({ ...filters, spxId: (e.target as HTMLInputElement).value });
	}

	function updateFrom(e: Event) {
		const raw = (e.target as HTMLInputElement).value;
		onFiltersChange({ ...filters, from: raw ? new Date(raw).toISOString() : null });
	}

	function updateTo(e: Event) {
		const raw = (e.target as HTMLInputElement).value;
		// End-of-day (23:59:59.999 UTC), NOT start-of-day like updateFrom — the backend applies
		// `created_at <= $to` (an inclusive upper bound), so a midnight-start timestamp would only
		// match bookings created at exactly that instant and silently exclude every booking
		// created during the rest of the picked day (whole-branch review finding).
		onFiltersChange({ ...filters, to: raw ? new Date(`${raw}T23:59:59.999Z`).toISOString() : null });
	}

	function clearAll() {
		onFiltersChange({ status: null, spxId: '', from: null, to: null });
	}

	const hasActiveFilters = $derived(
		filters.status !== null || filters.spxId !== '' || filters.from !== null || filters.to !== null
	);
</script>

<div class="flex flex-wrap items-end gap-3 p-3 rounded-lg border border-border bg-bg-surface">
	<div class="flex flex-col gap-1">
		<label for="ticket-filter-status" class="text-[10px] font-body text-text-muted uppercase tracking-wide"
			>Status</label
		>
		<select
			id="ticket-filter-status"
			value={filters.status ?? ''}
			onchange={updateStatus}
			class="min-h-[44px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			{#each STATUS_OPTIONS as opt (opt.value ?? 'all')}
				<option value={opt.value ?? ''}>{opt.label}</option>
			{/each}
		</select>
	</div>

	<div class="flex flex-col gap-1">
		<label for="ticket-filter-spxid" class="text-[10px] font-body text-text-muted uppercase tracking-wide"
			>SPX ID</label
		>
		<div class="relative">
			<Search size={14} aria-hidden="true" class="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
			<input
				id="ticket-filter-spxid"
				type="text"
				value={filters.spxId}
				oninput={updateSpxId}
				placeholder="Cari SPX ID"
				class="min-h-[44px] pl-8 pr-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</div>
	</div>

	<div class="flex flex-col gap-1">
		<label for="ticket-filter-from" class="text-[10px] font-body text-text-muted uppercase tracking-wide">Dari</label>
		<input
			id="ticket-filter-from"
			type="date"
			value={filters.from ? filters.from.slice(0, 10) : ''}
			onchange={updateFrom}
			class="min-h-[44px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		/>
	</div>

	<div class="flex flex-col gap-1">
		<label for="ticket-filter-to" class="text-[10px] font-body text-text-muted uppercase tracking-wide">Sampai</label>
		<input
			id="ticket-filter-to"
			type="date"
			value={filters.to ? filters.to.slice(0, 10) : ''}
			onchange={updateTo}
			class="min-h-[44px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		/>
	</div>

	{#if hasActiveFilters}
		<button
			type="button"
			onclick={clearAll}
			class="min-h-[44px] flex items-center gap-1.5 px-3 rounded-md text-[12px] font-body text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			<X size={14} aria-hidden="true" />
			Hapus filter
		</button>
	{/if}
</div>
