<script lang="ts">
	let {
		label,
		value,
		active = false,
		onclick
	}: { label: string; value: string; active?: boolean; onclick?: () => void } = $props();
</script>

<button
	type="button"
	{onclick}
	aria-pressed={onclick ? active : undefined}
	class="min-h-[44px] flex flex-col gap-1 p-3.5 rounded-lg border text-left transition-colors
		{active ? 'border-accent bg-accent/10' : 'border-border bg-bg-surface hover:bg-bg-base'}
		focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
>
	<span class="font-mono text-xl font-semibold text-text-primary">{value}</span>
	<span class="text-[11px] font-body text-text-muted">{label}</span>
</button>
