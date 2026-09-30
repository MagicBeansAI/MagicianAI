<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	import {
		ATTENTION_CATEGORY_LABELS,
		ATTENTION_CATEGORY_ORDER,
		type AttentionCategory
	} from './model';

	export let active: AttentionCategory = 'all';
	export let counts: Record<AttentionCategory, number> = {
		all: 0,
		requests: 0,
		approvals: 0,
		escalations: 0,
		failed: 0
	};
	export let disabled = false;

	const dispatch = createEventDispatcher<{
		change: { category: AttentionCategory };
	}>();
</script>

<nav class="attention-tabs" aria-label="Attention categories">
	{#each ATTENTION_CATEGORY_ORDER as category (category)}
		<button
			type="button"
			class="attention-tabs__tab"
			class:active={active === category}
			aria-current={active === category ? 'page' : undefined}
			disabled={disabled}
			on:click={() => dispatch('change', { category })}
		>
			<span>{ATTENTION_CATEGORY_LABELS[category]}</span>
			<span class="attention-tabs__count">{counts[category] ?? 0}</span>
		</button>
	{/each}
</nav>

<style>
	.attention-tabs {
		display: flex;
		align-items: stretch;
		gap: 18px;
		min-width: 0;
		overflow-x: auto;
		overflow-y: hidden;
		padding: 0 2px;
		border-bottom: 1px solid var(--border-soft, rgba(15, 23, 42, 0.12));
		scrollbar-width: thin;
		overscroll-behavior-inline: contain;
	}

	.attention-tabs__tab {
		position: relative;
		flex: 0 0 auto;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-height: 40px;
		padding: 0 1px;
		border: 0;
		background: transparent;
		color: var(--text-secondary, #5f6769);
		font: inherit;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 620;
		letter-spacing: 0;
		white-space: nowrap;
		cursor: pointer;
	}

	.attention-tabs__tab::after {
		content: '';
		position: absolute;
		left: 0;
		right: 0;
		bottom: -1px;
		height: 2px;
		background: transparent;
	}

	.attention-tabs__tab:hover:not(:disabled),
	.attention-tabs__tab.active {
		color: var(--text-primary, #202426);
	}

	.attention-tabs__tab.active::after {
		background: var(--accent-primary, #ff6b6b);
	}

	.attention-tabs__tab:focus-visible {
		outline: 2px solid var(--accent-primary, #ff6b6b);
		outline-offset: -3px;
	}

	.attention-tabs__tab:disabled {
		cursor: default;
		opacity: 0.55;
	}

	.attention-tabs__count {
		min-width: 18px;
		font-size: var(--text-xs, 0.78rem);
		font-variant-numeric: tabular-nums;
		color: var(--text-muted, #7b8387);
	}
</style>
