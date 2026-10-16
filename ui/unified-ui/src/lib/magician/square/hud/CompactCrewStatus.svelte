<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';

	export let working = 0;
	export let total = 0;
	export let attentionCount = 0;
	export let backendDown = false;
	export let spendUsd: number | null = null;
	export let summaryOpen = false;

	const dispatch = createEventDispatcher<{
		summary: void;
		crew: void;
		attention: void;
		usage: void;
	}>();
</script>

<section class="compact-status" aria-label="Crew status">
	<span class="compact-status__item" class:compact-status__item--danger={backendDown} title="Backend connection">
		<span class="compact-status__pulse" aria-hidden="true"></span>
		<strong>{backendDown ? 'Offline' : 'Online'}</strong>
	</span>
	<button type="button" class="compact-status__item" title="Open crew" on:click={() => dispatch('crew')}>
		<Icon name="zap" size={15} />
		<strong>{working}</strong><span>/ {total} active</span>
	</button>
	<button
		type="button"
		class="compact-status__item"
		class:compact-status__item--attention={attentionCount > 0}
		title="Open Attention"
		on:click={() => dispatch('attention')}
	>
		<Icon name="alert" size={15} />
		<strong>{attentionCount}</strong><span>need you</span>
	</button>
	<button type="button" class="compact-status__item" title="Open usage and cost" on:click={() => dispatch('usage')}>
		<Icon name="archive" size={15} />
		<strong>{spendUsd == null ? '—' : `$${spendUsd.toFixed(2)}`}</strong><span>7d</span>
	</button>
	<button
		type="button"
		class="compact-status__expand"
		class:compact-status__expand--active={summaryOpen}
		aria-label={summaryOpen ? 'Hide current work summary' : 'Show current work summary'}
		aria-expanded={summaryOpen}
		title={summaryOpen ? 'Hide current work summary' : 'Show current work summary'}
		on:click={() => dispatch('summary')}
	>
		<Icon name={summaryOpen ? 'chevron-up' : 'chevron-down'} size={16} />
	</button>
</section>

<style>
	.compact-status {
		pointer-events: auto;
		position: absolute;
		top: var(--game-hud-gutter, 0.75rem);
		left: 50%;
		z-index: var(--game-layer-hud, 30);
		display: flex;
		align-items: stretch;
		min-height: 2.35rem;
		border: 1px solid var(--game-border-strong);
		border-radius: var(--game-radius-md);
		background: color-mix(in srgb, var(--game-material-panel) 94%, transparent);
		box-shadow: var(--game-shadow-hud);
		color: var(--game-text);
		transform: translateX(-50%);
		backdrop-filter: blur(12px);
	}
	.compact-status__item,
	.compact-status__expand {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.32rem;
		min-width: 0;
		padding: 0 0.65rem;
		border: 0;
		border-right: 1px solid var(--game-border-soft);
		background: transparent;
		color: var(--game-text-muted);
		font: inherit;
		font-size: var(--game-type-2);
		white-space: nowrap;
	}
	button.compact-status__item,
	.compact-status__expand { cursor: pointer; }
	button.compact-status__item:hover,
	button.compact-status__item:focus-visible,
	.compact-status__expand:hover,
	.compact-status__expand:focus-visible,
	.compact-status__expand--active {
		background: var(--game-material-selected);
		color: var(--game-text);
		outline: none;
	}
	.compact-status__item strong { color: var(--game-text); }
	.compact-status__item--attention,
	.compact-status__item--attention strong { color: var(--game-state-attention); }
	.compact-status__item--danger,
	.compact-status__item--danger strong { color: var(--game-state-danger); }
	.compact-status__pulse {
		width: 0.48rem;
		height: 0.48rem;
		border-radius: 50%;
		background: var(--game-state-success);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--game-state-success) 16%, transparent);
	}
	.compact-status__item--danger .compact-status__pulse {
		background: var(--game-state-danger);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--game-state-danger) 16%, transparent);
	}
	.compact-status__expand {
		width: 2.35rem;
		padding: 0;
		border-right: 0;
	}
</style>
