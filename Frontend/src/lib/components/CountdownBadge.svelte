<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { formatCountdown } from '$lib/countdown';

	let { target, size = 'lg' }: { target: string | null; size?: 'lg' | 'sm' } = $props();

	let nowMs = $state(Date.now());
	let timer: ReturnType<typeof setInterval> | undefined;

	onMount(() => {
		timer = setInterval(() => (nowMs = Date.now()), 1000);
	});
	onDestroy(() => {
		if (timer) clearInterval(timer);
	});

	const formatted = $derived(target ? formatCountdown(target, nowMs) : null);
</script>

{#if formatted}
	<span
		class={size === 'lg'
			? 'font-mono text-[13px] font-semibold ' + (formatted.expired ? 'text-danger' : 'text-text-primary')
			: 'font-mono text-[10px] px-1.5 py-0.5 rounded ' +
				(formatted.expired ? 'bg-danger/10 text-danger' : 'bg-accent/10 text-accent')}
	>
		{formatted.label}{#if size === 'sm'}&nbsp;STANDBY{/if}
	</span>
{:else}
	<span class="text-text-muted text-[11px]">—</span>
{/if}
