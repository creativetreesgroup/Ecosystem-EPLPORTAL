<!-- Frontend/src/lib/components/Pagination.svelte -->
<!-- Deliberately generic (no /tickets-specific naming/imports) — page + hasMore + onPageChange
     is the same shape Rules/Price/Activity sub-fases will need, so this is meant to be reused
     as-is rather than re-authored per surface. -->
<script lang="ts">
	import { ChevronLeft, ChevronRight } from '@lucide/svelte';

	let {
		page,
		hasMore,
		onPageChange
	}: { page: number; hasMore: boolean; onPageChange: (page: number) => void } = $props();
</script>

<nav class="flex items-center justify-between gap-3 py-2" aria-label="Navigasi halaman">
	<button
		type="button"
		disabled={page <= 1}
		onclick={() => onPageChange(page - 1)}
		aria-label="Halaman sebelumnya"
		class="min-h-[44px] min-w-[44px] flex items-center justify-center rounded-md border border-border text-text-muted hover:text-text-primary disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		<ChevronLeft size={18} aria-hidden="true" />
	</button>
	<span class="text-[12px] font-mono text-text-muted" aria-current="page">Halaman {page}</span>
	<button
		type="button"
		disabled={!hasMore}
		onclick={() => onPageChange(page + 1)}
		aria-label="Halaman berikutnya"
		class="min-h-[44px] min-w-[44px] flex items-center justify-center rounded-md border border-border text-text-muted hover:text-text-primary disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		<ChevronRight size={18} aria-hidden="true" />
	</button>
</nav>
