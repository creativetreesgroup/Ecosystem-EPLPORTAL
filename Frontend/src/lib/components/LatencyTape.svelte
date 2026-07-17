<!-- Frontend/src/lib/components/LatencyTape.svelte -->
<script lang="ts">
	// Canvas-based scope-trace visualization of local_dispatch_us samples — the "phosphor
	// oscilloscope" component validated in Fase 7b's brainstorming. Respects
	// prefers-reduced-motion: renders one static frame instead of a continuous animation loop
	// when the media query matches (checked once on mount; this page doesn't need to react to
	// the preference changing mid-session).
	let { samples }: { samples: number[] } = $props();

	let canvasEl: HTMLCanvasElement | undefined = $state();
	const BUDGET_US = 1000; // 1ms — spikes above this render in --color-accent, not --color-live.

	function readCssColor(varName: string): string {
		return getComputedStyle(document.documentElement).getPropertyValue(varName).trim();
	}

	function draw() {
		if (!canvasEl) return;
		const ctx = canvasEl.getContext('2d');
		if (!ctx) return;
		const { width, height } = canvasEl;
		ctx.clearRect(0, 0, width, height);
		if (samples.length < 2) return;

		const maxSample = Math.max(...samples, BUDGET_US * 1.2);
		const stepX = width / (samples.length - 1);
		const liveColor = readCssColor('--color-live');
		const accentColor = readCssColor('--color-accent');

		ctx.beginPath();
		ctx.strokeStyle = liveColor;
		ctx.lineWidth = 2;
		ctx.shadowColor = liveColor;
		ctx.shadowBlur = 6;
		samples.forEach((sample, i) => {
			const x = i * stepX;
			const y = height - (sample / maxSample) * height;
			if (i === 0) ctx.moveTo(x, y);
			else ctx.lineTo(x, y);
		});
		ctx.stroke();

		// Mark spikes over budget with a separate glowing dot, not part of the continuous stroke.
		samples.forEach((sample, i) => {
			if (sample <= BUDGET_US) return;
			const x = i * stepX;
			const y = height - (sample / maxSample) * height;
			ctx.beginPath();
			ctx.fillStyle = accentColor;
			ctx.shadowColor = accentColor;
			ctx.shadowBlur = 8;
			ctx.arc(x, y, 3, 0, Math.PI * 2);
			ctx.fill();
		});
	}

	$effect(() => {
		const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		draw();
		if (reducedMotion) return; // static single frame, no redraw loop
		// samples is a $state-tracked prop from the parent; re-running this effect whenever it
		// changes (Svelte 5 tracks `samples` access inside draw() automatically) IS the redraw
		// loop — no requestAnimationFrame needed since updates are event-driven (new WS samples),
		// not a continuous clock-driven animation.
	});

	const p99 = $derived.by(() => {
		if (samples.length === 0) return 0;
		const sorted = [...samples].sort((a, b) => a - b);
		const idx = Math.floor(sorted.length * 0.99);
		return sorted[Math.min(idx, sorted.length - 1)];
	});
</script>

<div class="rounded-lg border border-border bg-bg-surface p-4">
	<canvas bind:this={canvasEl} width="600" height="140" class="w-full" aria-hidden="true"></canvas>
	<div class="flex items-baseline gap-2 mt-2">
		<span class="font-mono text-live text-2xl font-semibold">
			{(p99 / 1000).toFixed(2)}<span class="text-xs text-text-muted">ms p99</span>
		</span>
	</div>
	<p class="sr-only" aria-live="polite">Latency keputusan p99: {(p99 / 1000).toFixed(2)} milidetik</p>
</div>
