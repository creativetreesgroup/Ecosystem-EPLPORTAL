<!-- Frontend/src/lib/components/PriceRow.svelte -->
<!-- One route_prices row: collapsed summary + expand-in-place editor. Unlike RuleRow.svelte
     (Fase 7d), this component owns its own local edit-in-progress state and persists on its own
     explicit Save action (createPrice/updatePrice) — /price's backend is genuine per-resource
     CRUD, not a replace-all model, so there is no page-level batch Save to propagate edits into. -->
<script lang="ts">
	import { Trash2 } from '@lucide/svelte';
	import { formatRupiah, priceDraftIsValid, type PriceDraft } from '$lib/prices';
	import { createPrice, updatePrice, deletePrice } from '$lib/api-prices';
	import { ApiError } from '$lib/api';
	import type { LocationItem } from '$lib/api-rules';
	import ChipInput from './ChipInput.svelte';
	import LocationCombobox from './LocationCombobox.svelte';

	const SERVICE_TYPE_OPTIONS = ['TRONTON', 'FUSO', 'CDD LONG', 'CDE LONG', 'BLINDVAN', 'WINGBOX', 'ENGKEL', '40FCL'];

	let {
		draft,
		locations,
		onCreateLocation,
		onSaved,
		onRemove,
		readOnly
	}: {
		draft: PriceDraft;
		locations: LocationItem[];
		onCreateLocation: (name: string) => Promise<LocationItem>;
		onSaved: (saved: PriceDraft) => void;
		onRemove: () => void;
		readOnly: boolean;
	} = $props();

	// svelte-ignore state_referenced_locally -- deliberate one-time snapshot: `local` is this
	// component's own edit-in-progress copy, seeded once from `draft` and never resynced from it.
	let local = $state<PriceDraft>({ ...draft });
	// Reactive to local.id, NOT computed once from the `draft` prop — after a successful create,
	// `local.id` is updated to the server-assigned id (see save() below) and this must flip to
	// false immediately, or a subsequent edit+save on the SAME now-persisted row would call
	// createPrice again instead of updatePrice (duplicate row / spurious 409).
	const isNew = $derived(local.id === null);
	// svelte-ignore state_referenced_locally -- deliberate one-time snapshot of local.id at mount:
	// a NEW row starts expanded; toggling stays independent of isNew afterward (user can collapse
	// a new row, and a just-saved row must not auto-collapse/expand from this line ever again).
	let expanded = $state(local.id === null);
	let saving = $state(false);
	let deleting = $state(false);
	let errorMsg = $state('');

	function updateLocal(patch: Partial<PriceDraft>) {
		local = { ...local, ...patch };
	}

	const summary = $derived(
		`${local.origin || '—'} → ${local.destinations.length > 0 ? local.destinations.join(' → ') : '—'} · ${formatRupiah(local.price)}`
	);

	async function save() {
		if (!priceDraftIsValid(local)) {
			errorMsg = 'Lengkapi semua field yang wajib diisi (kode rute, asal, min. 1 tujuan, jenis kendaraan, harga > 0).';
			return;
		}
		saving = true;
		errorMsg = '';
		try {
			const saved = isNew ? await createPrice(local) : await updatePrice(local.id as string, local);
			// Adopt the server-confirmed id/fields, but keep OUR OWN clientKey stable — the parent
			// keys its {#each} on clientKey, and preserving it here (rather than taking whatever
			// fresh clientKey api-prices.ts's priceOutputToDraft generated) avoids an unnecessary
			// remount of this component on every successful save.
			local = { ...saved, clientKey: local.clientKey };
			onSaved(local);
		} catch (e) {
			if (e instanceof ApiError && e.status === 409) {
				errorMsg = 'Kode rute sudah dipakai.';
			} else {
				errorMsg = 'Gagal menyimpan. Coba lagi.';
			}
		} finally {
			saving = false;
		}
	}

	async function del() {
		if (!confirm(`Hapus harga untuk rute "${local.routeCode}"?`)) return;
		deleting = true;
		errorMsg = '';
		try {
			await deletePrice(local.id as string);
			onRemove();
		} catch {
			errorMsg = 'Gagal menghapus. Coba lagi.';
			deleting = false;
		}
	}
</script>

<div class="rounded-lg border border-border bg-bg-surface">
	<div class="flex items-center gap-3 p-3">
		<div
			role="button"
			tabindex="0"
			aria-expanded={expanded}
			onclick={() => (expanded = !expanded)}
			onkeydown={(e) => {
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					expanded = !expanded;
				}
			}}
			class="flex-1 flex flex-col gap-0.5 cursor-pointer rounded-md px-1 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			<div class="flex items-center gap-2">
				<span class="text-[13px] font-heading font-medium text-text-primary">{local.routeCode || 'Rute baru'}</span>
				{#if local.region}
					<span class="text-[10px] font-body px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-muted">
						{local.region}
					</span>
				{/if}
			</div>
			<span class="text-[11px] font-mono text-text-muted">{summary}</span>
		</div>

		{#if !readOnly}
			<button
				type="button"
				onclick={del}
				disabled={deleting || isNew}
				aria-label={`Hapus harga rute ${local.routeCode || 'baru'}`}
				class="min-h-[36px] min-w-[36px] flex items-center justify-center rounded text-text-muted hover:text-danger disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<Trash2 size={14} aria-hidden="true" />
			</button>
		{/if}
	</div>

	{#if expanded}
		<fieldset disabled={readOnly} class="flex flex-col gap-3 p-3 pt-0 border-0">
			{#if errorMsg}
				<p role="alert" aria-live="polite" class="text-[12px] text-danger">{errorMsg}</p>
			{/if}

			<div class="grid grid-cols-2 gap-3">
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Kode Rute</span>
					<input
						type="text"
						value={local.routeCode}
						oninput={(e) => updateLocal({ routeCode: (e.target as HTMLInputElement).value })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Region</span>
					<input
						type="text"
						value={local.region}
						oninput={(e) => updateLocal({ region: (e.target as HTMLInputElement).value })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>

			<LocationCombobox
				label="Asal"
				{locations}
				{onCreateLocation}
				value={local.origin ? [local.origin] : []}
				onChange={(v) => updateLocal({ origin: v[0] ?? '' })}
			/>

			<LocationCombobox
				label="Tujuan (maks 5)"
				{locations}
				{onCreateLocation}
				value={local.destinations}
				onChange={(v) => updateLocal({ destinations: v })}
				multi
				max={5}
			/>

			<ChipInput
				label="Jenis Kendaraan"
				value={local.vehicleType ? [local.vehicleType] : []}
				onChange={(v) => updateLocal({ vehicleType: v[0] ?? '' })}
				options={SERVICE_TYPE_OPTIONS.map((v) => ({ value: v, label: v }))}
				multi={false}
			/>

			<label class="flex flex-col gap-1 max-w-[200px]">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Harga (Rp)</span>
				<input
					type="number"
					min="0"
					value={local.price}
					oninput={(e) => updateLocal({ price: Number((e.target as HTMLInputElement).value) || 0 })}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-mono bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<div class="flex gap-2">
				<button
					type="button"
					onclick={save}
					disabled={saving}
					class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					{saving ? 'Menyimpan…' : 'Simpan'}
				</button>
				{#if isNew}
					<button
						type="button"
						onclick={onRemove}
						class="min-h-[44px] px-4 rounded-md text-[13px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						Batal
					</button>
				{/if}
			</div>
		</fieldset>
	{/if}
</div>
