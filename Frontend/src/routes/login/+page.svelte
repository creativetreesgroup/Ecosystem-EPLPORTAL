<!-- Frontend/src/routes/login/+page.svelte -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { apiPost, ApiError } from '$lib/api';

	let username = $state('');
	let password = $state('');
	let showPassword = $state(false);
	let loading = $state(false);
	let errorMsg = $state('');

	let usernameInput: HTMLInputElement | undefined = $state();

	let siteName = $state('TOWER');
	let brandTag = $state('');

	$effect(() => {
		// Public, no-session branding — best-effort, a fetch failure just keeps the defaults
		// above rather than blocking the page (this is decoration, not a requirement to log in).
		fetch('/branding')
			.then((r) => (r.ok ? r.json() : null))
			.then((b) => {
				if (b) {
					siteName = b.site_name || siteName;
					brandTag = b.brand_tag || '';
				}
			})
			.catch(() => {});
	});

	const canSubmit = $derived(username.trim().length > 0 && password.length > 0);

	async function login() {
		if (!canSubmit || loading) return;
		loading = true;
		errorMsg = '';
		try {
			await apiPost('/auth/portal-login', { username: username.trim(), password });
			await goto('/command');
		} catch (e) {
			if (e instanceof ApiError) {
				// Generic message, deliberately identical for "no such user" and "wrong password"
				// (backend already protects this distinction via constant-time comparison — the
				// UI must not leak it back through a different error string). Per the design doc's
				// disclosed data flow: clear the password field and return focus to username, form
				// otherwise stays filled in.
				errorMsg = 'Username atau password salah';
				password = '';
				usernameInput?.focus();
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		} finally {
			loading = false;
		}
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter' && canSubmit) login();
	}
</script>

<svelte:head>
	<title>{siteName} — Masuk</title>
</svelte:head>

<div class="min-h-screen flex items-center justify-center p-4 bg-bg-base">
	<div class="w-full max-w-[380px]">
		<div class="text-center mb-8">
			<div
				class="w-14 h-14 rounded-lg bg-accent/15 border border-accent/30 flex items-center justify-center mx-auto mb-3"
			>
				<span class="font-heading font-bold text-accent text-lg">T</span>
			</div>
			<div class="flex items-center justify-center gap-2">
				<h1 class="font-heading text-[22px] font-bold text-text-primary tracking-tight">{siteName}</h1>
				{#if brandTag}
					<span class="px-2 py-0.5 rounded-md text-[12px] font-bold tracking-wide bg-accent text-bg-base"
						>{brandTag}</span
					>
				{/if}
			</div>
		</div>

		<div class="rounded-lg border border-border bg-bg-surface overflow-hidden">
			<div class="px-5 py-3.5 border-b border-border">
				<span class="font-body text-[10px] font-bold text-text-muted uppercase tracking-[0.12em]"
					>Masuk ke Portal</span
				>
			</div>

			<div class="p-5 space-y-4">
				{#if errorMsg}
					<div
						role="alert"
						aria-live="polite"
						class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
					>
						{errorMsg}
					</div>
				{/if}

				<div class="space-y-1.5">
					<label for="login-username" class="block text-[11px] font-semibold text-text-muted uppercase tracking-widest font-body"
						>Username</label
					>
					<input
						id="login-username"
						type="text"
						bind:value={username}
						bind:this={usernameInput}
						onkeydown={onKeydown}
						placeholder="Username portal"
						autocomplete="username"
						spellcheck="false"
						class="w-full min-h-[44px] px-3 py-2.5 rounded-lg text-[14px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					/>
				</div>

				<div class="space-y-1.5">
					<label for="login-password" class="block text-[11px] font-semibold text-text-muted uppercase tracking-widest font-body"
						>Password</label
					>
					<div class="relative">
						<input
							id="login-password"
							type={showPassword ? 'text' : 'password'}
							bind:value={password}
							onkeydown={onKeydown}
							placeholder="••••••••••"
							autocomplete="current-password"
							class="w-full min-h-[44px] px-3 pr-12 py-2.5 rounded-lg text-[14px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						/>
						<button
							type="button"
							onclick={() => (showPassword = !showPassword)}
							aria-pressed={showPassword}
							class="absolute inset-y-0 right-0 flex items-center px-3 min-w-[44px] text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-lg"
						>
							<span class="sr-only">{showPassword ? 'Sembunyikan password' : 'Tampilkan password'}</span>
							<span aria-hidden="true" class="text-[11px] font-body">{showPassword ? 'Sembunyikan' : 'Tampilkan'}</span>
						</button>
					</div>
				</div>

				<button
					type="button"
					onclick={login}
					disabled={!canSubmit || loading}
					class="w-full min-h-[44px] py-2.5 rounded-lg text-[13px] font-bold font-body transition-opacity bg-accent text-bg-base hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					{loading ? 'Memverifikasi…' : 'Masuk ke Portal'}
				</button>
			</div>
		</div>
	</div>
</div>
