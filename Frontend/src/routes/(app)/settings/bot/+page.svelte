<!-- Frontend/src/routes/(app)/settings/bot/+page.svelte -->
<!-- Bot/WAHA configuration form. GET/PUT /bot/settings are BOTH Permission::ManageBotSettings-
     gated (main-account only) — unlike /settings/branding, this page is content-gated, not just
     edit-gated: there is no read-only view for non-main-account. The normal path is the "Bot"
     nav entry simply not existing for them (+layout.svelte, Task 3); this page's own forbidden
     state handles the case where a non-main-account session reaches this URL directly anyway —
     fetchBotSettings() genuinely 403s, and that's shown as a clear message, not a raw error. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchBotSettings, saveBotSettings, type BotSettings } from '$lib/api-bot-settings';
	import { isValidUrlFormat, apiKeyError } from '$lib/bot-settings';
	import { ApiError } from '$lib/api';

	let { data: _data }: PageProps = $props();

	function emptySettings(): BotSettings {
		return {
			enabled: false,
			webhookUrl: '',
			waNumber: '',
			waGroup: '',
			wahaUrl: '',
			wahaSession: '',
			wahaApiKeySet: false
		};
	}

	let settings = $state<BotSettings>(emptySettings());
	let lastSaved = $state<BotSettings>(emptySettings());
	let apiKeyInput = $state('');
	let loading = $state(true);
	let saving = $state(false);
	let forbidden = $state(false);
	let errorMsg = $state('');
	let successMsg = $state('');

	const dirty = $derived(JSON.stringify(settings) !== JSON.stringify(lastSaved) || apiKeyInput.trim() !== '');
	const apiKeyErrorMsg = $derived(apiKeyError(settings.wahaApiKeySet, apiKeyInput));
	const wahaUrlErrorMsg = $derived(isValidUrlFormat(settings.wahaUrl) ? null : 'URL tidak valid');
	const webhookUrlErrorMsg = $derived(isValidUrlFormat(settings.webhookUrl) ? null : 'URL tidak valid');
	const hasFormErrors = $derived(
		apiKeyErrorMsg !== null || wahaUrlErrorMsg !== null || webhookUrlErrorMsg !== null
	);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const result = await fetchBotSettings();
			settings = result;
			lastSaved = { ...result };
		} catch (err) {
			if (err instanceof ApiError && err.status === 403) {
				forbidden = true;
			} else {
				errorMsg =
					err instanceof ApiError ? `Gagal memuat pengaturan bot: ${err.message}` : 'Gagal memuat pengaturan bot';
			}
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function handleSave() {
		if (hasFormErrors) return;
		saving = true;
		errorMsg = '';
		successMsg = '';
		try {
			const result = await saveBotSettings({ ...settings, wahaApiKey: apiKeyInput });
			settings = result;
			lastSaved = { ...result };
			apiKeyInput = '';
			successMsg = 'Pengaturan bot tersimpan.';
		} catch (err) {
			errorMsg = err instanceof ApiError ? `Gagal menyimpan: ${err.message}` : 'Gagal menyimpan pengaturan bot';
		} finally {
			saving = false;
		}
	}
</script>

<svelte:head>
	<title>Bot — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else if forbidden}
		<p role="alert" aria-live="polite" class="text-[13px] text-danger">
			Anda tidak memiliki akses ke halaman ini.
		</p>
	{:else}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}
		{#if successMsg}
			<p role="status" aria-live="polite" class="text-[13px] text-accent">{successMsg}</p>
		{/if}

		<label class="flex items-center gap-2">
			<input type="checkbox" bind:checked={settings.enabled} class="h-4 w-4 accent-accent" />
			<span class="text-[13px] font-body text-text-primary">Aktifkan integrasi bot</span>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Webhook URL</span>
			<input
				type="text"
				bind:value={settings.webhookUrl}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			{#if webhookUrlErrorMsg}
				<span class="text-[11px] text-danger">{webhookUrlErrorMsg}</span>
			{/if}
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nomor WhatsApp (OTP)</span>
			<input
				type="text"
				bind:value={settings.waNumber}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Grup WhatsApp</span>
			<input
				type="text"
				bind:value={settings.waGroup}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">WAHA URL</span>
			<input
				type="text"
				bind:value={settings.wahaUrl}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			{#if wahaUrlErrorMsg}
				<span class="text-[11px] text-danger">{wahaUrlErrorMsg}</span>
			{/if}
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">WAHA Session</span>
			<input
				type="text"
				bind:value={settings.wahaSession}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</label>

		<label class="flex flex-col gap-1">
			<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">WAHA API Key</span>
			<input
				type="password"
				bind:value={apiKeyInput}
				placeholder={settings.wahaApiKeySet ? 'Biarkan kosong untuk tidak mengubah' : 'Wajib diisi (setup pertama)'}
				class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
			{#if apiKeyErrorMsg}
				<span class="text-[11px] text-danger">{apiKeyErrorMsg}</span>
			{/if}
		</label>

		<button
			type="button"
			onclick={handleSave}
			disabled={saving || !dirty || hasFormErrors}
			class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			{saving ? 'Menyimpan…' : 'Simpan Perubahan'}
		</button>
	{/if}
</div>
