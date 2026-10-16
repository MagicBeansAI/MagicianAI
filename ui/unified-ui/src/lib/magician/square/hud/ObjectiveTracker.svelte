<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { FleetObjective } from '../fleetObjectives';

	export let objective: FleetObjective | null = null;
	export let inspectorOpen = false;
	export let docked = false;

	const dispatch = createEventDispatcher<{ open: string; close: void }>();
</script>

<section class="objective" class:objective--inspector={inspectorOpen} data-urgency={objective?.urgency ?? 'steady'} aria-label="Current work summary">
	<div class="objective__mark" aria-hidden="true"><Icon name="flag" size={17} /></div>
	<div class="objective__body">
		<div class="objective__meta">
			<span>{objective?.eyebrow ?? 'Current work'}</span>
			<span class="objective__state">{objective?.progressLabel ?? 'Standing by'}</span>
		</div>
		<strong class="objective__title">{objective?.title ?? 'No active task'}</strong>
		<span class="objective__detail">
			{objective?.detail ?? 'Create a task or open Tasks to assign work to the crew.'}
		</span>
	</div>
	{#if objective}
		<button
			type="button"
			class="objective__open"
			title={objective.actionLabel}
			aria-label={`${objective.actionLabel}: ${objective.title}`}
			on:click={() => dispatch('open', objective?.quest.id)}
		>
			<Icon name="arrow-right" size={17} />
		</button>
	{/if}
	{#if !docked}
		<button type="button" class="objective__close" title="Close summary" aria-label="Close current work summary" on:click={() => dispatch('close')}>
			<Icon name="x" size={15} />
		</button>
	{/if}
</section>

<style>
	.objective {
		pointer-events: auto;
		position: absolute;
		top: calc(var(--game-hud-gutter, 14px) + 2.8rem);
		left: 50%;
		right: auto;
		z-index: var(--game-layer-hud, 30);
		display: grid;
		grid-template-columns: 2rem minmax(12rem, 1fr) 2rem 2rem;
		align-items: center;
		gap: 0.65rem;
		width: min(40rem, calc(100% - 12rem));
		min-height: 3.6rem;
		padding: 0.45rem 0.55rem;
		border: 1px solid color-mix(in srgb, var(--objective-accent, #d7a84d) 42%, transparent);
		border-radius: var(--game-radius-md, 6px);
		background: color-mix(in srgb, var(--game-material-panel, #12171b) 92%, transparent);
		box-shadow: var(--game-shadow-hud, 0 10px 26px rgba(0, 0, 0, 0.34));
		color: var(--game-text, #f5f2e8);
		backdrop-filter: blur(12px);
		transform: translateX(-50%);
	}

	.objective::after {
		position: absolute;
		inset: 3px;
		border: 1px solid color-mix(in srgb, var(--game-text, #fff) 8%, transparent);
		border-radius: calc(var(--game-radius-md, 6px) - 2px);
		content: '';
		pointer-events: none;
	}
	.objective--inspector { width: min(36rem, calc(100% - var(--game-inspector-width-wide, 26rem) - 8rem)); }

	.objective[data-urgency='critical'],
	.objective[data-urgency='attention'] {
		--objective-accent: var(--game-state-danger, #e26d5a);
	}

	.objective[data-urgency='active'] {
		--objective-accent: var(--game-state-success, #62b889);
	}

	.objective__mark {
		display: grid;
		width: 2rem;
		height: 2rem;
		place-items: center;
		border: 1px solid color-mix(in srgb, var(--objective-accent, #d7a84d) 48%, transparent);
		border-radius: 50%;
		color: var(--objective-accent, #d7a84d);
	}

	.objective__body {
		display: grid;
		min-width: 0;
		gap: 0.1rem;
	}

	.objective__meta {
		display: flex;
		gap: 0.55rem;
		align-items: center;
		color: var(--game-text-muted, #aeb5b9);
		font-size: var(--game-type-2, 0.72rem);
		font-weight: 700;
		text-transform: uppercase;
	}

	.objective__state {
		color: var(--objective-accent, #d7a84d);
	}

	.objective__title,
	.objective__detail {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.objective__title {
		font-size: var(--game-type-4, 0.96rem);
		line-height: 1.25;
	}

	.objective__detail {
		color: var(--game-text-muted, #aeb5b9);
		font-size: var(--game-type-2, 0.78rem);
	}

	.objective__open {
		position: relative;
		z-index: 1;
		display: grid;
		width: 2rem;
		height: 2rem;
		place-items: center;
		padding: 0;
		border: 1px solid color-mix(in srgb, var(--objective-accent, #d7a84d) 58%, transparent);
		border-radius: 50%;
		background: color-mix(in srgb, var(--objective-accent, #d7a84d) 13%, transparent);
		color: var(--objective-accent, #d7a84d);
		cursor: pointer;
	}

	.objective__open:hover,
	.objective__open:focus-visible {
		background: color-mix(in srgb, var(--objective-accent, #d7a84d) 25%, transparent);
		outline: 2px solid color-mix(in srgb, var(--objective-accent, #d7a84d) 48%, transparent);
		outline-offset: 2px;
	}

	.objective__close {
		position: relative;
		z-index: 1;
		display: grid;
		width: 2rem;
		height: 2rem;
		place-items: center;
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--game-text-muted);
		cursor: pointer;
	}
	.objective__close:hover,
	.objective__close:focus-visible {
		color: var(--game-text);
		outline: 2px solid var(--game-focus-color);
		outline-offset: 1px;
	}
</style>
