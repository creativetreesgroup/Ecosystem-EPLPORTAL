<!-- Frontend/src/lib/components/RuleRow.svelte -->
<!-- One AcceptRule: collapsed summary row + expand-in-place editor. Mode selector swaps the
     visible field set (booking_id: just booking IDs; route: origin/destinations/match_mode;
     filter: neither) on top of the shared fields every mode has. All fields disabled at once via
     a native <fieldset disabled> when readOnly — cascades through ChipInput/LocationCombobox's
     own internal <input>/<button> elements regardless of component boundaries, so no per-field
     readOnly prop threading is needed. -->
<script lang="ts">
	import { Trash2 } from '@lucide/svelte';
	import {
		conditionSummary,
		ruleIsEmpty,
		setCocOnly,
		setNonCocOnly,
		SERVICE_TYPE_OPTIONS,
		SHIFT_TYPE_OPTIONS,
		TRIP_TYPE_OPTIONS,
		type RuleDraft,
		type RuleConditions,
		type RuleMode
	} from '$lib/rules';
	import type { LocationItem } from '$lib/api-rules';
	import ChipInput from './ChipInput.svelte';
	import LocationCombobox from './LocationCombobox.svelte';

	let {
		rule,
		locations,
		onCreateLocation,
		onChange,
		onDelete,
		readOnly
	}: {
		rule: RuleDraft;
		locations: LocationItem[];
		onCreateLocation: (name: string) => Promise<LocationItem>;
		onChange: (rule: RuleDraft) => void;
		onDelete: () => void;
		readOnly: boolean;
	} = $props();

	let expanded = $state(false);

	const MODE_OPTIONS: { value: RuleMode; label: string }[] = [
		{ value: 'booking_id', label: 'Booking ID' },
		{ value: 'route', label: 'Rute' },
		{ value: 'filter', label: 'Filter' }
	];

	function modeLabel(mode: RuleMode): string {
		return MODE_OPTIONS.find((o) => o.value === mode)?.label ?? mode;
	}

	function updateRule(patch: Partial<RuleDraft>) {
		onChange({ ...rule, ...patch });
	}

	function updateConditions(patch: Partial<RuleConditions>) {
		onChange({ ...rule, conditions: { ...rule.conditions, ...patch } });
	}

	function numOrNull(raw: string): number | null {
		return raw === '' ? null : Number(raw);
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
				<span class="text-[10px] font-body px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-muted uppercase">
					{modeLabel(rule.mode)}
				</span>
				<span class="text-[13px] font-heading font-medium text-text-primary">{rule.name || 'Rule tanpa nama'}</span>
			</div>
			<span class="text-[11px] font-mono text-text-muted">{conditionSummary(rule)}</span>
		</div>

		<label class="flex items-center gap-1.5 text-[11px] font-body text-text-muted">
			<input
				type="checkbox"
				checked={rule.enabled}
				disabled={readOnly}
				onchange={(e) => updateRule({ enabled: (e.target as HTMLInputElement).checked })}
				class="h-4 w-4 accent-accent"
			/>
			Aktif
		</label>

		{#if !readOnly}
			<button
				type="button"
				onclick={onDelete}
				aria-label={`Hapus rule ${rule.name || 'tanpa nama'}`}
				class="min-h-[36px] min-w-[36px] flex items-center justify-center rounded text-text-muted hover:text-danger focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<Trash2 size={14} aria-hidden="true" />
			</button>
		{/if}
	</div>

	{#if expanded}
		<fieldset disabled={readOnly} class="flex flex-col gap-3 p-3 pt-0 border-0">
			{#if ruleIsEmpty(rule)}
				<p class="text-[11px] text-accent">Rule ini belum punya kondisi — belum akan cocok dengan booking apa pun.</p>
			{/if}

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nama</span>
				<input
					type="text"
					value={rule.name}
					oninput={(e) => updateRule({ name: (e.target as HTMLInputElement).value })}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<div class="flex gap-1" role="radiogroup" aria-label="Mode rule">
				{#each MODE_OPTIONS as opt (opt.value)}
					<button
						type="button"
						role="radio"
						aria-checked={rule.mode === opt.value}
						onclick={() => updateRule({ mode: opt.value })}
						class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
							rule.mode === opt.value
								? 'bg-accent text-bg-base border-accent'
								: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
						}`}
					>
						{opt.label}
					</button>
				{/each}
			</div>

			{#if rule.mode === 'booking_id'}
				<ChipInput
					label="Booking ID"
					value={rule.conditions.bookingIds}
					onChange={(v) => updateConditions({ bookingIds: v })}
				/>
			{/if}

			{#if rule.mode === 'route'}
				<LocationCombobox
					label="Asal"
					{locations}
					{onCreateLocation}
					value={rule.conditions.origin ? [rule.conditions.origin] : []}
					onChange={(v) => updateConditions({ origin: v[0] ?? '' })}
				/>
				<LocationCombobox
					label="Tujuan (urut, maks 5)"
					{locations}
					{onCreateLocation}
					value={rule.conditions.destinations}
					onChange={(v) => updateConditions({ destinations: v })}
					multi
					max={5}
				/>
				<div class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Mode Cocok</span>
					<div class="flex gap-1" role="radiogroup" aria-label="Mode cocok rute">
						{#each [{ value: 'strict', label: 'Ketat' }, { value: 'flexible', label: 'Fleksibel' }] as opt (opt.value)}
							<button
								type="button"
								role="radio"
								aria-checked={rule.conditions.matchMode === opt.value}
								onclick={() => updateConditions({ matchMode: opt.value as 'strict' | 'flexible' })}
								class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
									rule.conditions.matchMode === opt.value
										? 'bg-accent text-bg-base border-accent'
										: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
								}`}
							>
								{opt.label}
							</button>
						{/each}
					</div>
					<p class="text-[11px] text-text-muted">
						{rule.conditions.matchMode === 'strict'
							? 'Semua destinasi wajib muncul berurutan.'
							: 'Hanya destinasi terakhir yang wajib muncul.'}
					</p>
				</div>
			{/if}

			<ChipInput
				label="Jenis Kendaraan"
				value={rule.conditions.serviceTypes}
				onChange={(v) => updateConditions({ serviceTypes: v })}
				options={SERVICE_TYPE_OPTIONS.map((v) => ({ value: v, label: v }))}
			/>

			<div class="grid grid-cols-2 gap-3">
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Berat Maks (kg)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.maxWeight ?? ''}
						oninput={(e) => updateConditions({ maxWeight: numOrNull((e.target as HTMLInputElement).value) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">COD Maks (Rp)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.maxCodAmount ?? ''}
						oninput={(e) => updateConditions({ maxCodAmount: numOrNull((e.target as HTMLInputElement).value) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
			</div>

			<div class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Tipe Booking</span>
				<div class="flex gap-1" role="radiogroup" aria-label="Tipe booking">
					{#each [{ value: 'all', label: 'Semua' }, { value: 'spxid', label: 'SPXID' }, { value: 'reguler', label: 'Reguler' }] as opt (opt.value)}
						<button
							type="button"
							role="radio"
							aria-checked={rule.conditions.bookingType === opt.value}
							onclick={() => updateConditions({ bookingType: opt.value as 'all' | 'spxid' | 'reguler' })}
							class={`min-h-[36px] px-3 rounded-md text-[12px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
								rule.conditions.bookingType === opt.value
									? 'bg-accent text-bg-base border-accent'
									: 'bg-bg-base text-text-muted border-border hover:text-text-primary'
							}`}
						>
							{opt.label}
						</button>
					{/each}
				</div>
			</div>

			<ChipInput
				label="Shift"
				value={rule.conditions.shiftTypes.map(String)}
				onChange={(v) => updateConditions({ shiftTypes: v.map(Number) })}
				options={SHIFT_TYPE_OPTIONS.map((o) => ({ value: String(o.value), label: o.label }))}
			/>

			<ChipInput
				label="Jenis Trip"
				value={rule.conditions.tripTypes.map(String)}
				onChange={(v) => updateConditions({ tripTypes: v.map(Number) })}
				options={TRIP_TYPE_OPTIONS.map((o) => ({ value: String(o.value), label: o.label }))}
			/>

			<div class="flex gap-4">
				<label class="flex items-center gap-1.5 text-[12px] font-body text-text-primary">
					<input
						type="checkbox"
						checked={rule.conditions.cocOnly}
						onchange={(e) => updateConditions(setCocOnly(rule.conditions, (e.target as HTMLInputElement).checked))}
						class="h-4 w-4 accent-accent"
					/>
					Hanya COC
				</label>
				<label class="flex items-center gap-1.5 text-[12px] font-body text-text-primary">
					<input
						type="checkbox"
						checked={rule.conditions.nonCocOnly}
						onchange={(e) => updateConditions(setNonCocOnly(rule.conditions, (e.target as HTMLInputElement).checked))}
						class="h-4 w-4 accent-accent"
					/>
					Hanya Non-COC
				</label>
			</div>

			<div class="grid grid-cols-3 gap-3">
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Min. Deadline (menit)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.minDeadlineMin ?? ''}
						oninput={(e) => updateConditions({ minDeadlineMin: numOrNull((e.target as HTMLInputElement).value) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<label class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Kuota Maks (0 = tanpa batas)</span>
					<input
						type="number"
						min="0"
						value={rule.conditions.maxAcceptCount}
						oninput={(e) =>
							updateConditions({ maxAcceptCount: Math.max(0, Number((e.target as HTMLInputElement).value) || 0) })}
						class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</label>
				<div class="flex flex-col gap-1">
					<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Sudah Diterima</span>
					<span class="min-h-[40px] flex items-center px-2.5 rounded-md text-[13px] font-mono bg-bg-base border border-border text-text-muted">
						{rule.conditions.acceptedCount}
					</span>
				</div>
			</div>

			<label class="flex flex-col gap-1 max-w-[160px]">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Prioritas (-999..999)</span>
				<input
					type="number"
					min="-999"
					max="999"
					value={rule.priority}
					oninput={(e) => updateRule({ priority: Number((e.target as HTMLInputElement).value) || 0 })}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-mono bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
		</fieldset>
	{/if}
</div>
