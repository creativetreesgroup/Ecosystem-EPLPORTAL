<!-- Frontend/src/lib/components/AcceptEventRow.svelte -->
<!-- One accept_events row: collapsed summary + expand-in-place raw JSON detail. Read-only by
     nature (accept_events is append-only at the DB level — app_role has no UPDATE/DELETE grant
     on this table), so this is the simplest expand-in-place component in this codebase: exactly
     one focusable element, no nested controls, no form fields. -->
<script lang="ts">
	import { ChevronDown, ChevronRight } from '@lucide/svelte';
	import { outcomeLabel, formatTimestamp, formatMicroseconds, formatMilliseconds } from '$lib/activity';
	import type { AcceptEventRow } from '$lib/api-activity';

	let { event }: { event: AcceptEventRow } = $props();

	let expanded = $state(false);
</script>

<div class="rounded-lg border border-border bg-bg-surface">
	<div
		role="button"
		tabindex="0"
		aria-expanded={expanded}
		onclick={() => (expanded = !expanded)}
		onkeydown={(e) => {
			if (e.key === 'Enter' || e.key === ' ') {
				e.preventDefault();
				expanded = !expanded;
			}
		}}
		class="flex items-center gap-3 p-3 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		{#if expanded}
			<ChevronDown size={14} aria-hidden="true" class="text-text-muted shrink-0" />
		{:else}
			<ChevronRight size={14} aria-hidden="true" class="text-text-muted shrink-0" />
		{/if}
		<span
			class="text-[10px] font-body px-1.5 py-0.5 rounded bg-bg-base border border-border text-text-primary uppercase shrink-0"
		>
			{outcomeLabel(event.outcome)}
		</span>
		<span class="text-[11px] font-mono text-text-muted flex-1 truncate">
			{event.bookingId ?? '—'}
		</span>
		<span class="text-[11px] font-mono text-text-muted shrink-0">{formatMicroseconds(event.localDispatchUs)}</span>
		<span class="text-[11px] font-mono text-text-muted shrink-0">{formatMilliseconds(event.acceptE2eMs)}</span>
		<span class="text-[11px] font-body text-text-muted shrink-0">{formatTimestamp(event.createdAt)}</span>
	</div>

	{#if expanded}
		<div class="p-3 pt-0 flex flex-col gap-2">
			<div class="grid grid-cols-2 gap-2 text-[11px] font-mono text-text-muted">
				<span>ID: {event.id}</span>
				<span>Rule ID: {event.ruleId ?? '—'}</span>
			</div>
			<pre
				class="text-[11px] font-mono text-text-primary bg-bg-base border border-border rounded-md p-2 overflow-x-auto">{JSON.stringify(
					event.detail,
					null,
					2
				)}</pre>
		</div>
	{/if}
</div>
