<!-- Shared shell for every /settings/* sub-route. The nav array grows by one entry per future
     sub-phase (Locations, Sub-users, SPX Credentials — Fase 7i-7k) — no placeholder entries for
     resources that don't exist yet, matching TopNav.svelte's own established convention of not
     building UI for not-yet-built surfaces. "Bot" is main-account-only (unlike "Branding"):
     GET /bot/settings itself requires Permission::ManageBotSettings, so a non-main-account
     session must never even see this nav entry — matching Fase 7f's Log Bot tab-hiding pattern,
     not Fase 7g's Branding read-only-view pattern. -->
<script lang="ts">
	import { page } from '$app/state';
	import type { LayoutProps } from './$types';

	let { children, data }: LayoutProps = $props();

	const NAV_ITEMS = $derived(
		data.user.is_main_account
			? [
					{ href: '/settings/branding', label: 'Branding' },
					{ href: '/settings/bot', label: 'Bot' }
				]
			: [{ href: '/settings/branding', label: 'Branding' }]
	);
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
