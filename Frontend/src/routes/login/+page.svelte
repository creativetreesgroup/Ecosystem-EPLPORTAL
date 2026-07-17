<!-- Frontend/src/routes/login/+page.svelte -->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { apiPost, ApiError } from '$lib/api';
	import { Eye, EyeOff } from '@lucide/svelte';

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
				if (e.status === 401) {
					// Generic message, deliberately identical for "no such user" and "wrong password"
					// (backend already protects this distinction via constant-time comparison — the
					// UI must not leak it back through a different error string).
					errorMsg = 'Username atau password salah';
				} else if (e.status === 429) {
					// tower_governor's login_rate_limit_layer (~20/min/IP) tripped — this is not a
					// credentials problem, don't tell the user their password is wrong.
					errorMsg = 'Terlalu banyak percobaan. Coba lagi sebentar lagi.';
				} else {
					// Any other non-2xx (e.g. 500) — transient server error, not a credentials problem.
					errorMsg = 'Terjadi kesalahan pada server. Coba lagi.';
				}
				// Per the design doc's disclosed data flow: clear the password field and return focus
				// to username on any rejected attempt (username stays filled in). Applying this to
				// 429/500 too, not just 401 — the user will retry with the same credentials regardless
				// of which of these three fired, and re-focusing username is harmless; the only cost is
				// re-typing the password, which is a minor courtesy loss, not a correctness issue.
				password = '';
				usernameInput?.focus();
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		} finally {
			loading = false;
		}
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

				<form
					onsubmit={(e) => {
						e.preventDefault();
						login();
					}}
					class="space-y-4"
				>
					<div class="space-y-1.5">
						<label for="login-username" class="block text-[11px] font-semibold text-text-muted uppercase tracking-widest font-body"
							>Username</label
						>
						<input
							id="login-username"
							type="text"
							bind:value={username}
							bind:this={usernameInput}
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
								{#if showPassword}
									<EyeOff size={18} aria-hidden="true" />
								{:else}
									<Eye size={18} aria-hidden="true" />
								{/if}
							</button>
						</div>
					</div>

					<button
						type="submit"
						disabled={!canSubmit || loading}
						class="w-full min-h-[44px] py-2.5 rounded-lg text-[13px] font-bold font-body transition-opacity bg-accent text-bg-base hover:opacity-90 disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{loading ? 'Memverifikasi…' : 'Masuk ke Portal'}
					</button>
				</form>
			</div>
		</div>
	</div>
</div>
