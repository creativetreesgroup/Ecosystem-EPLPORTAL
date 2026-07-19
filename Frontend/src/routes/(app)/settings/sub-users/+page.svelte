<!-- Frontend/src/routes/(app)/settings/sub-users/+page.svelte -->
<!-- Create/delete management page for the tenant's portal_users. GET /auth/portal-users has no
     permission gate (any authenticated session sees the real list), only POST/DELETE are
     main-account-gated — so this page is edit-gated like /settings/branding and
     /settings/locations, never content-gated like /settings/bot. The backend also enforces a
     self-lockout guard on DELETE (a main account cannot delete their own row) — since the
     frontend has no portal_user id to compare (only username, from /auth/me), that row's delete
     button is disabled via a username match (isSelf), with an inline note explaining why. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import { Eye, EyeOff } from '@lucide/svelte';
	import type { PageProps } from './$types';
	import { fetchSubUsers, createSubUser, deleteSubUser, type PortalUser } from '$lib/api-sub-users';
	import { validatePassword, isSelf } from '$lib/sub-users';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let subUsers = $state<PortalUser[]>([]);
	let username = $state('');
	let password = $state('');
	let displayName = $state('');
	let makeMainAccount = $state(false);
	let showPassword = $state(false);
	let loading = $state(true);
	let creating = $state(false);
	let deletingId = $state<string | null>(null);
	let errorMsg = $state('');

	const passwordError = $derived(password === '' ? null : validatePassword(password));
	const canSubmit = $derived(
		username.trim() !== '' && displayName.trim() !== '' && password !== '' && validatePassword(password) === null
	);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			subUsers = await fetchSubUsers();
		} catch {
			errorMsg = 'Gagal memuat sub-user.';
		} finally {
			loading = false;
		}
	}

	onMount(load);

	async function handleCreate() {
		if (!canSubmit) return;
		creating = true;
		errorMsg = '';
		try {
			const created = await createSubUser({
				username: username.trim(),
				password,
				displayName: displayName.trim(),
				isMainAccount: makeMainAccount
			});
			subUsers = [...subUsers, created];
			username = '';
			password = '';
			displayName = '';
			makeMainAccount = false;
		} catch (err) {
			errorMsg =
				err instanceof ApiError && err.status === 409 ? 'Username ini sudah dipakai.' : 'Gagal membuat sub-user.';
		} finally {
			creating = false;
		}
	}

	async function handleDelete(subUser: PortalUser) {
		if (!confirm(`Hapus akun "${subUser.username}"?`)) return;
		deletingId = subUser.id;
		errorMsg = '';
		try {
			await deleteSubUser(subUser.id);
			subUsers = subUsers.filter((u) => u.id !== subUser.id);
		} catch {
			errorMsg = 'Gagal menghapus sub-user.';
		} finally {
			deletingId = null;
		}
	}
</script>

<svelte:head>
	<title>Sub-user — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else}
		{#if readOnly}
			<div
				role="alert"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
			>
				Hanya akun utama yang dapat mengelola sub-user.
			</div>
		{/if}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}

		<fieldset disabled={readOnly} class="flex flex-col gap-3 border-0 p-0">
			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Username</span>
				<input
					type="text"
					bind:value={username}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Password</span>
				<div class="relative">
					<input
						type={showPassword ? 'text' : 'password'}
						bind:value={password}
						autocomplete="new-password"
						class="w-full min-h-[40px] px-2.5 pr-12 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
					<button
						type="button"
						onclick={() => (showPassword = !showPassword)}
						aria-pressed={showPassword}
						class="absolute inset-y-0 right-0 flex items-center px-3 min-w-[44px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-md"
					>
						<span class="sr-only">{showPassword ? 'Sembunyikan password' : 'Tampilkan password'}</span>
						{#if showPassword}
							<EyeOff size={16} aria-hidden="true" />
						{:else}
							<Eye size={16} aria-hidden="true" />
						{/if}
					</button>
				</div>
				{#if passwordError}
					<span class="text-[11px] text-danger">{passwordError}</span>
				{/if}
			</label>

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Nama Tampilan</span>
				<input
					type="text"
					bind:value={displayName}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>

			<label class="flex items-center gap-2">
				<input type="checkbox" bind:checked={makeMainAccount} class="h-4 w-4 accent-accent" />
				<span class="text-[13px] font-body text-text-primary">Jadikan akun utama</span>
			</label>

			<button
				type="button"
				onclick={handleCreate}
				disabled={creating || !canSubmit}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{creating ? 'Membuat…' : 'Buat Sub-user'}
			</button>
		</fieldset>

		{#if subUsers.length === 0}
			<p class="text-[13px] text-text-muted">Belum ada sub-user.</p>
		{:else}
			<ul class="flex flex-col gap-2">
				{#each subUsers as subUser (subUser.id)}
					{@const self = isSelf(subUser.username, data.user.username)}
					<li class="flex items-center justify-between gap-2 rounded-lg border border-border bg-bg-surface p-3">
						<div class="flex flex-col gap-0.5">
							<span class="text-[13px] font-body text-text-primary">
								{subUser.displayName}
								<span class="text-text-muted">({subUser.username})</span>
								{#if subUser.isMainAccount}
									<span
										class="text-[10px] px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-muted uppercase"
									>
										Akun Utama
									</span>
								{/if}
							</span>
							{#if self}
								<span class="text-[11px] text-text-muted">Tidak bisa menghapus akun sendiri.</span>
							{/if}
						</div>
						<button
							type="button"
							disabled={readOnly || self || deletingId === subUser.id}
							onclick={() => handleDelete(subUser)}
							class="min-h-[36px] px-2 text-[11px] text-danger disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							{deletingId === subUser.id ? 'Menghapus…' : 'Hapus'}
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
