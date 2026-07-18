<!-- Frontend/src/routes/(app)/price/+page.svelte -->
<!-- /price: client-side filter + pagination over the full tenant price list. Each row persists
     independently (PriceRow's own Save/Delete) — no page-level dirty-tracking or batch Save,
     unlike /rules, since /price's backend is genuine per-resource REST. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchPrices } from '$lib/api-prices';
	import { fetchLocations, createLocation, type LocationItem } from '$lib/api-rules';
	import { newPriceDraft, matchesFilter, type PriceDraft } from '$lib/prices';
	import PriceRow from '$lib/components/PriceRow.svelte';
	import Pagination from '$lib/components/Pagination.svelte';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	const PAGE_SIZE = 20;

	let rows = $state<PriceDraft[]>([]);
	let locations = $state<LocationItem[]>([]);
	let loading = $state(true);
	let errorMsg = $state('');
	let query = $state('');
	let page = $state(1);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const [prices, locs] = await Promise.all([fetchPrices(), fetchLocations()]);
			rows = prices;
			locations = locs;
		} catch {
			errorMsg = 'Gagal memuat daftar harga. Coba lagi.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	const filtered = $derived(rows.filter((r) => matchesFilter(r, query)));
	const pageCount = $derived(Math.max(1, Math.ceil(filtered.length / PAGE_SIZE)));
	const pageRows = $derived(filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE));
	const hasMore = $derived(page < pageCount);

	function handleQueryChange(next: string) {
		query = next;
		page = 1;
	}

	function addDraftRow() {
		rows = [newPriceDraft(), ...rows];
		query = '';
		page = 1;
	}

	function handleSaved(saved: PriceDraft) {
		rows = rows.map((r) => (r.clientKey === saved.clientKey ? saved : r));
	}

	function handleRemove(clientKey: string) {
		rows = rows.filter((r) => r.clientKey !== clientKey);
		const newPageCount = Math.max(1, Math.ceil(rows.filter((r) => matchesFilter(r, query)).length / PAGE_SIZE));
		if (page > newPageCount) page = newPageCount;
	}

	async function handleCreateLocation(name: string): Promise<LocationItem> {
		const created = await createLocation(name);
		locations = [...locations, created];
		return created;
	}
</script>

<svelte:head>
	<title>Harga — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Daftar Harga</h1>

	{#if readOnly}
		<div
			role="alert"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
		>
			Hanya akun utama yang dapat mengubah harga.
		</div>
	{/if}

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
		<div class="flex items-end gap-3">
			<label class="flex-1 flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Cari</span>
				<input
					type="text"
					value={query}
					oninput={(e) => handleQueryChange((e.target as HTMLInputElement).value)}
					placeholder="Kode rute, region, atau asal"
					class="min-h-[44px] px-3 rounded-md text-[13px] font-body bg-bg-surface border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			{#if !readOnly}
				<button
					type="button"
					onclick={addDraftRow}
					class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Tambah Harga
				</button>
			{/if}
		</div>

		<div class="flex flex-col gap-2">
			{#each pageRows as row (row.clientKey)}
				<PriceRow
					draft={row}
					{locations}
					onCreateLocation={handleCreateLocation}
					onSaved={handleSaved}
					onRemove={() => handleRemove(row.clientKey)}
					{readOnly}
				/>
			{/each}
		</div>

		<Pagination {page} {hasMore} onPageChange={(next) => (page = next)} />
	{/if}
</div>
