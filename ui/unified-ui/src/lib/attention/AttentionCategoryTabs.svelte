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
	export let compact = false;

	const dispatch = createEventDispatcher<{
		change: { category: AttentionCategory };
	}>();
</script>

<nav class="attention-tabs" class:attention-tabs--compact={compact} aria-label="Attention categories">
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
	.attention-tabs.attention-tabs--compact {
		gap: 0.625rem;
		/* Room for the 3px scrollbar so it never sits on the tab labels. */
		padding-bottom: 4px;
	}
	.attention-tabs--compact .attention-tabs__tab {
		min-height: 26px;
		font-size: 0.6875rem;
		gap: 0.1875rem;
	}
	.attention-tabs--compact .attention-tabs__count {
		font-size: 0.625rem;
		min-width: auto;
	}
	/* A thin, themed scrollbar instead of the platform's grey bar, which in
	   the notch panel covered the bottom of the tabs once counts grew. */
	.attention-tabs::-webkit-scrollbar {
		height: 3px;
	}
	.attention-tabs::-webkit-scrollbar-track {
		background: transparent;
	}
	.attention-tabs::-webkit-scrollbar-thumb {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 38%, transparent);
		border-radius: 999px;
	}
	.attention-tabs:hover::-webkit-scrollbar-thumb {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 62%, transparent);
	}
	/* The standard properties override ::-webkit-scrollbar where both exist,
	   so they only style engines without it. */
	@supports not selector(::-webkit-scrollbar) {
		.attention-tabs {
			scrollbar-width: thin;
			scrollbar-color: color-mix(in srgb, var(--accent-primary, #ff6b6b) 45%, transparent) transparent;
		}
	}
	.attention-tabs {
		display: flex;
		align-items: stretch;
		gap: 18px;
		min-width: 0;
		overflow-x: auto;
		overflow-y: hidden;
		padding: 0 2px;
		border-bottom: 1px solid var(--border-soft, rgba(15, 23, 42, 0.12));
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
