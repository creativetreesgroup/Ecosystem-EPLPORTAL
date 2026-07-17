<!-- Frontend/src/lib/components/NotificationCenter.svelte -->
<script lang="ts">
	import { Bell } from '@lucide/svelte';

	// Shell only (Fase 7b design doc's disclosed scope) — real notification data source
	// (notifier crate / notifications table, never used by production code so far) needs its
	// own design pass in a future sub-fase. This is the icon + empty panel only.
	let open = $state(false);
</script>

<div class="relative">
	<button
		type="button"
		aria-label="Notifikasi"
		aria-expanded={open}
		onclick={() => (open = !open)}
		class="w-9 h-9 min-w-[44px] min-h-[44px] flex items-center justify-center rounded-lg text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		<Bell size={18} aria-hidden="true" />
	</button>
	{#if open}
		<div
			class="absolute right-0 mt-2 w-64 rounded-lg border border-border bg-bg-surface shadow-lg p-4 text-xs text-text-muted"
			role="dialog"
			aria-label="Panel notifikasi"
		>
			Belum ada notifikasi.
		</div>
	{/if}
</div>
