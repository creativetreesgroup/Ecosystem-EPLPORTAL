<!-- Frontend/src/routes/(app)/rules/+page.svelte -->
<!-- /rules: local-edit + single-Save Rule Builder. All mutations happen against local $state;
     one "Simpan Perubahan" PUTs the whole set and REPLACES local state with the response (never
     merges) — this is how the user sees server-side dedupe/collapse and sanitize warnings
     reflected, matching what the backend actually did. -->
<script lang="ts">
	import { beforeNavigate } from '$app/navigation';
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchSettings, saveSettings, fetchLocations, createLocation, type LocationItem } from '$lib/api-rules';
	import { ApiError } from '$lib/api';
	import { newRuleDraft, isDirty, type RuleDraft, type RulesPageState, type RuleMode } from '$lib/rules';
	import RuleRow from '$lib/components/RuleRow.svelte';
	import AutoAcceptSwitch from '$lib/components/AutoAcceptSwitch.svelte';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let autoAcceptEnabled = $state(false);
	let rules = $state<RuleDraft[]>([]);
	let locations = $state<LocationItem[]>([]);
	let lastSaved = $state<RulesPageState>({ autoAcceptEnabled: false, rules: [] });
	let loading = $state(true);
	let saving = $state(false);
	let errorMsg = $state('');
	let warnings = $state<string[]>([]);
	let armProofExpired = $state(false);

	const dirty = $derived(isDirty({ autoAcceptEnabled, rules }, lastSaved));

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const [settings, locs] = await Promise.all([fetchSettings(), fetchLocations()]);
			autoAcceptEnabled = settings.autoAcceptEnabled;
			rules = settings.rules;
			warnings = settings.warnings;
			lastSaved = { autoAcceptEnabled: settings.autoAcceptEnabled, rules: settings.rules };
			locations = locs;
		} catch {
			errorMsg = 'Gagal memuat pengaturan rule. Coba lagi.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	function addRule(mode: RuleMode) {
		rules = [...rules, newRuleDraft(mode)];
	}

	function updateRule(clientKey: string, next: RuleDraft) {
		rules = rules.map((r) => (r.clientKey === clientKey ? next : r));
	}

	function deleteRule(clientKey: string) {
		rules = rules.filter((r) => r.clientKey !== clientKey);
	}

	async function handleCreateLocation(name: string): Promise<LocationItem> {
		const created = await createLocation(name);
		locations = [...locations, created];
		return created;
	}

	async function save() {
		saving = true;
		errorMsg = '';
		try {
			const result = await saveSettings({ autoAcceptEnabled, rules });
			autoAcceptEnabled = result.autoAcceptEnabled;
			rules = result.rules;
			warnings = result.warnings;
			lastSaved = { autoAcceptEnabled: result.autoAcceptEnabled, rules: result.rules };
		} catch (e) {
			// Narrowed to exactly the arm-attempt case (mirrors the backend's own
			// `if body.auto_accept_enabled && !currently_enabled` gate in put_settings): a 401 here
			// means the OTP proof window lapsed. A 401 in any OTHER state (session actually expired
			// mid-page) would be misreported as "OTP expired" too — a disclosed limitation, not
			// fixed here, since apiPost/fetch in this codebase don't surface distinguishing error
			// body text (Fase 7c's Task 6 tracked the same generic-error-message gap for
			// /bookings/:id/accept; this is the same underlying apiPost/ApiError design boundary,
			// out of scope to fix from this page alone).
			if (e instanceof ApiError && e.status === 401 && autoAcceptEnabled && !lastSaved.autoAcceptEnabled) {
				armProofExpired = true;
			} else if (e instanceof ApiError) {
				errorMsg = 'Gagal menyimpan. Coba lagi.';
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		} finally {
			saving = false;
		}
	}

	beforeNavigate((nav) => {
		if (dirty && !confirm('Ada perubahan yang belum disimpan. Tetap tinggalkan halaman?')) {
			nav.cancel();
		}
	});
</script>

<svelte:window
	onbeforeunload={(e) => {
		if (dirty) e.preventDefault();
	}}
/>

<svelte:head>
	<title>Rules — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-3xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Rule Builder</h1>

	{#if readOnly}
		<div
			role="alert"
			class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
		>
			Hanya akun utama yang dapat mengubah rule.
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

	{#if warnings.length > 0}
		<div
			role="alert"
			aria-live="polite"
			class="flex flex-col gap-1 px-3.5 py-2.5 rounded-lg text-[12px] font-body border bg-accent/10 text-accent border-accent/30"
		>
			{#each warnings as w (w)}
				<p>{w}</p>
			{/each}
		</div>
	{/if}

	{#if loading}
		<p class="text-[12px] text-text-muted">Memuat…</p>
	{:else}
		<AutoAcceptSwitch
			enabled={autoAcceptEnabled}
			onChange={(next) => (autoAcceptEnabled = next)}
			{armProofExpired}
			onArmProofExpiredHandled={() => (armProofExpired = false)}
			{readOnly}
		/>

		<div class="flex flex-col gap-2">
			{#each rules as rule (rule.clientKey)}
				<RuleRow
					{rule}
					{locations}
					onCreateLocation={handleCreateLocation}
					onChange={(next) => updateRule(rule.clientKey, next)}
					onDelete={() => deleteRule(rule.clientKey)}
					{readOnly}
				/>
			{/each}
		</div>

		{#if !readOnly}
			<div class="flex gap-2">
				<button
					type="button"
					onclick={() => addRule('route')}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Rule Rute
				</button>
				<button
					type="button"
					onclick={() => addRule('booking_id')}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Rule Booking ID
				</button>
				<button
					type="button"
					onclick={() => addRule('filter')}
					class="min-h-[44px] px-4 rounded-md text-[12px] font-body border border-border text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					+ Rule Filter
				</button>
			</div>

			<button
				type="button"
				onclick={save}
				disabled={saving || !dirty}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{saving ? 'Menyimpan…' : 'Simpan Perubahan'}
			</button>
		{/if}
	{/if}
</div>
