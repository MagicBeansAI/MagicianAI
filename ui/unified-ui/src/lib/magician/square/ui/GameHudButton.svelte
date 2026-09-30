<script lang="ts">
	import GameTooltip from './GameTooltip.svelte';
	import type { GameControlSize, GameTone } from './types';
	import '../game-chrome.css';

	export let label: string;
	export let tooltip = '';
	export let tooltipPosition: 'top' | 'bottom' | 'left' | 'right' = 'top';
	export let shortcut = '';
	export let tone: GameTone = 'neutral';
	export let size: GameControlSize = 'default';
	export let active = false;
	export let pressed: boolean | undefined = undefined;
	export let disabled = false;
	export let showLabel = false;
	export let badge: string | number | null = null;
	export let badgeLabel = 'notifications';
	export let type: 'button' | 'submit' | 'reset' = 'button';
	export let className = '';

	$: hasBadge = badge !== null && badge !== '';
	$: accessibleLabel = hasBadge ? `${label}, ${badge} ${badgeLabel}` : label;
</script>

<GameTooltip
	content={tooltip || label}
	position={tooltipPosition}
	{shortcut}
	disabled={disabled}
>
	<button
		{...$$restProps}
		{type}
		class={`game-ui-hud-button ${className}`.trim()}
		data-game-tone={tone}
		data-tone={tone}
		data-size={size}
		data-active={active}
		data-icon-only={!showLabel}
		aria-label={accessibleLabel}
		aria-pressed={pressed}
		{disabled}
		on:click
		on:focus
		on:blur
		on:keydown
		on:keyup
		on:pointerdown
	>
		<span class="game-ui-hud-button__icon" aria-hidden="true"><slot /></span>
		{#if showLabel}<span class="game-ui-hud-button__label">{label}</span>{/if}
		{#if hasBadge}<span class="game-ui-hud-button__badge" aria-hidden="true">{badge}</span>{/if}
	</button>
</GameTooltip>
