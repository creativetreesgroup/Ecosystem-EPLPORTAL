<script lang="ts">
	import { onMount } from 'svelte';

	let status = $state('checking...');

	onMount(async () => {
		try {
			const res = await fetch('/api/healthz');
			const data = await res.json();
			status = `${data.service}: ${data.status}`;
		} catch {
			status = 'unreachable';
		}
	});
</script>

<main class="flex min-h-screen items-center justify-center bg-neutral-950 text-neutral-100">
	<div class="text-center">
		<h1 class="text-3xl font-bold">TOWER</h1>
		<p class="mt-2 text-sm text-neutral-400">reactor-core health: {status}</p>
	</div>
</main>
