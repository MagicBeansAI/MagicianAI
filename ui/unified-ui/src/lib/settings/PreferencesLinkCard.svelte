<script lang="ts">
	// Settings shows a summary of the owner's preferences and links to the full
	// editor in Memory → Preferences, so the Settings page stays short.
	import { onMount } from 'svelte';
	import { fetchPreferenceView, type IntentionKind } from '$lib/preferences/preferencesApi';

	const MEMORY_PREFERENCES_HREF = '/memory?tab=preferences';

	let counts = $state<Record<IntentionKind, number> | null>(null);
	let failed = $state(false);

	onMount(async () => {
		try {
			const view = await fetchPreferenceView();
			const next: Record<IntentionKind, number> = { standing_instruction: 0, goal: 0, taste: 0 };
			for (const preference of view.preferences) next[preference.kind] += 1;
			counts = next;
		} catch {
			failed = true;
		}
	});

	function plural(count: number, one: string, many: string): string {
		return `${count} ${count === 1 ? one : many}`;
	}
</script>

<section class="preferences-link-card" aria-labelledby="preferences-link-title">
	<div class="preferences-link-text">
		<p class="overline">Preferences</p>
		<h2 id="preferences-link-title">Your preferences</h2>
		<p>Rules, goals and tastes Magician checks an action against before it sends, books, orders or buys for you.</p>
		{#if counts}
			<p class="counts">
				{plural(counts.standing_instruction, 'rule', 'rules')} · {plural(counts.goal, 'goal', 'goals')} · {plural(counts.taste, 'taste', 'tastes')}
			</p>
		{:else if failed}
			<p class="counts">Preferences are unavailable right now.</p>
		{/if}
	</div>
	<a class="open-link" href={MEMORY_PREFERENCES_HREF}>Open in Memory</a>
</section>

<style>
	.preferences-link-card {
		align-items: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-sizing: border-box;
		display: flex;
		flex-wrap: wrap;
		gap: 1rem;
		justify-content: space-between;
		padding: 1rem;
	}

	.preferences-link-text {
		flex: 1 1 18rem;
		min-width: 0;
	}

	.overline,
	h2,
	p {
		letter-spacing: 0;
		margin: 0;
	}

	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
	}

	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
		margin: 0.2rem 0 0.35rem;
	}

	p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.5;
	}

	.counts {
		color: var(--text-primary);
		font-size: 0.86rem;
		font-weight: 600;
		margin-top: 0.45rem;
	}

	.open-link {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		display: inline-flex;
		font-size: 0.86rem;
		font-weight: 600;
		min-height: 2.25rem;
		padding: 0.55rem 0.8rem;
		text-decoration: none;
		white-space: nowrap;
	}

	.open-link:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 70%, transparent);
		outline-offset: 2px;
	}
</style>
