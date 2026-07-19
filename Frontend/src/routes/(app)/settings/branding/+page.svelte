<!-- Frontend/src/routes/(app)/settings/branding/+page.svelte -->
<!-- Single-form settings page for the tenant's Branding record. GET /branding has no permission
     gate (any authenticated session sees real current values), only PUT is main-account-gated
     (Permission::ManageBranding) — so this page is never content-gated, unlike Fase 7f's Log Bot
     tab; non-main-account instead gets the same data behind a native <fieldset disabled>
     cascade, identical pattern to /rules'/`/price`'s RuleRow/PriceRow. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import { fetchBranding, saveBranding, type Branding } from '$lib/api-branding';
	import {
		validateBrandingForm,
		validateImageFile,
		fileToDataUri,
		TITLE_MAX,
		SUBTITLE_MAX,
		SITE_NAME_MAX,
		BRAND_TAG_MAX
	} from '$lib/branding';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	function emptyBranding(): Branding {
		return { title: '', subtitle: '', siteName: '', brandTag: '', logoDataUri: null, faviconDataUri: null };
	}

	let branding = $state<Branding>(emptyBranding());
	let lastSaved = $state<Branding>(emptyBranding());
	let loading = $state(true);
	let saving = $state(false);
	let errorMsg = $state('');
	let successMsg = $state('');
	let logoError = $state('');
	let faviconError = $state('');

	const dirty = $derived(JSON.stringify(branding) !== JSON.stringify(lastSaved));
	const formErrors = $derived(validateBrandingForm(branding));
	const hasFormErrors = $derived(Object.keys(formErrors).length > 0);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			const result = await fetchBranding();
			branding = result;
			lastSaved = { ...result };
		} catch (err) {
			errorMsg = err instanceof ApiError ? `Gagal memuat branding: ${err.message}` : 'Gagal memuat branding';
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
			const result = await saveBranding(branding);
			branding = result;
			lastSaved = { ...result };
			successMsg = 'Branding tersimpan.';
		} catch (err) {
			errorMsg = err instanceof ApiError ? `Gagal menyimpan: ${err.message}` : 'Gagal menyimpan branding';
		} finally {
			saving = false;
		}
	}

	async function handleLogoSelect(e: Event) {
		logoError = '';
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		const err = validateImageFile(file);
		if (err) {
			logoError = err;
			input.value = '';
			return;
		}
		branding.logoDataUri = await fileToDataUri(file);
		input.value = '';
	}

	async function handleFaviconSelect(e: Event) {
		faviconError = '';
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		const err = validateImageFile(file);
		if (err) {
			faviconError = err;
			input.value = '';
			return;
		}
		branding.faviconDataUri = await fileToDataUri(file);
		input.value = '';
	}
</script>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat...</p>
	{:else}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}
		{#if successMsg}
			<p role="status" aria-live="polite" class="text-[13px] text-accent">{successMsg}</p>
		{/if}
		<fieldset disabled={readOnly} class="flex flex-col gap-4 border-0 p-0">
			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Judul</span>
				<input
					type="text"
					bind:value={branding.title}
					maxlength={TITLE_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.title}
					<span class="text-[11px] text-danger">{formErrors.title}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Subjudul</span>
				<input
					type="text"
					bind:value={branding.subtitle}
					maxlength={SUBTITLE_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.subtitle}
					<span class="text-[11px] text-danger">{formErrors.subtitle}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nama Situs</span>
				<input
					type="text"
					bind:value={branding.siteName}
					maxlength={SITE_NAME_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.siteName}
					<span class="text-[11px] text-danger">{formErrors.siteName}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Brand Tag</span>
				<input
					type="text"
					bind:value={branding.brandTag}
					maxlength={BRAND_TAG_MAX}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
				{#if formErrors.brandTag}
					<span class="text-[11px] text-danger">{formErrors.brandTag}</span>
				{/if}
			</label>

			<div class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Logo</span>
				{#if branding.logoDataUri}
					<img
						src={branding.logoDataUri}
						alt="Pratinjau logo situs"
						class="h-16 w-16 object-contain rounded border border-border bg-bg-base"
					/>
				{/if}
				<div class="flex items-center gap-2">
					<input
						type="file"
						accept="image/png,image/jpeg,image/webp"
						onchange={handleLogoSelect}
						aria-label="Unggah logo"
						class="text-[12px] text-text-muted"
					/>
					{#if branding.logoDataUri}
						<button
							type="button"
							onclick={() => (branding.logoDataUri = null)}
							class="text-[11px] text-danger min-h-[36px] px-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							Hapus
						</button>
					{/if}
				</div>
				{#if logoError}
					<span class="text-[11px] text-danger">{logoError}</span>
				{/if}
			</div>

			<div class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Favicon</span>
				{#if branding.faviconDataUri}
					<img
						src={branding.faviconDataUri}
						alt="Pratinjau favicon situs"
						class="h-8 w-8 object-contain rounded border border-border bg-bg-base"
					/>
				{/if}
				<div class="flex items-center gap-2">
					<input
						type="file"
						accept="image/png,image/jpeg,image/webp"
						onchange={handleFaviconSelect}
						aria-label="Unggah favicon"
						class="text-[12px] text-text-muted"
					/>
					{#if branding.faviconDataUri}
						<button
							type="button"
							onclick={() => (branding.faviconDataUri = null)}
							class="text-[11px] text-danger min-h-[36px] px-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							Hapus
						</button>
					{/if}
				</div>
				{#if faviconError}
					<span class="text-[11px] text-danger">{faviconError}</span>
				{/if}
			</div>

			<button
				type="button"
				onclick={handleSave}
				disabled={saving || !dirty || hasFormErrors}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{saving ? 'Menyimpan...' : 'Simpan Perubahan'}
			</button>
		</fieldset>
	{/if}
</div>
