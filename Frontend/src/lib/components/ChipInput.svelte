<!-- Generic chip list: free-text entry (no `options` prop — e.g. booking_ids) or closed-vocabulary
     multi-select (`options` prop — e.g. service_types, shift_types, trip_types). Both render/remove
     chips identically; only whether arbitrary text is accepted differs, so one small prop covers
     both rather than two near-duplicate components. Numeric vocabularies (shift/trip types) are the
     caller's responsibility to map to/from string — this component only ever holds `string[]`. -->
<script lang="ts">
	import { X } from '@lucide/svelte';

	let {
		label,
		value,
		onChange,
		options,
		multi = true
	}: {
		label: string;
		value: string[];
		onChange: (value: string[]) => void;
		options?: { value: string; label: string }[];
		multi?: boolean;
	} = $props();

	let draft = $state('');
	const inputId = `chip-input-${crypto.randomUUID()}`;

	function addFreeText() {
		const trimmed = draft.trim();
		if (trimmed !== '' && !value.includes(trimmed)) {
			onChange([...value, trimmed]);
		}
		draft = '';
	}

	function remove(item: string) {
		onChange(value.filter((v) => v !== item));
	}

	function toggleOption(optValue: string) {
		if (multi) {
			if (value.includes(optValue)) {
				onChange(value.filter((v) => v !== optValue));
			} else {
				onChange([...value, optValue]);
			}
		} else {
			// Single-select: clicking the already-selected option clears it (allows a genuine
			// "nothing chosen yet" state); clicking any other option replaces the selection.
			onChange(value.includes(optValue) ? [] : [optValue]);
		}
	}
</script>

<div class="flex flex-col gap-1.5">
	<span id={`${inputId}-label`} class="text-[10px] font-body text-text-muted uppercase tracking-wide">{label}</span>

	{#if options}
		<div
			class="flex flex-wrap gap-1.5"
			role={multi ? 'group' : 'radiogroup'}
			aria-labelledby={`${inputId}-label`}
		>
			{#each options as opt (opt.value)}
				<button
					type="button"
					role={multi ? undefined : 'radio'}
					aria-pressed={multi ? value.includes(opt.value) : undefined}
					aria-checked={multi ? undefined : value.includes(opt.value)}
					onclick={() => toggleOption(opt.value)}
					class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
						value.includes(opt.value)
							? 'bg-accent text-bg-base border-accent'
							: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
					}`}
				>
					{opt.label}
				</button>
			{/each}
		</div>
	{:else}
		<div class="flex flex-wrap items-center gap-1.5">
			{#each value as item (item)}
				<span
					class="flex items-center gap-1 min-h-[32px] pl-2.5 pr-1.5 rounded-md text-[12px] font-mono bg-bg-base border border-border text-text-primary"
				>
					{item}
					<button
						type="button"
						onclick={() => remove(item)}
						aria-label={`Hapus ${item}`}
						class="min-h-[24px] min-w-[24px] flex items-center justify-center rounded text-text-muted hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						<X size={12} aria-hidden="true" />
					</button>
				</span>
			{/each}
			<input
				id={inputId}
				type="text"
				bind:value={draft}
				aria-labelledby={`${inputId}-label`}
				onkeydown={(e) => {
					if (e.key === 'Enter') {
						e.preventDefault();
						addFreeText();
					}
				}}
				placeholder="Ketik lalu Enter"
				class="min-h-[36px] px-2.5 rounded-md text-[12px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</div>
	{/if}
</div>
