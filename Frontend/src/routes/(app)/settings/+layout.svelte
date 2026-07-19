<!-- Shared shell for every /settings/* sub-route. NAV_ITEMS is a flat array with a per-item
     mainAccountOnly flag, filtered by data.user.is_main_account — refactored from the previous
     if/else ternary (tracked as a Minor in Fase 7h's whole-branch review) now that a SECOND
     always-visible entry ("Lokasi") needs adding; the ternary would have needed updating in both
     branches for every future always-visible entry. Scales cleanly to Fase 7j (Sub-users,
     main-account-only like Bot) and 7k (SPX Credentials, open like Branding/Locations) without
     further restructuring. No placeholder entries for resources that don't exist yet, matching
     TopNav.svelte's own established convention. -->
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { children, data }: LayoutProps = $props();

	type NavItem = { href: string; label: string; mainAccountOnly?: boolean };

	const ALL_NAV_ITEMS: NavItem[] = [
		{ href: '/settings/branding', label: 'Branding' },
		{ href: '/settings/bot', label: 'Bot', mainAccountOnly: true },
		{ href: '/settings/locations', label: 'Lokasi' }
	];

	const NAV_ITEMS = $derived(ALL_NAV_ITEMS.filter((item) => !item.mainAccountOnly || data.user.is_main_account));
</script>

<div class="flex flex-col gap-4 p-4">
	<h1 class="font-heading font-bold text-lg text-text-primary">Settings</h1>
	<nav class="flex gap-4 border-b border-border" aria-label="Navigasi settings">
		{#each NAV_ITEMS as item (item.href)}
			<a
				href={item.href}
				class="pb-2 border-b-2 text-[13px] font-body min-h-[44px] flex items-center focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent
					{page.url.pathname.startsWith(item.href)
					? 'border-accent text-accent'
					: 'border-transparent text-text-muted hover:text-text-primary'}"
				aria-current={page.url.pathname.startsWith(item.href) ? 'page' : undefined}
			>
				{item.label}
			</a>
		{/each}
	</nav>
	{@render children()}
</div>
