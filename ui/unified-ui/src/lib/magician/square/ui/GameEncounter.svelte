<script lang="ts" context="module">
	let gameEncounterSequence = 0;

	function nextGameEncounterId(): string {
		gameEncounterSequence += 1;
		return `game-encounter-${gameEncounterSequence}`;
	}
</script>

<script lang="ts">
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import type { GameTone } from './types';
	import '../game-chrome.css';

	export let title: string;
	export let summary = '';
	export let kind: 'decision' | 'blocker' | 'delivery' | 'notice' = 'decision';
	export let kindLabel = '';
	export let recommendation = '';
	export let consequence = '';
	export let nextState = '';
	export let risk = '';
	export let tone: GameTone | null = null;
	export let urgent = false;
	export let resolved = false;
	export let className = '';

	const titleId = nextGameEncounterId();
	const DEFAULT_TONES: Record<typeof kind, GameTone> = {
		decision: 'attention',
		blocker: 'danger',
		delivery: 'success',
		notice: 'active'
	};
	const DEFAULT_ICONS: Record<typeof kind, IconName> = {
		decision: 'alert',
		blocker: 'x',
		delivery: 'check',
		notice: 'info'
	};

	$: effectiveTone = resolved ? 'success' : tone ?? DEFAULT_TONES[kind];
	$: effectiveKindLabel = kindLabel || (resolved ? 'Resolved' : kind[0].toUpperCase() + kind.slice(1));
</script>

<section
	{...$$restProps}
	class={`game-ui-encounter ${className}`.trim()}
	data-game-tone={effectiveTone}
	data-resolved={resolved}
	role={urgent && !resolved ? 'alert' : 'group'}
	aria-labelledby={titleId}
	aria-live={urgent && !resolved ? 'assertive' : undefined}
>
	<header class="game-ui-encounter__header">
		<span class="game-ui-encounter__icon" aria-hidden="true">
			{#if $$slots.icon}<slot name="icon" />{:else}<Icon name={DEFAULT_ICONS[kind]} size={20} />{/if}
		</span>
		<div>
			<p class="game-ui-encounter__kind">{effectiveKindLabel}</p>
			<h3 id={titleId} class="game-ui-encounter__title">{title}</h3>
		</div>
		{#if $$slots.badge}<slot name="badge" />{/if}
	</header>
	{#if summary}<p class="game-ui-encounter__summary">{summary}</p>{/if}
	{#if $$slots.default}<div class="game-ui-encounter__body"><slot /></div>{/if}
	{#if recommendation || consequence || risk || nextState}
		<dl class="game-ui-encounter__facts">
			{#if recommendation}
				<div class="game-ui-encounter__fact"><dt>Recommended</dt><dd>{recommendation}</dd></div>
			{/if}
			{#if consequence}
				<div class="game-ui-encounter__fact"><dt>Consequence</dt><dd>{consequence}</dd></div>
			{/if}
			{#if risk}<div class="game-ui-encounter__fact"><dt>Risk</dt><dd>{risk}</dd></div>{/if}
			{#if nextState}
				<div class="game-ui-encounter__fact"><dt>Next state</dt><dd>{nextState}</dd></div>
			{/if}
		</dl>
	{/if}
	{#if $$slots.actions}<div class="game-ui-encounter__actions"><slot name="actions" /></div>{/if}
</section>
