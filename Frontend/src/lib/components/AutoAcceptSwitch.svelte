<!-- Frontend/src/lib/components/AutoAcceptSwitch.svelte -->
<!-- The auto_accept_enabled kill switch + its OTP arm flow. Unlike TicketDetailDrawer.svelte
     (Fase 7c, exactly one focusable element so a full Tab-trap was explicitly deferred), this
     modal has several focusable elements (code input, send/verify/close buttons) and needs a
     REAL wrap-around focus trap — implementing the upgrade path TicketDetailDrawer's own comment
     anticipated, for this component specifically. -->
<script lang="ts">
	import { onDestroy, tick } from 'svelte';
	import { X, ShieldCheck, ShieldOff } from '@lucide/svelte';
	import { requestAaOtp, verifyAaOtp } from '$lib/api-rules';
	import { ApiError } from '$lib/api';

	let {
		enabled,
		onChange,
		armProofExpired,
		onArmProofExpiredHandled,
		readOnly
	}: {
		enabled: boolean;
		onChange: (next: boolean) => void;
		armProofExpired: boolean;
		onArmProofExpiredHandled: () => void;
		readOnly: boolean;
	} = $props();

	const CODE_TTL_MS = 180_000;
	const RESEND_COOLDOWN_MS = 60_000;
	// Matches the backend's PWVERIFY_TTL_SECS (Backend/crates/api-gateway/src/otp.rs) — the
	// window in which Save must happen for a successful arm to actually persist.
	const ARM_WINDOW_MS = 120_000;

	let modalOpen = $state(false);
	let code = $state('');
	let errorMsg = $state('');
	let notConfigured = $state(false);
	let requesting = $state(false);
	let verifying = $state(false);
	let codeExpiresAt = $state<number | null>(null);
	let resendReadyAt = $state<number | null>(null);
	let armWindowExpiresAt = $state<number | null>(null);
	let now = $state(Date.now());
	let dialogEl: HTMLDivElement | undefined = $state();
	let previouslyFocusedEl: HTMLElement | null = null;

	let ticker: ReturnType<typeof setInterval> | undefined;
	$effect(() => {
		if (modalOpen || armWindowExpiresAt !== null) {
			ticker = setInterval(() => (now = Date.now()), 1000);
			return () => clearInterval(ticker);
		}
	});
	onDestroy(() => clearInterval(ticker));

	const codeSecondsLeft = $derived(codeExpiresAt ? Math.max(0, Math.ceil((codeExpiresAt - now) / 1000)) : 0);
	const resendSecondsLeft = $derived(resendReadyAt ? Math.max(0, Math.ceil((resendReadyAt - now) / 1000)) : 0);
	const armWindowSecondsLeft = $derived(
		armWindowExpiresAt ? Math.max(0, Math.ceil((armWindowExpiresAt - now) / 1000)) : 0
	);

	// Stop ticking once the window naturally lapses — otherwise the interval above would never
	// clear itself (armWindowExpiresAt stays non-null forever unless something resets it).
	$effect(() => {
		if (armWindowExpiresAt !== null && armWindowSecondsLeft === 0) {
			armWindowExpiresAt = null;
		}
	});

	function openModal(withMessage: string = '') {
		modalOpen = true;
		errorMsg = withMessage;
		notConfigured = false;
		code = '';
		codeExpiresAt = null;
		resendReadyAt = null;
		armWindowExpiresAt = null;
	}

	function closeModal() {
		modalOpen = false;
	}

	// The `disabled` attribute on `requesting`/`verifying` is applied the instant those flags flip,
	// and browsers natively blur an element the moment it becomes disabled — moving
	// document.activeElement to <body>, OUTSIDE dialogEl. Since handleDialogKeydown is bound via
	// onkeydown on the dialog div, keydown only bubbles from whatever currently has focus: once
	// focus lands on <body>, the Tab-trap and Escape-close both silently stop working. Call this
	// after every point requesting/verifying toggles to pull focus back inside the dialog.
	async function refocusIfEscaped() {
		await tick();
		if (!modalOpen || !dialogEl) return;
		if (document.activeElement !== null && !dialogEl.contains(document.activeElement)) {
			const target = dialogEl.querySelector<HTMLElement>('button:not([disabled]), input:not([disabled])');
			target?.focus();
		}
	}

	async function sendCode() {
		requesting = true;
		errorMsg = '';
		notConfigured = false;
		await refocusIfEscaped();
		try {
			await requestAaOtp();
			codeExpiresAt = Date.now() + CODE_TTL_MS;
			resendReadyAt = Date.now() + RESEND_COOLDOWN_MS;
		} catch (e) {
			if (e instanceof ApiError && e.status === 400) {
				notConfigured = true;
			} else if (e instanceof ApiError && e.status === 429) {
				errorMsg = 'Kode sudah dikirim, tunggu sebentar sebelum meminta lagi.';
			} else {
				errorMsg = 'Gagal mengirim kode. Coba lagi.';
			}
		} finally {
			requesting = false;
			await refocusIfEscaped();
		}
	}

	async function submitCode() {
		verifying = true;
		errorMsg = '';
		await refocusIfEscaped();
		try {
			await verifyAaOtp(code);
			onChange(true);
			modalOpen = false;
			armWindowExpiresAt = Date.now() + ARM_WINDOW_MS;
		} catch (e) {
			if (e instanceof ApiError && e.status === 401) {
				errorMsg = 'Kode salah atau kedaluwarsa, coba lagi.';
			} else if (e instanceof ApiError && e.status === 429) {
				errorMsg = 'Terlalu banyak percobaan, minta kode baru.';
			} else {
				errorMsg = 'Gagal memverifikasi kode. Coba lagi.';
			}
		} finally {
			verifying = false;
			await refocusIfEscaped();
		}
	}

	function toggle() {
		if (readOnly) return;
		if (enabled) {
			onChange(false);
			armWindowExpiresAt = null;
		} else {
			openModal();
		}
	}

	// Mirrors TicketDetailDrawer.svelte's established pattern: react to a prop transition via
	// $effect rather than an imperative bind:this method, so the page (Task 7) stays purely
	// declarative when it needs to tell this component "the arm window lapsed, ask again."
	$effect(() => {
		if (armProofExpired) {
			openModal('Kode kedaluwarsa, verifikasi ulang.');
			onArmProofExpiredHandled();
		}
	});

	$effect(() => {
		if (modalOpen) {
			previouslyFocusedEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			const firstFocusable = dialogEl?.querySelector<HTMLElement>('button:not([disabled]), input:not([disabled])');
			firstFocusable?.focus();
		} else if (previouslyFocusedEl) {
			previouslyFocusedEl.focus();
			previouslyFocusedEl = null;
		}
	});

	function handleDialogKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			closeModal();
			return;
		}
		if (e.key !== 'Tab' || !dialogEl) return;
		const focusables = Array.from(
			dialogEl.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled])')
		);
		if (focusables.length === 0) return;
		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		if (e.shiftKey && document.activeElement === first) {
			e.preventDefault();
			last.focus();
		} else if (!e.shiftKey && document.activeElement === last) {
			e.preventDefault();
			first.focus();
		}
	}
</script>

<div class="flex items-center justify-between gap-3 p-4 rounded-lg border border-border bg-bg-surface">
	<div class="flex items-center gap-2.5">
		{#if enabled}
			<ShieldCheck size={18} class="text-live" aria-hidden="true" />
		{:else}
			<ShieldOff size={18} class="text-text-muted" aria-hidden="true" />
		{/if}
		<div>
			<p class="text-[13px] font-heading font-semibold text-text-primary">Auto-Accept</p>
			<p class="text-[11px] font-body text-text-muted" aria-live="polite">
				{enabled ? 'Aktif — booking cocok diterima otomatis' : 'Nonaktif'}
			</p>
			{#if armWindowSecondsLeft > 0}
				<p class="text-[11px] font-body text-accent" aria-live="polite">
					Simpan dalam {armWindowSecondsLeft} detik agar Auto-Accept benar-benar aktif
				</p>
			{/if}
		</div>
	</div>
	<button
		type="button"
		role="switch"
		aria-checked={enabled}
		aria-label="Aktifkan atau nonaktifkan Auto-Accept"
		onclick={toggle}
		disabled={readOnly}
		class={`min-h-[44px] min-w-[44px] px-4 rounded-md text-[12px] font-body border disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
			enabled ? 'bg-live text-bg-base border-live' : 'bg-bg-base text-text-muted border-border'
		}`}
	>
		{enabled ? 'ON' : 'OFF'}
	</button>
</div>

{#if modalOpen}
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-bg-base/70 p-4">
		<div
			bind:this={dialogEl}
			role="dialog"
			aria-modal="true"
			aria-label="Verifikasi kode OTP"
			tabindex="-1"
			onkeydown={handleDialogKeydown}
			class="w-full max-w-sm flex flex-col gap-3 p-4 rounded-lg border border-border bg-bg-surface"
		>
			<div class="flex items-center justify-between">
				<h2 class="text-[13px] font-heading font-semibold text-text-primary">Verifikasi Auto-Accept</h2>
				<button
					type="button"
					onclick={closeModal}
					aria-label="Tutup"
					class="min-h-[32px] min-w-[32px] flex items-center justify-center rounded text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					<X size={16} aria-hidden="true" />
				</button>
			</div>

			{#if notConfigured}
				<p role="alert" aria-live="polite" class="text-[12px] text-danger">
					Pengiriman OTP belum dikonfigurasi untuk tenant ini. Hubungi admin untuk mengatur nomor WhatsApp
					sebelum mengaktifkan Auto-Accept.
				</p>
			{:else}
				{#if errorMsg}
					<p role="alert" aria-live="polite" class="text-[12px] text-danger">{errorMsg}</p>
				{/if}

				{#if codeExpiresAt === null}
					<button
						type="button"
						onclick={sendCode}
						disabled={requesting}
						class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{requesting ? 'Mengirim…' : 'Kirim kode'}
					</button>
				{:else}
					<p class="text-[11px] font-mono text-text-muted" aria-live="polite">
						{codeSecondsLeft > 0 ? `Kode berlaku ${codeSecondsLeft} detik lagi` : 'Kode sudah kedaluwarsa'}
					</p>
					<label class="flex flex-col gap-1">
						<span class="text-[10px] font-body text-text-muted uppercase tracking-wide">Kode OTP</span>
						<input
							type="text"
							inputmode="numeric"
							maxlength="6"
							bind:value={code}
							class="min-h-[44px] px-2.5 rounded-md text-[16px] font-mono tracking-widest bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						/>
					</label>
					<button
						type="button"
						onclick={submitCode}
						disabled={verifying || code.trim() === ''}
						class="min-h-[44px] px-4 rounded-md text-[13px] font-body bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{verifying ? 'Memverifikasi…' : 'Verifikasi'}
					</button>
					<button
						type="button"
						onclick={sendCode}
						disabled={requesting || resendSecondsLeft > 0}
						class="min-h-[44px] px-4 rounded-md text-[12px] font-body text-text-muted disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
					>
						{resendSecondsLeft > 0 ? `Kirim ulang (${resendSecondsLeft}s)` : 'Kirim ulang'}
					</button>
				{/if}
			{/if}
		</div>
	</div>
{/if}
