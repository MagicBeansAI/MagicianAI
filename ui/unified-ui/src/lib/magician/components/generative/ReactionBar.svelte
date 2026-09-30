<script lang="ts">
	/**
	 * ReactionBar Component — GD-F03-A
	 *
	 * Display-only emoji reactions bar.
	 */

	interface Reaction {
		emoji: string;
		count: number;
		reacted?: boolean;
	}

	export let reactions: unknown = [];
	export let showCount: unknown = true;

	function isReaction(item: unknown): item is Reaction {
		return (
			typeof item === 'object' &&
			item !== null &&
			typeof (item as Record<string, unknown>).emoji === 'string' &&
			typeof (item as Record<string, unknown>).count === 'number'
		);
	}

	function normalizeReactions(value: unknown): Reaction[] {
		if (!Array.isArray(value)) return [];
		return value.filter(isReaction);
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	function formatCount(count: number): string {
		if (count >= 1000000) return `${(count / 1000000).toFixed(1)}M`;
		if (count >= 1000) return `${(count / 1000).toFixed(1)}K`;
		return String(count);
	}

	$: safeReactions = normalizeReactions(reactions);
	$: safeShowCount = toBoolean(showCount);
</script>

{#if safeReactions.length > 0}
	<div class="muij-reaction-bar" role="group" aria-label="Reactions">
		{#each safeReactions as reaction}
			<span
				class="muij-reaction"
				class:muij-reaction-active={reaction.reacted}
				aria-label="{reaction.emoji} reaction, {reaction.count} count"
			>
				<span class="muij-reaction-emoji">{reaction.emoji}</span>
				{#if safeShowCount}
					<span class="muij-reaction-count">{formatCount(reaction.count)}</span>
				{/if}
			</span>
		{/each}
	</div>
{:else}
	<div class="muij-reaction-bar-empty">
		<span class="muij-reaction-bar-empty-text">No reactions</span>
	</div>
{/if}

<style>
	.muij-reaction-bar {
		display: inline-flex;
		flex-wrap: wrap;
		gap: var(--space-xs);
		font-family: var(--font-primary);
	}

	.muij-reaction-bar-empty {
		color: var(--text-muted);
		font-size: 0.75rem;
	}

	.muij-reaction {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.125rem 0.375rem;
		background: var(--bg-soft);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg);
		font-size: 0.8125rem;
		cursor: default;
	}

	.muij-reaction-active {
		background: color-mix(in srgb, var(--accent-primary) 15%, white);
		border-color: var(--accent-primary);
	}

	.muij-reaction-emoji {
		line-height: 1;
	}

	.muij-reaction-count {
		font-size: 0.6875rem;
		font-weight: 500;
		color: var(--text-secondary);
	}

	.muij-reaction-active .muij-reaction-count {
		color: var(--accent-primary);
	}
</style>
