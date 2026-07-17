<!-- Frontend/src/lib/components/TopNav.svelte -->
<!-- Horizontal top-bar nav for the (app) route group. /tickets, /rules, /price, /settings,
     /activity 404 until their own sub-fases build them — same disclosed pattern /command itself
     had before Task 8; this task's job is the shell, not those pages. -->
<script lang="ts">
	import { page } from '$app/state';
	import HealthPill from './HealthPill.svelte';
	import type { WsStatus } from '$lib/ws.svelte';

	let { wsStatus }: { wsStatus: WsStatus } = $props();

	const NAV_ITEMS = [
		{ href: '/command', label: 'Command' },
		{ href: '/tickets', label: 'Tickets' },
		{ href: '/rules', label: 'Rules' },
		{ href: '/price', label: 'Price' },
		{ href: '/settings', label: 'Settings' },
		{ href: '/activity', label: 'Activity' }
	];
</script>

<nav
	class="h-12 border-b border-border bg-bg-surface flex items-center px-4 gap-5 overflow-x-auto"
	aria-label="Navigasi utama"
>
	<span class="font-heading font-bold text-text-primary text-sm shrink-0">TOWER</span>
	<ul class="flex gap-4 text-xs font-body shrink-0">
		{#each NAV_ITEMS as item (item.href)}
			<li>
				<a
					href={item.href}
					class="inline-block py-3.5 border-b-2 transition-colors min-h-[44px] flex items-center focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent
						{page.url.pathname.startsWith(item.href)
						? 'border-accent text-accent'
						: 'border-transparent text-text-muted hover:text-text-primary'}"
					aria-current={page.url.pathname.startsWith(item.href) ? 'page' : undefined}
				>
					{item.label}
				</a>
			</li>
		{/each}
	</ul>
	<div class="ml-auto flex items-center gap-3 shrink-0">
		<HealthPill status={wsStatus} />
		<button
			type="button"
			aria-label="Notifikasi"
			class="w-9 h-9 min-w-[44px] min-h-[44px] flex items-center justify-center rounded-lg text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			<span aria-hidden="true">&#128276;</span>
		</button>
	</div>
</nav>
