<!-- Separate from ChipInput: this needs remote-list search (native <input list> + <datalist> —
     no hand-rolled ARIA listbox needed, ponytail: native platform feature covers it), inline
     "create new location" when the typed name matches nothing, single/multi selection, and
     (multi only) capped count + explicit reorder. Materially different interaction from a plain
     chip list, so it is its own component rather than a ChipInput variant. -->
<script lang="ts">
	import { X, ChevronUp, ChevronDown, Plus } from '@lucide/svelte';
	import type { LocationItem } from '$lib/api-rules';

	let {
		label,
		locations,
		value,
		onChange,
		onCreateLocation,
		multi = false,
		max
	}: {
		label: string;
		locations: LocationItem[];
		value: string[];
		onChange: (value: string[]) => void;
		onCreateLocation: (name: string) => Promise<LocationItem>;
		multi?: boolean;
		max?: number;
	} = $props();

	let draft = $state('');
	let creating = $state(false);
	let errorMsg = $state('');
	const listId = `location-list-${crypto.randomUUID()}`;

	async function commit() {
		const trimmed = draft.trim();
		if (trimmed === '') return;
		errorMsg = '';
		if (
			multi &&
			(value.some((v) => v.toLowerCase() === trimmed.toLowerCase()) ||
				(max !== undefined && value.length >= max))
		) {
			draft = '';
			return;
		}

		const existing = locations.find((l) => l.name.toLowerCase() === trimmed.toLowerCase());
		if (existing) {
			onChange(multi ? [...value, existing.name] : [existing.name]);
			draft = '';
			return;
		}

		creating = true;
		try {
			const created = await onCreateLocation(trimmed);
			onChange(multi ? [...value, created.name] : [created.name]);
			draft = '';
		} catch {
			errorMsg = `Gagal menambah lokasi "${trimmed}".`;
		} finally {
			creating = false;
		}
	}

	function remove(name: string) {
		onChange(value.filter((v) => v !== name));
	}

	function moveUp(index: number) {
		if (index === 0) return;
		const next = [...value];
		[next[index - 1], next[index]] = [next[index], next[index - 1]];
		onChange(next);
	}

	function moveDown(index: number) {
		if (index === value.length - 1) return;
		const next = [...value];
		[next[index], next[index + 1]] = [next[index + 1], next[index]];
		onChange(next);
	}

	const atMax = $derived(multi && max !== undefined && value.length >= max);
	const showInput = $derived(multi ? !atMax : value.length === 0);
</script>

<div class="flex flex-col gap-1.5">
	<span id={`${listId}-label`} class="text-[10px] font-body text-text-muted uppercase tracking-wide">{label}</span>

	{#if value.length > 0}
		<ol class="flex flex-col gap-1">
			{#each value as name, i (name)}
				<li
					class="flex items-center gap-1.5 min-h-[36px] pl-2.5 pr-1.5 rounded-md text-[12px] font-body bg-bg-base border border-border text-text-primary"
				>
					<span class="flex-1">{name}</span>
					{#if multi}
						<button
							type="button"
							onclick={() => moveUp(i)}
							disabled={i === 0}
							aria-label={`Naikkan ${name}`}
							class="min-h-[28px] min-w-[28px] flex items-center justify-center rounded text-text-muted hover:text-text-primary disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							<ChevronUp size={12} aria-hidden="true" />
						</button>
						<button
							type="button"
							onclick={() => moveDown(i)}
							disabled={i === value.length - 1}
							aria-label={`Turunkan ${name}`}
							class="min-h-[28px] min-w-[28px] flex items-center justify-center rounded text-text-muted hover:text-text-primary disabled:opacity-30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							<ChevronDown size={12} aria-hidden="true" />
						</button>
					{/if}
					<button
						type="button"
						onclick={() => remove(name)}
						aria-label={`Hapus ${name}`}
						class="min-h-[28px] min-w-[28px] flex items-center justify-center rounded text-text-muted hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						<X size={12} aria-hidden="true" />
					</button>
				</li>
			{/each}
		</ol>
	{/if}

	{#if showInput}
		<div class="flex items-center gap-1.5">
			<input
				list={listId}
				type="text"
				bind:value={draft}
				disabled={creating}
				onkeydown={(e) => {
					if (e.key === 'Enter') {
						e.preventDefault();
						commit();
					}
				}}
				aria-labelledby={`${listId}-label`}
				placeholder="Cari atau tambah lokasi"
				class="min-h-[36px] flex-1 px-2.5 rounded-md text-[12px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			<datalist id={listId}>
				{#each locations as loc (loc.id)}
					<option value={loc.name}></option>
				{/each}
			</datalist>
			<button
				type="button"
				onclick={commit}
				disabled={creating || draft.trim() === ''}
				aria-label="Tambah lokasi"
				class="min-h-[36px] min-w-[36px] flex items-center justify-center rounded-md border border-border text-text-muted hover:text-text-primary disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<Plus size={14} aria-hidden="true" />
			</button>
		</div>
	{/if}

	{#if errorMsg}
		<p role="alert" aria-live="polite" class="text-[11px] text-danger">{errorMsg}</p>
	{/if}
</div>
