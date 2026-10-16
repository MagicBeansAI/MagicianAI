<script lang="ts">
	/**
	 * One preference's history, newest first: what changed, who changed it
	 * and from where. A forgotten preference keeps its dated entries, but its
	 * wording is erased everywhere — shown as "wording erased", never as
	 * blank text that might read as an empty preference.
	 */
	import { onMount } from 'svelte';

	import { fetchPreferenceHistory, type HistoryEntry } from './preferencesApi';

	export let id: string;

	let entries: HistoryEntry[] = [];
	let error = '';
	let loading = true;

	const OP_WORDS: Record<string, string> = {
		added: 'Added',
		edited: 'Reworded',
		confirmed: 'Confirmed',
		superseded: 'Replaced',
		retired: 'Retired',
		restored: 'Restored',
		scope_changed: 'Scope changed',
		forgotten: 'Forgotten'
	};

	const VIA_WORDS: Record<string, string> = {
		owner_surface: 'you',
		memory_tool: 'chat',
		consolidator: 'memory review',
		learning: 'learning',
		alignment: 'a preference check',
		lifecycle: 'memory upkeep',
		memory_api: 'the memory page',
		backfill: 'a migration',
		taste_review: 'taste review',
		unknown: 'unknown'
	};

	function who(entry: HistoryEntry): string {
		const via = VIA_WORDS[entry.via] ?? entry.via;
		return entry.surface ? `${via} on ${entry.surface === 'ios' ? 'iPhone' : entry.surface}` : via;
	}

	onMount(async () => {
		try {
			entries = await fetchPreferenceHistory(id);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : String(cause);
		} finally {
			loading = false;
		}
	});
</script>

<div class="history" data-testid="preference-history">
	{#if loading}
		<p class="muted">Loading history…</p>
	{:else if error}
		<p role="alert">{error}</p>
	{:else if entries.length === 0}
		<p class="muted">No recorded changes yet.</p>
	{:else}
		<ol>
			{#each entries as entry, index (index)}
				<li data-testid="preference-history-entry">
					<span class="when">{new Date(entry.atMs).toLocaleString()}</span>
					<strong>{OP_WORDS[entry.op] ?? entry.op}</strong>
					<span class="muted">by {who(entry)}</span>
					{#if entry.wordingErased}
						<span class="erased">wording erased</span>
					{:else if entry.wordingBefore && entry.wordingAfter && entry.wordingBefore !== entry.wordingAfter}
						<span class="change"><del>{entry.wordingBefore}</del> → {entry.wordingAfter}</span>
					{:else if entry.wordingAfter}
						<span class="change">{entry.wordingAfter}</span>
					{/if}
				</li>
			{/each}
		</ol>
	{/if}
</div>

<style>
	.history {
		margin-top: 0.5rem;
		font-size: 0.85rem;
	}
	ol {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: 0.4rem;
	}
	li {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		align-items: baseline;
		overflow-wrap: anywhere;
	}
	.when,
	.muted {
		color: var(--text-secondary);
	}
	.change {
		flex-basis: 100%;
	}
	.erased {
		font-style: italic;
		color: var(--text-secondary);
	}
	p {
		margin: 0.3rem 0;
	}
</style>
