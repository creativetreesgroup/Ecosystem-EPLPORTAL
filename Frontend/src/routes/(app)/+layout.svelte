<!-- Frontend/src/routes/(app)/+layout.svelte -->
<!-- Session-gated app shell (see +layout.server.ts for the redirect-if-unauthenticated check).
     One shared WS connection for the whole authenticated app: setContext('ws', ws) makes it
     available to /command and every future page in this route group via getContext('ws'),
     rather than each page opening its own socket. -->
<script lang="ts">
	import TopNav from '$lib/components/TopNav.svelte';
	import { createWsStore } from '$lib/ws.svelte';
	import { setContext } from 'svelte';

	let { children } = $props();

	const ws = createWsStore();
	setContext('ws', ws);
</script>

<div class="min-h-screen bg-bg-base">
	<TopNav wsStatus={ws.status} />
	<main>
		{@render children()}
	</main>
</div>
