<script lang="ts">
	import GameTooltip from './GameTooltip.svelte';
	import type { GameTone } from './types';
	import '../game-chrome.css';

	export let label: string;
	export let value: string | number;
	export let detail = '';
	export let tone: GameTone = 'neutral';
	export let actionable = false;
	export let disabled = false;
	export let tooltip = '';
	export let live: 'off' | 'polite' = 'off';
	export let className = '';

	$: accessibleLabel = detail ? `${label}: ${value}. ${detail}` : `${label}: ${value}`;
</script>

<GameTooltip content={tooltip} disabled={!tooltip || disabled} display="block">
	{#if actionable}
		<button
			{...$$restProps}
			type="button"
			class={`game-ui-status-instrument ${className}`.trim()}
			data-game-tone={tone}
			aria-label={accessibleLabel}
			{disabled}
			on:click
			on:focus
			on:blur
			on:keydown
		>
			<span class="game-ui-status-instrument__icon" aria-hidden="true"><slot name="icon" /></span>
			<span class="game-ui-status-instrument__label" aria-hidden="true">{label}</span>
			<span class="game-ui-status-instrument__value" aria-hidden="true">{value}</span>
			{#if detail}<span class="game-ui-status-instrument__detail" aria-hidden="true">{detail}</span>{/if}
		</button>
	{:else}
		<div
			{...$$restProps}
			class={`game-ui-status-instrument ${className}`.trim()}
			data-game-tone={tone}
			role="status"
			aria-label={accessibleLabel}
			aria-live={live === 'off' ? undefined : live}
		>
			<span class="game-ui-status-instrument__icon" aria-hidden="true"><slot name="icon" /></span>
			<span class="game-ui-status-instrument__label" aria-hidden="true">{label}</span>
			<span class="game-ui-status-instrument__value" aria-hidden="true">{value}</span>
			{#if detail}<span class="game-ui-status-instrument__detail" aria-hidden="true">{detail}</span>{/if}
		</div>
	{/if}
</GameTooltip>
