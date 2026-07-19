<!-- Frontend/src/lib/components/BotLogRow.svelte -->
<!-- One bot_log entry: a single flat row, no expand/interactive affordance at all — every field
     on a BotLogEntry is already scalar (unlike accept_events' detail JSONB), so there's nothing
     to disclose. -->
<script lang="ts">
	import { logTypeLabel, kindLabel, formatTimestamp, formatMilliseconds } from '$lib/activity';
	import type { BotLogRow } from '$lib/api-activity';

	let { entry }: { entry: BotLogRow } = $props();
</script>

<div class="flex items-center gap-3 p-3 rounded-lg border border-border bg-bg-surface">
	<span
		class={`text-[10px] font-body px-1.5 py-0.5 rounded uppercase border shrink-0 ${
			entry.logType === 'error' ? 'bg-danger/10 text-danger border-danger/30' : 'bg-live/10 text-live border-live/30'
		}`}
	>
		{logTypeLabel(entry.logType)}
	</span>
	<span class="text-[11px] font-body text-text-muted shrink-0">{kindLabel(entry.kind)}</span>
	<span class="text-[11px] font-mono text-text-muted flex-1 truncate">
		{entry.bookingId ?? entry.rule ?? '—'}
	</span>
	<span class="text-[11px] font-mono text-text-muted shrink-0">{formatMilliseconds(entry.latencyMs)}</span>
	{#if entry.error}
		<span class="text-[11px] font-body text-danger truncate max-w-[240px]" title={entry.error}>{entry.error}</span>
	{/if}
	<span class="text-[11px] font-body text-text-muted shrink-0">{formatTimestamp(new Date(entry.ts))}</span>
</div>
