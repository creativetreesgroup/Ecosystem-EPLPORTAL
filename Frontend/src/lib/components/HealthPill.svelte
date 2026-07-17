<!-- Frontend/src/lib/components/HealthPill.svelte -->
<!-- Live WS connection indicator for TopNav. Covers all 4 WsStatus states with a glyph AND a
     text label (never color-only — colorblind/low-vision users can't rely on hue alone), and
     aria-live="polite" so status changes (e.g. connected -> reconnecting) are announced. -->
<script lang="ts">
	import type { WsStatus } from '$lib/ws.svelte';

	let { status }: { status: WsStatus } = $props();

	const CONFIG: Record<WsStatus, { glyph: string; label: string; colorClass: string }> = {
		connected: { glyph: '●', label: 'LIVE', colorClass: 'text-live' },
		connecting: { glyph: '◐', label: 'MENYAMBUNG', colorClass: 'text-accent' },
		reconnecting: { glyph: '◐', label: 'RECONNECTING', colorClass: 'text-accent' },
		disconnected: { glyph: '○', label: 'TERPUTUS', colorClass: 'text-danger' }
	};
	const cfg = $derived(CONFIG[status]);
</script>

<span
	class="inline-flex items-center gap-1.5 text-[10px] font-mono font-semibold {cfg.colorClass}"
	aria-live="polite"
>
	<span aria-hidden="true">{cfg.glyph}</span>
	{cfg.label}
</span>
