<!-- Frontend/src/routes/(app)/settings/spx-credentials/+page.svelte -->
<!-- Manage the tenant's stored SPX agency credentials. GET /auth/spx-credentials has no
     permission gate (any session sees the list), only PUT/DELETE and POST /auth/spx-login
     are main-account-gated — so this page is edit-gated like /settings/branding and
     /settings/locations. There is no partial update or rename on the backend (PUT always
     upserts the whole username+password for a label), so editing is delete-and-recreate:
     re-submitting the add form with an existing label overwrites it. Saved credentials only
     take effect after reactor-core restarts (the poller bootstraps them once at boot) —
     hence the always-visible notice. The "Test Koneksi" button runs a REAL login against
     the live SPX upstream (up to ~80s), so it is click-only, in-flight-locked, 60s-cooldown
     guarded (client + server), and 90s-aborted; its result copy is deliberately honest,
     because the backend cannot distinguish wrong-password from SPX-down. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import { Eye, EyeOff } from '@lucide/svelte';
	import type { PageProps } from './$types';
	import {
		fetchSpxCredentials,
		saveSpxCredential,
		deleteSpxCredential,
		testSpxLogin,
		type SpxCredential
	} from '$lib/api-spx-credentials';
	import {
		validateLabel,
		validateUsername,
		validatePassword,
		duplicateUsernameLabel
	} from '$lib/spx-credentials';
	import { ApiError } from '$lib/api';

	let { data }: PageProps = $props();
	const readOnly = $derived(!data.user.is_main_account);

	let credentials = $state<SpxCredential[]>([]);
	let label = $state('');
	let username = $state('');
	let password = $state('');
	let showPassword = $state(false);
	let loading = $state(true);
	let saving = $state(false);
	let deletingLabel = $state<string | null>(null);
	let errorMsg = $state('');
	let successMsg = $state('');

	// Per-label transient UI state for the Test button.
	let testing = $state<Record<string, boolean>>({});
	let testResult = $state<Record<string, string>>({});
	let cooldownUntil = $state<Record<string, number>>({});
	let now = $state(0);

	const labelError = $derived(label === '' ? null : validateLabel(label));
	const usernameError = $derived(username === '' ? null : validateUsername(username));
	const passwordError = $derived(password === '' ? null : validatePassword(password));
	const dupLabel = $derived(duplicateUsernameLabel(username, credentials, label.trim()));
	const overwriteLabel = $derived(
		label.trim() !== '' && credentials.some((c) => c.label === label.trim()) ? label.trim() : null
	);
	const canSubmit = $derived(
		validateLabel(label) === null &&
			validateUsername(username) === null &&
			validatePassword(password) === null &&
			dupLabel === null
	);

	async function load() {
		loading = true;
		errorMsg = '';
		try {
			credentials = await fetchSpxCredentials();
		} catch {
			errorMsg = 'Gagal memuat kredensial SPX.';
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		load();
		// 1s ticker for the cooldown countdown. Reassigns `now` only while a
		// cooldown is still displayed, so an idle page never re-renders. Compare
		// against the shown `now` (not Date.now()) so the falling-edge tick that
		// pushes `now` past the deadline still fires — otherwise `now` freezes ~1s
		// short and the button sticks on "Tunggu 1s" until a reload.
		const timer = setInterval(() => {
			const active = Object.values(cooldownUntil).some((t) => t > now);
			if (active) now = Date.now();
		}, 1000);
		return () => clearInterval(timer);
	});

	function cooldownRemaining(l: string): number {
		const until = cooldownUntil[l] ?? 0;
		return until > now ? Math.ceil((until - now) / 1000) : 0;
	}

	async function handleCreate() {
		if (!canSubmit) return;
		saving = true;
		errorMsg = '';
		successMsg = '';
		try {
			const saved = await saveSpxCredential(label.trim(), username.trim(), password);
			const idx = credentials.findIndex((c) => c.label === saved.label);
			if (idx >= 0) credentials[idx] = saved;
			else credentials = [...credentials, saved];
			label = '';
			username = '';
			password = '';
			showPassword = false;
			successMsg = 'Kredensial tersimpan. Aktif setelah reactor-core direstart.';
		} catch (err) {
			errorMsg =
				err instanceof ApiError && err.status === 409
					? 'Label ini sedang dipakai, coba lagi.'
					: 'Gagal menyimpan kredensial.';
		} finally {
			saving = false;
		}
	}

	async function handleDelete(l: string) {
		if (!confirm(`Hapus kredensial "${l}"?`)) return;
		deletingLabel = l;
		errorMsg = '';
		successMsg = '';
		try {
			await deleteSpxCredential(l);
			credentials = credentials.filter((c) => c.label !== l);
		} catch {
			errorMsg = 'Gagal menghapus kredensial.';
		} finally {
			deletingLabel = null;
		}
	}

	async function handleTest(l: string) {
		if (testing[l] || cooldownRemaining(l) > 0) return;
		testing = { ...testing, [l]: true };
		testResult = { ...testResult, [l]: '' };
		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), 90_000);
		try {
			const result = await testSpxLogin(l, controller.signal);
			testResult = {
				...testResult,
				[l]: result.ok
					? `Login berhasil (tier: ${result.tier}).`
					: 'Tidak berhasil membuat sesi. Periksa username/password, atau SPX sedang tidak bisa dihubungi.'
			};
		} catch (err) {
			let msg = 'Gagal menguji koneksi.';
			if (err instanceof DOMException && err.name === 'AbortError')
				msg = 'Test koneksi melebihi batas waktu (90 detik).';
			else if (err instanceof ApiError && err.status === 429)
				msg = 'Test koneksi baru saja dijalankan, coba lagi sebentar.';
			testResult = { ...testResult, [l]: msg };
		} finally {
			clearTimeout(timeout);
			testing = { ...testing, [l]: false };
			cooldownUntil = { ...cooldownUntil, [l]: Date.now() + 60_000 };
			now = Date.now();
		}
	}
</script>

<svelte:head>
	<title>Akun SPX — TOWER</title>
</svelte:head>

<div class="flex flex-col gap-4 max-w-xl">
	<div
		role="alert"
		class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
	>
		Kredensial hanya dimuat poller saat reactor-core dijalankan. Perubahan di sini baru aktif setelah
		restart.
	</div>

	{#if loading}
		<p class="text-[13px] text-text-muted">Memuat…</p>
	{:else}
		{#if readOnly}
			<div
				role="alert"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-body border bg-accent/10 text-accent border-accent/30"
			>
				Hanya akun utama yang dapat mengubah kredensial SPX.
			</div>
		{/if}
		{#if errorMsg}
			<p role="alert" aria-live="polite" class="text-[13px] text-danger">{errorMsg}</p>
		{/if}
		{#if successMsg}
			<p role="status" aria-live="polite" class="text-[13px] text-accent">{successMsg}</p>
		{/if}

		<fieldset disabled={readOnly} class="flex flex-col gap-3 border-0 p-0">
			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Label</span>
				<input
					type="text"
					bind:value={label}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			{#if labelError}<span class="text-[11px] text-danger">{labelError}</span>{/if}
			{#if overwriteLabel}
				<span class="text-[11px] text-text-muted"
					>Label ini sudah ada — menyimpan akan menimpa kredensial lama.</span
				>
			{/if}

			<label class="flex flex-col gap-1">
				<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Username</span>
				<input
					type="text"
					bind:value={username}
					class="min-h-[40px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				/>
			</label>
			{#if usernameError}<span class="text-[11px] text-danger">{usernameError}</span>{/if}
			{#if dupLabel}
				<span class="text-[11px] text-danger"
					>Username ini sudah dipakai label "{dupLabel}". Dua label dengan username sama akan
					bentrok saat poller start.</span
				>
			{/if}

			<div class="flex flex-col gap-1">
				<label
					for="new-spx-password"
					class="text-[10px] font-body text-text-muted uppercase tracking-wide">Password</label
				>
				<div class="relative">
					<input
						id="new-spx-password"
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
				{#if passwordError}<span class="text-[11px] text-danger">{passwordError}</span>{/if}
			</div>

			<button
				type="button"
				onclick={handleCreate}
				disabled={saving || !canSubmit}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				{saving ? 'Menyimpan…' : 'Simpan Kredensial'}
			</button>
		</fieldset>

		{#if credentials.length === 0}
			<p class="text-[13px] text-text-muted">Belum ada kredensial SPX.</p>
		{:else}
			<ul class="flex flex-col gap-2">
				{#each credentials as cred (cred.label)}
					{@const remaining = cooldownRemaining(cred.label)}
					<li class="flex flex-col gap-1 rounded-lg border border-border bg-bg-surface p-3">
						<div class="flex items-center justify-between gap-2">
							<span class="text-[13px] font-body text-text-primary">
								{cred.label}
								<span class="text-text-muted">({cred.username})</span>
							</span>
							<div class="flex items-center gap-2">
								<button
									type="button"
									disabled={readOnly || testing[cred.label] || remaining > 0}
									onclick={() => handleTest(cred.label)}
									class="min-h-[36px] px-2 text-[11px] text-accent disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
								>
									{#if testing[cred.label]}
										Menguji…
									{:else if remaining > 0}
										Tunggu {remaining}s
									{:else}
										Test
									{/if}
								</button>
								<button
									type="button"
									disabled={readOnly || deletingLabel === cred.label}
									onclick={() => handleDelete(cred.label)}
									class="min-h-[36px] px-2 text-[11px] text-danger disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
								>
									{deletingLabel === cred.label ? 'Menghapus…' : 'Hapus'}
								</button>
							</div>
						</div>
						{#if testResult[cred.label]}
							<span role="status" aria-live="polite" class="text-[11px] text-text-muted"
								>{testResult[cred.label]}</span
							>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</div>
