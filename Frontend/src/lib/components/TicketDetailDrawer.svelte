<!-- Frontend/src/lib/components/TicketDetailDrawer.svelte -->
<!-- Slide-in modal panel: booking detail + accept_events audit trail (Task 4's
     fetchBookingDetail/fetchAuditTrail). bookingId === null means closed — nothing renders, no
     listeners attached. Backdrop `<div>` and the panel `<div>` below are rendered as SIBLINGS
     (not nested) inside the same {#if} block: a click starting inside the panel bubbles up through
     the panel's own ancestors, never through the backdrop (a sibling, not an ancestor), so it
     can never reach the backdrop's onClose handler — verified, not just copied from the brief.

     The panel is a plain <div role="dialog">, not <aside role="dialog"> (the brief's sample):
     svelte-check's a11y rule (a11y_no_noninteractive_element_to_interactive_role) flags <aside>
     because it's already an ARIA landmark element, and landmark + dialog role on the same node
     is a real conflict for assistive tech, not a lint false-positive — matches the
     div-role="dialog" pattern already used by NotificationCenter.svelte in this codebase. -->
<script lang="ts">
	import { X } from '@lucide/svelte';
	import { fetchBookingDetail, fetchAuditTrail, type AuditEvent } from '$lib/api-tickets';
	import type { TicketDetailRow } from '$lib/tickets';

	let { bookingId, onClose }: { bookingId: string | null; onClose: () => void } = $props();

	type DetailState =
		| (TicketDetailRow & { updatedAt: string; acceptLatencyMs: number | null; isCoc: boolean })
		| null;

	let detail = $state<DetailState>(null);
	let auditTrail = $state<AuditEvent[]>([]);
	let loading = $state(false);
	let errorMsg = $state('');
	let closeButtonEl: HTMLButtonElement | undefined = $state();
	let previouslyFocusedEl: HTMLElement | null = null;

	// Data load, keyed on bookingId. `requestedId` guards against a slow-resolving fetch for a
	// PREVIOUS bookingId landing after the user already switched to a different one (or closed
	// the drawer) — without this, a fast id2 response could still get clobbered a moment later
	// by a slow id1 response finally arriving.
	$effect(() => {
		if (!bookingId) {
			detail = null;
			auditTrail = [];
			return;
		}
		const requestedId = bookingId;
		loading = true;
		errorMsg = '';
		Promise.all([fetchBookingDetail(requestedId), fetchAuditTrail(requestedId)])
			.then(([d, events]) => {
				if (requestedId !== bookingId) return;
				detail = d;
				auditTrail = events;
			})
			.catch(() => {
				if (requestedId !== bookingId) return;
				errorMsg = 'Gagal memuat detail tiket.';
			})
			.finally(() => {
				if (requestedId !== bookingId) return;
				loading = false;
			});
	});

	// Focus management (hand-rolled, no focus-trap dependency — this drawer has exactly ONE
	// focusable element, the close button, so a general multi-element trap is overkill; see the
	// Tab-key branch in handleKeydown below for how Tab/Shift+Tab is kept from leaving it):
	// - Open: remember whatever had focus (the row that triggered this) and move focus onto the
	//   close button, the drawer's first focusable element, so a keyboard user isn't left focused
	//   on a now-backdrop-obscured element.
	// - Close: restore focus to that remembered trigger element so focus doesn't fall back to
	//   <body>. $effect only runs client-side (never during SSR), so `document` is always defined
	//   here.
	$effect(() => {
		if (bookingId) {
			previouslyFocusedEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
			// Deferred via a macrotask, NOT called synchronously here — verified live with a real
			// keyboard Playwright test (opening this drawer via Enter on a table row), not a
			// hypothetical: opening via a KEYBOARD Enter press (vs. a mouse click) and focusing this
			// close <button> too early re-triggers Chromium's native "Enter activates the currently
			// focused button" default action a moment later — firing an unwanted click on the close
			// button and immediately closing the drawer that had just opened. `setTimeout(..., 0)`
			// pushes the focus move to the next macrotask, safely after the triggering keydown's
			// native default-action processing has fully finished, regardless of the exact
			// effect-vs-keydown ordering Svelte uses under the hood.
			const focusTimeoutId = setTimeout(() => closeButtonEl?.focus(), 0);
			return () => clearTimeout(focusTimeoutId);
		} else if (previouslyFocusedEl) {
			previouslyFocusedEl.focus();
			previouslyFocusedEl = null;
		}
	});

	// Indonesian labels for `detail.failureReason` — same mapping convention as
	// TicketsTable.svelte's `statusLabel` (internal value -> Indonesian display text).
	function failureReasonLabel(reason: NonNullable<TicketDetailRow['failureReason']>): string {
		if (reason === 'expired') return 'Kedaluwarsa';
		if (reason === 'taken_by_other') return 'Diambil agensi lain';
		return 'Gagal saat diproses'; // 'manual_accept_failed'
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			onClose();
			return;
		}
		// Minimal Tab trap: the close button is the ONLY focusable element in this dialog, so
		// there's no first/last-element pair to cycle between — blocking Tab/Shift+Tab from
		// leaving it (it's already focused) is sufficient to satisfy aria-modal="true" without a
		// general-purpose focus-trap library. If this drawer ever gains more focusable elements
		// (e.g. action buttons), this needs to become a real wrap-around trap (Tab from last
		// focusable -> first, Shift+Tab from first -> last) instead of always refocusing the
		// close button.
		if (e.key === 'Tab') {
			e.preventDefault();
			closeButtonEl?.focus();
		}
	}
</script>

<!-- Handler is only bound while the drawer is open (`undefined` otherwise) — Svelte adds/removes
     the actual window listener as this prop changes, so no listener leaks once bookingId is null. -->
<svelte:window onkeydown={bookingId ? handleKeydown : undefined} />

{#if bookingId}
	<div class="fixed inset-0 bg-black/40 z-40" onclick={onClose} aria-hidden="true"></div>
	<div
		class="fixed right-0 top-0 bottom-0 w-full sm:w-[420px] bg-bg-surface border-l border-border z-50 overflow-y-auto p-4 flex flex-col gap-4"
		role="dialog"
		aria-label="Detail tiket"
		aria-modal="true"
	>
		<div class="flex items-center justify-between">
			<h2 class="font-heading font-bold text-text-primary text-sm">Detail Tiket</h2>
			<button
				type="button"
				bind:this={closeButtonEl}
				onclick={onClose}
				aria-label="Tutup panel detail"
				class="min-w-[44px] min-h-[44px] flex items-center justify-center rounded-lg text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<X size={18} aria-hidden="true" />
			</button>
		</div>

		{#if loading}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:else if errorMsg}
			<p role="alert" aria-live="polite" class="text-[12px] text-danger">{errorMsg}</p>
		{:else if detail}
			<dl class="grid grid-cols-2 gap-x-3 gap-y-2 text-[12px] font-body">
				<dt class="text-text-muted">SPX ID</dt>
				<dd class="font-mono text-text-primary">{detail.spxId}</dd>
				<dt class="text-text-muted">Status</dt>
				<dd class="text-text-primary">{detail.status}</dd>
				{#if detail.status === 'failed' && detail.failureReason !== null}
					<dt class="text-text-muted">Alasan Gagal</dt>
					<dd class="text-danger">{failureReasonLabel(detail.failureReason)}</dd>
				{/if}
				<dt class="text-text-muted">Rute</dt>
				<dd class="text-text-primary">{detail.route.join(' → ') || '—'}</dd>
				<dt class="text-text-muted">Layanan</dt>
				<dd class="text-text-primary">{detail.serviceType ?? '—'}</dd>
				<dt class="text-text-muted">Berat</dt>
				<dd class="font-mono text-text-primary">{detail.weight.toFixed(1)} kg</dd>
				<dt class="text-text-muted">COD</dt>
				<dd class="font-mono text-text-primary">
					{detail.codAmount > 0 ? detail.codAmount.toLocaleString('id-ID') : '—'}
				</dd>
				<dt class="text-text-muted">COC</dt>
				<dd class="text-text-primary">{detail.isCoc ? 'Ya' : 'Tidak'}</dd>
				<dt class="text-text-muted">Otomatis</dt>
				<dd class="text-text-primary">{detail.autoAccepted ? 'Ya' : 'Tidak'}</dd>
				{#if detail.acceptLatencyMs !== null}
					<dt class="text-text-muted">Latency</dt>
					<dd class="font-mono text-live">{detail.acceptLatencyMs}ms</dd>
				{/if}
			</dl>

			<div>
				<h3 class="font-heading font-bold text-text-primary text-[12px] mb-2">Riwayat Percobaan</h3>
				{#if auditTrail.length === 0}
					<p class="text-[11px] text-text-muted">Belum ada percobaan tercatat.</p>
				{:else}
					<ul class="flex flex-col gap-2">
						{#each auditTrail as event (event.id)}
							<li class="p-2.5 rounded-md border border-border text-[11px] font-body">
								<div class="flex justify-between">
									<span class="text-text-primary font-semibold">{event.outcome}</span>
									<span class="font-mono text-text-muted">{new Date(event.createdAt).toLocaleTimeString('id-ID')}</span>
								</div>
								{#if event.localDispatchUs !== null || event.acceptE2eMs !== null}
									<div class="font-mono text-text-muted mt-1">
										{#if event.localDispatchUs !== null}<span>decision: {(event.localDispatchUs / 1000).toFixed(2)}ms</span>{/if}
										{#if event.acceptE2eMs !== null}<span class="ml-2">e2e: {event.acceptE2eMs}ms</span>{/if}
									</div>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			</div>
		{/if}
	</div>
{/if}
