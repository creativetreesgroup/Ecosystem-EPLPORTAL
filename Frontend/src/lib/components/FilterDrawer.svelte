<!-- Frontend/src/lib/components/FilterDrawer.svelte -->
<script lang="ts">
	// Slide-in panel replacing TicketFilterBar's inline row — has far more fields than fit inline,
	// and needs a REAL multi-element focus trap (many interactive fields, not a single button like
	// AutoAcceptSwitch.svelte's OTP modal), broadened here to also include <select> as a focusable
	// element type.
	import { onMount } from 'svelte';
	import { X } from '@lucide/svelte';
	import type { TicketFilters, TicketStatus } from '$lib/tickets';
	import { EMPTY_TICKET_FILTERS } from '$lib/tickets';
	import { fetchVehicleTypes } from '$lib/api-command';
	import { fetchLocations, type LocationItem } from '$lib/api-rules';

	let {
		open,
		filters,
		onFiltersChange,
		onClose,
		resultCount
	}: {
		open: boolean;
		filters: TicketFilters;
		onFiltersChange: (f: TicketFilters) => void;
		onClose: () => void;
		resultCount: number;
	} = $props();

	let dialogEl: HTMLDivElement | undefined = $state();
	let previouslyFocusedEl: HTMLElement | null = null;
	let vehicleTypes = $state<string[]>([]);
	let locations = $state<LocationItem[]>([]);

	onMount(() => {
		fetchVehicleTypes()
			.then((types) => (vehicleTypes = types))
			.catch(() => (vehicleTypes = []));
		fetchLocations()
			.then((locs) => (locations = locs))
			.catch(() => (locations = []));
	});

	$effect(() => {
		if (open) {
			previouslyFocusedEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			dialogEl
				?.querySelector<HTMLElement>('input:not([disabled]), select:not([disabled]), button:not([disabled])')
				?.focus();
		} else if (previouslyFocusedEl) {
			previouslyFocusedEl.focus();
			previouslyFocusedEl = null;
		}
	});

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			onClose();
			return;
		}
		if (e.key !== 'Tab' || !dialogEl) return;
		const focusables = Array.from(
			dialogEl.querySelectorAll<HTMLElement>('input:not([disabled]), select:not([disabled]), button:not([disabled])')
		);
		if (focusables.length === 0) return;
		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		if (e.shiftKey && document.activeElement === first) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && document.activeElement === last) {
			e.preventDefault();
			first.focus();
		}
	}

	function set<K extends keyof TicketFilters>(key: K, value: TicketFilters[K]) {
		onFiltersChange({ ...filters, [key]: value });
	}

	function resetAll() {
		onFiltersChange({ ...EMPTY_TICKET_FILTERS });
	}

	const STATUS_OPTIONS: { value: TicketStatus | null; label: string }[] = [
		{ value: null, label: 'Semua status' },
		{ value: 'pending', label: 'Pending (live)' },
		{ value: 'accepted', label: 'Diterima' },
		{ value: 'failed', label: 'Gagal' }
	];
</script>

{#if open}
	<div class="fixed inset-0 z-40 bg-black/50" onclick={onClose} aria-hidden="true"></div>
	<div
		bind:this={dialogEl}
		onkeydown={handleKeydown}
		role="dialog"
		aria-modal="true"
		aria-labelledby="filter-drawer-title"
		tabindex="-1"
		class="fixed inset-y-0 right-0 z-50 w-full max-w-sm overflow-y-auto bg-bg-surface border-l border-border p-4 flex flex-col gap-4"
	>
		<div class="flex items-center justify-between">
			<h2 id="filter-drawer-title" class="font-heading font-semibold text-text-primary text-[14px]">Filter Lanjutan</h2>
			<button
				type="button"
				onclick={onClose}
				aria-label="Tutup filter"
				class="min-h-[44px] min-w-[44px] flex items-center justify-center rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<X size={18} aria-hidden="true" />
			</button>
		</div>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted">
			Urutkan
			<select
				value={filters.sort}
				onchange={(e) => set('sort', (e.target as HTMLSelectElement).value as TicketFilters['sort'])}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="newest">Terbaru masuk</option>
				<option value="deadline_soonest">Deadline terdekat</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-request-id">
			ID Request
			<input
				id="filter-request-id"
				type="text"
				value={filters.requestId}
				oninput={(e) => set('requestId', (e.target as HTMLInputElement).value)}
				placeholder="cth. FMR-..."
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-booking-name">
			Nama Booking
			<input
				id="filter-booking-name"
				type="text"
				value={filters.bookingName}
				oninput={(e) => set('bookingName', (e.target as HTMLInputElement).value)}
				placeholder="cth. SPXID-JKT..."
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-armada">
			Armada
			<select
				id="filter-armada"
				value={filters.vehicleType ?? ''}
				onchange={(e) => set('vehicleType', (e.target as HTMLSelectElement).value || null)}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua armada</option>
				{#each vehicleTypes as vt (vt)}
					<option value={vt}>{vt}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-origin-station">
			Station Keberangkatan
			<select
				id="filter-origin-station"
				value={filters.originStation ?? ''}
				onchange={(e) => set('originStation', (e.target as HTMLSelectElement).value || null)}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua station</option>
				{#each locations as loc (loc.id)}
					<option value={loc.name}>{loc.name}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-dest-station">
			Station Tujuan
			<select
				id="filter-dest-station"
				value={filters.destStation ?? ''}
				onchange={(e) => set('destStation', (e.target as HTMLSelectElement).value || null)}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua tujuan</option>
				{#each locations as loc (loc.id)}
					<option value={loc.name}>{loc.name}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-tag">
			Tag Tiket
			<select
				id="filter-tag"
				value={filters.bookingType ?? (filters.tripType !== null ? `trip:${filters.tripType}` : '')}
				onchange={(e) => {
					const v = (e.target as HTMLSelectElement).value;
					if (v === '') {
						onFiltersChange({ ...filters, bookingType: null, tripType: null });
					} else if (v.startsWith('trip:')) {
						onFiltersChange({ ...filters, bookingType: null, tripType: Number(v.slice(5)) });
					} else {
						onFiltersChange({ ...filters, bookingType: v as 'coc' | 'reguler', tripType: null });
					}
				}}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua tag</option>
				<option value="coc">COC</option>
				<option value="reguler">REG</option>
				<option value="trip:0">ADHOC</option>
				<option value="trip:1">FIX</option>
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-status">
			Status
			<select
				id="filter-status"
				value={filters.status ?? ''}
				onchange={(e) => set('status', ((e.target as HTMLSelectElement).value || null) as TicketStatus | null)}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{#each STATUS_OPTIONS as opt (opt.value ?? 'all')}
					<option value={opt.value ?? ''}>{opt.label}</option>
				{/each}
			</select>
		</label>

		<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-cod">
			COD
			<select
				id="filter-cod"
				value={filters.cod === null ? '' : String(filters.cod)}
				onchange={(e) => {
					const v = (e.target as HTMLSelectElement).value;
					set('cod', v === '' ? null : v === 'true');
				}}
				class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<option value="">Semua</option>
				<option value="true">Ya</option>
				<option value="false">Tidak</option>
			</select>
		</label>

		<div class="grid grid-cols-2 gap-2">
			<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-weight-min">
				Berat Min (KG)
				<input
					id="filter-weight-min"
					type="number"
					value={filters.weightMin ?? ''}
					oninput={(e) => {
						const v = (e.target as HTMLInputElement).value;
						set('weightMin', v === '' ? null : Number(v));
					}}
					placeholder="0"
					class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			<label class="flex flex-col gap-1 text-[11px] text-text-muted" for="filter-weight-max">
				Berat Maks (KG)
				<input
					id="filter-weight-max"
					type="number"
					value={filters.weightMax ?? ''}
					oninput={(e) => {
						const v = (e.target as HTMLInputElement).value;
						set('weightMax', v === '' ? null : Number(v));
					}}
					placeholder="∞"
					class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
		</div>

		<fieldset class="flex flex-col gap-2">
			<legend class="text-[11px] text-text-muted uppercase tracking-wide">Periode / Waktu Booking</legend>
			<div class="grid grid-cols-2 gap-2">
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-pickup-from">
					Dari
					<input
						id="filter-pickup-from"
						type="date"
						value={filters.pickupFrom ? filters.pickupFrom.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('pickupFrom', v ? new Date(v).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-pickup-to">
					Sampai
					<input
						id="filter-pickup-to"
						type="date"
						value={filters.pickupTo ? filters.pickupTo.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('pickupTo', v ? new Date(`${v}T23:59:59.999Z`).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>
		</fieldset>

		<fieldset class="flex flex-col gap-2">
			<legend class="text-[11px] text-text-muted uppercase tracking-wide">Batas Waktu Konfirmasi (Deadline)</legend>
			<div class="grid grid-cols-2 gap-2">
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-deadline-from">
					Dari
					<input
						id="filter-deadline-from"
						type="date"
						value={filters.deadlineFrom ? filters.deadlineFrom.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('deadlineFrom', v ? new Date(v).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1 text-[10px] text-text-muted" for="filter-deadline-to">
					Sampai
					<input
						id="filter-deadline-to"
						type="date"
						value={filters.deadlineTo ? filters.deadlineTo.slice(0, 10) : ''}
						onchange={(e) => {
							const v = (e.target as HTMLInputElement).value;
							set('deadlineTo', v ? new Date(`${v}T23:59:59.999Z`).toISOString() : null);
						}}
						class="min-h-[44px] px-2.5 rounded-md text-[13px] bg-bg-base border border-border text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>
		</fieldset>

		<div class="mt-auto flex items-center justify-between gap-3 pt-3 border-t border-border">
			<span class="text-[11px] text-text-muted">{resultCount} tiket cocok</span>
			<div class="flex gap-2">
				<button
					type="button"
					onclick={resetAll}
					class="min-h-[44px] px-3 rounded-md text-[12px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					Reset Semua
				</button>
				<button
					type="button"
					onclick={onClose}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-bold bg-accent text-bg-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					Selesai
				</button>
			</div>
		</div>
	</div>
{/if}
