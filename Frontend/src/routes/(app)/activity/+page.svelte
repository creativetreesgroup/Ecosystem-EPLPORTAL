<!-- Frontend/src/routes/(app)/activity/+page.svelte -->
<!-- /activity: two tabs with genuinely different pagination models — Riwayat Keputusan
     (accept_events) is server-paginated (unbounded, growing table); Log Bot (bot_log) fetches its
     full <=200-entry list once and paginates client-side (backend-capped, bounded). The Log Bot
     tab button itself is content-gated (only rendered for is_main_account), not just its
     mutations — matching GET /bot/logs' own ManageBotSettings requirement on the READ path, not
     just the DELETE path, unlike /rules'/`/price`'s view-for-all-edit-for-main-account pattern. -->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { PageProps } from './$types';
	import {
		fetchAcceptEvents,
		fetchBotLogs,
		clearBotLogs,
		type AcceptEventRow,
		type BotLogRow
	} from '$lib/api-activity';
	import AcceptEventRowItem from '$lib/components/AcceptEventRow.svelte';
	import BotLogRowItem from '$lib/components/BotLogRow.svelte';
	import Pagination from '$lib/components/Pagination.svelte';

	let { data }: PageProps = $props();
	const canViewBotLog = $derived(data.user.is_main_account);

	type Tab = 'events' | 'botlog';
	let activeTab = $state<Tab>('events');

	// Riwayat Keputusan (accept_events) — server-side pagination, real fetch on every page change.
	let eventRows = $state<AcceptEventRow[]>([]);
	let eventPage = $state(1);
	let eventHasMore = $state(false);
	let eventsLoading = $state(true);
	let eventsError = $state('');

	async function loadEvents() {
		eventsLoading = true;
		eventsError = '';
		try {
			const result = await fetchAcceptEvents(eventPage);
			eventRows = result.rows;
			eventHasMore = result.hasMore;
		} catch {
			eventsError = 'Gagal memuat riwayat keputusan. Coba lagi.';
		} finally {
			eventsLoading = false;
		}
	}

	function handleEventPageChange(next: number) {
		eventPage = next;
		loadEvents();
	}

	// Log Bot (bot_log) — one fetch of the full (<=200-entry) list, client-side pagination after.
	const BOT_LOG_PAGE_SIZE = 20;
	let botLogAll = $state<BotLogRow[]>([]);
	let botLogPage = $state(1);
	let botLogLoading = $state(false);
	let botLogError = $state('');
	let botLogLoaded = $state(false);

	async function loadBotLogs() {
		botLogLoading = true;
		botLogError = '';
		try {
			botLogAll = await fetchBotLogs();
			botLogLoaded = true;
		} catch {
			botLogError = 'Gagal memuat log bot. Coba lagi.';
		} finally {
			botLogLoading = false;
		}
	}

	const botLogPageCount = $derived(Math.max(1, Math.ceil(botLogAll.length / BOT_LOG_PAGE_SIZE)));
	const botLogPageRows = $derived(
		botLogAll.slice((botLogPage - 1) * BOT_LOG_PAGE_SIZE, botLogPage * BOT_LOG_PAGE_SIZE)
	);
	const botLogHasMore = $derived(botLogPage < botLogPageCount);

	async function handleClearBotLogs() {
		if (!confirm('Hapus semua log bot? Tindakan ini tidak dapat dibatalkan.')) return;
		try {
			await clearBotLogs();
			botLogAll = [];
			botLogPage = 1;
		} catch {
			botLogError = 'Gagal menghapus log bot. Coba lagi.';
		}
	}

	// Only fetch bot logs the first time that tab is actually opened, not on every tab switch.
	function selectTab(tab: Tab) {
		activeTab = tab;
		if (tab === 'botlog' && !botLogLoaded) {
			loadBotLogs();
		}
	}

	onMount(loadEvents);
</script>

<svelte:head>
	<title>Activity — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-4xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Activity</h1>

	<div class="flex gap-2" role="tablist" aria-label="Activity tabs">
		<button
			type="button"
			role="tab"
			aria-selected={activeTab === 'events'}
			onclick={() => selectTab('events')}
			class={`min-h-[44px] px-4 rounded-md text-[13px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
				activeTab === 'events'
					? 'bg-accent text-bg-base border-accent'
					: 'bg-bg-surface text-text-muted border-border hover:text-text-primary'
			}`}
		>
			Riwayat Keputusan
		</button>
		{#if canViewBotLog}
			<button
				type="button"
				role="tab"
				aria-selected={activeTab === 'botlog'}
				onclick={() => selectTab('botlog')}
				class={`min-h-[44px] px-4 rounded-md text-[13px] font-body border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
					activeTab === 'botlog'
						? 'bg-accent text-bg-base border-accent'
						: 'bg-bg-surface text-text-muted border-border hover:text-text-primary'
				}`}
			>
				Log Bot
			</button>
		{/if}
	</div>

	{#if activeTab === 'events'}
		{#if eventsError}
			<div
				role="alert"
				aria-live="polite"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
			>
				{eventsError}
			</div>
		{/if}
		{#if eventsLoading}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:else}
			<div class="flex flex-col gap-2">
				{#each eventRows as event (event.id)}
					<AcceptEventRowItem {event} />
				{/each}
			</div>
			<Pagination page={eventPage} hasMore={eventHasMore} onPageChange={handleEventPageChange} />
		{/if}
	{:else if activeTab === 'botlog'}
		{#if botLogError}
			<div
				role="alert"
				aria-live="polite"
				class="px-3.5 py-2.5 rounded-lg text-[13px] font-medium font-body border bg-danger/10 text-danger border-danger/30"
			>
				{botLogError}
			</div>
		{/if}
		{#if botLogLoading}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:else}
			<button
				type="button"
				onclick={handleClearBotLogs}
				disabled={botLogAll.length === 0}
				class="min-h-[44px] px-4 rounded-md text-[13px] font-body border border-danger/30 text-danger disabled:opacity-40 self-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				Hapus Log
			</button>
			<div class="flex flex-col gap-2">
				<!-- BotLogEntry has no id field at all — keyed on ts+index. Safe here specifically
				     because this list is never locally reordered/mutated item-by-item (only
				     wholesale replaced on fetch or cleared to empty on delete), so index-inclusion
				     in the key causes no correctness issue despite the usual "don't key on index"
				     caution — it only guards against a same-millisecond ts collision. -->
				{#each botLogPageRows as entry, i (`${entry.ts}-${i}`)}
					<BotLogRowItem {entry} />
				{/each}
			</div>
			<Pagination page={botLogPage} hasMore={botLogHasMore} onPageChange={(next) => (botLogPage = next)} />
		{/if}
	{/if}
</div>
