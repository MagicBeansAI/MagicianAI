<script lang="ts">
	/**
	 * Shared `@`-mention picker list. Presentational only — the composer owns the
	 * state machine (see `composerMentions.ts`) and positions this via a wrapper.
	 * Used by FloatingComposer.
	 */
	import { createEventDispatcher } from 'svelte';
	import { mentionKindLabel, type ComposerMentionItem } from './composerMentions';

	export let matches: ComposerMentionItem[] = [];
	export let activeIndex = 0;

	const dispatch = createEventDispatcher<{ select: ComposerMentionItem }>();
</script>

<div class="mention-picker" role="listbox" aria-label="Insert reference">
	{#if matches.length > 0}
		{#each matches as item, index (item.id)}
			<button
				type="button"
				class="mention-option"
				class:active={index === activeIndex}
				role="option"
				aria-selected={index === activeIndex}
				on:mousedown|preventDefault
				on:click={() => dispatch('select', item)}
			>
				<span class="mention-badge">{mentionKindLabel(item.kind)}</span>
				<span class="mention-copy">
					<span class="mention-label">{item.label}</span>
					{#if item.detail}
						<span class="mention-detail">{item.detail}</span>
					{/if}
				</span>
			</button>
		{/each}
	{:else}
		<div class="mention-empty">No matching names</div>
	{/if}
</div>

<style>
	.mention-picker {
		padding: 6px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 10px;
		background: color-mix(in srgb, var(--bg-elevated, #fff) 92%, var(--accent-primary, #c2502a) 8%);
		box-shadow: var(--shadow-md, 0 14px 32px -18px rgba(0, 0, 0, 0.35));
		max-height: 246px;
		overflow: auto;
	}

	.mention-option {
		width: 100%;
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		align-items: center;
		gap: 10px;
		padding: 8px 9px;
		border: 0;
		border-radius: 7px;
		background: transparent;
		color: var(--text-primary, #1a1a1a);
		text-align: left;
		cursor: pointer;
	}

	.mention-option:hover,
	.mention-option.active {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
	}

	.mention-badge {
		min-width: 70px;
		text-align: center;
		font-family: var(--font-mono);
		font-size: 10px;
		line-height: 1;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--accent-primary, #c2502a);
		border: 1px solid var(--accent-border, rgba(194, 80, 42, 0.28));
		border-radius: 999px;
		background: var(--accent-soft, rgba(194, 80, 42, 0.1));
		padding: 4px 7px;
	}

	.mention-copy {
		display: grid;
		gap: 2px;
		min-width: 0;
	}

	.mention-label {
		font-family: var(--font-primary);
		font-size: 13px;
		font-weight: 650;
		color: var(--text-primary, #1a1a1a);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.mention-detail {
		font-family: var(--font-primary);
		font-size: 11.5px;
		line-height: 1.25;
		color: var(--text-muted, #888);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.mention-empty {
		padding: 10px;
		font-family: var(--font-primary);
		font-size: 12px;
		color: var(--text-muted, #888);
	}
</style>
