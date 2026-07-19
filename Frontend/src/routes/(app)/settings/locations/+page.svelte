<!-- Frontend/src/routes/(app)/settings/locations/+page.svelte -->
<!-- Flat add/delete management list for the tenant's known route locations. GET /locations has
     no permission gate (any authenticated session sees the real list), only POST/DELETE are
     main-account-gated — so this page is edit-gated like /settings/branding (always visible,
     controls individually disabled), never content-gated like /settings/bot. Deleting a location
     is genuinely safe (confirmed via schema: no other table references route_locations by id,
     accept_rules/route_prices store location names as plain text) — a native confirm() is
     sufficient, no "in use" warning needed. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchLocations, createLocation, deleteLocation, type LocationItem } from '$lib/api-locations';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let locations = $state<LocationItem[]>([]);
	let newName = $state('');
	let loading = $state(true);
	let adding = $state(false);
	let deletingId = $state<string | null>(null);
	let errorMsg = $state('');

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			locations = await fetchLocations();
		} catch {
			errorMsg = 'Gagal memuat lokasi.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function handleAdd() {
		const trimmed = newName.trim();
		if (trimmed === '') return;
		adding = true;
		errorMsg = '';
		try {
			const created = await createLocation(trimmed);
			locations = [...locations, created].sort((a, b) => a.name.localeCompare(b.name));
			newName = '';
		} catch (err) {
			errorMsg = err instanceof ApiError && err.status === 409 ? 'Lokasi ini sudah ada.' : 'Gagal menambah lokasi.';
		} finally {
			adding = false;
		}
	}

	async function handleDelete(location: LocationItem) {
		if (!confirm(`Hapus lokasi "${location.name}"?`)) return;
		deletingId = location.id;
		errorMsg = '';
		try {
			await deleteLocation(location.id);
			locations = locations.filter((l) => l.id !== location.id);
		} catch {
			errorMsg = 'Gagal menghapus lokasi.';
		} finally {
			deletingId = null;
		}
	}
</script>

<svelte:head>
	<title>Lokasi — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}

		<fieldset disabled={readOnly} class="flex gap-2 border-0 p-0">
			<label class="sr-only" for="new-location-name">Nama lokasi baru</label>
			<input
				id="new-location-name"
				type="text"
				bind:value={newName}
				onkeydown={(e) => e.key === 'Enter' && !adding && handleAdd()}
				placeholder="Nama lokasi baru"
				class="flex-1 min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			<button
				type="button"
				onclick={handleAdd}
				disabled={adding || newName.trim() === ''}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{adding ? 'Menambah…' : 'Tambah'}
			</button>
		</fieldset>

		{#if locations.length === 0}
			<p class="text-[13px] text-text-muted">Belum ada lokasi.</p>
		{:else}
			<ul class="flex flex-col gap-2">
				{#each locations as location (location.id)}
					<li class="flex items-center justify-between gap-2 rounded-lg border border-border bg-bg-surface p-3">
						<span class="text-[13px] font-body text-text-primary">{location.name}</span>
						<button
							type="button"
							disabled={readOnly || deletingId === location.id}
							onclick={() => handleDelete(location)}
							class="min-h-[36px] px-2 text-[11px] text-danger disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							{deletingId === location.id ? 'Menghapus…' : 'Hapus'}
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
