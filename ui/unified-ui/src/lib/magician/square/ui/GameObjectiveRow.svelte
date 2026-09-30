<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import GameTooltip from './GameTooltip.svelte';
	import type { GameTone } from './types';
	import '../game-chrome.css';

	export let objective: string;
	export let objectiveId = '';
	export let detail = '';
	export let meta = '';
	export let state = '';
	export let progress: number | null = null;
	export let tone: GameTone = 'neutral';
	export let selected = false;
	export let complete = false;
	export let interactive = true;
	export let disabled = false;
	export let tooltip = '';
	export let ariaLabel = '';
	export let className = '';

	const dispatch = createEventDispatcher<{ select: { id: string } }>();
	$: safeProgress = progress == null ? null : Math.max(0, Math.min(100, progress));
	$: accessibleLabel = ariaLabel || [objective, detail, state, safeProgress == null ? '' : `${safeProgress}% complete`]
		.filter(Boolean)
		.join('. ');

	function select(): void {
		if (interactive && !disabled) dispatch('select', { id: objectiveId });
	}
</script>

<GameTooltip content={tooltip || objective} display="block" position="right">
	<svelte:element
		this={interactive ? 'button' : 'div'}
		{...$$restProps}
		type={interactive ? 'button' : undefined}
		role={interactive ? undefined : 'group'}
		class={`game-ui-objective-row ${className}`.trim()}
		data-game-tone={tone}
		data-selected={selected}
		data-complete={complete}
		aria-label={accessibleLabel}
		aria-current={selected ? 'true' : undefined}
		disabled={interactive ? disabled : undefined}
		on:click={select}
	>
		<span class="game-ui-objective-row__marker" aria-hidden="true">
			{#if $$slots.icon}<slot name="icon" />{:else if complete}<Icon name="check" size={16} />{/if}
		</span>
		<span class="game-ui-objective-row__content" aria-hidden={interactive ? 'true' : undefined}>
			<span class="game-ui-objective-row__title">{objective}</span>
			{#if detail}<span class="game-ui-objective-row__detail">{detail}</span>{/if}
			{#if meta}<span class="game-ui-objective-row__meta">{meta}</span>{/if}
			{#if safeProgress != null}
				<span
					class="game-ui-objective-row__progress"
					role={interactive ? undefined : 'progressbar'}
					aria-label={interactive ? undefined : `Progress for ${objective}`}
					aria-valuemin={interactive ? undefined : 0}
					aria-valuemax={interactive ? undefined : 100}
					aria-valuenow={interactive ? undefined : safeProgress}
				>
					<span style={`width: ${safeProgress}%`}></span>
				</span>
			{/if}
		</span>
		{#if state}<span class="game-ui-objective-row__state" aria-hidden={interactive ? 'true' : undefined}>{state}</span>{:else}<span></span>{/if}
	</svelte:element>
</GameTooltip>
