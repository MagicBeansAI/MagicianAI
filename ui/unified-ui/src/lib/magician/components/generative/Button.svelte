<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';

	export let label: string = '';
	/**
	 * Shared-icon name rendered before the label (or alone when label is
	 * empty). THE mechanism for icons in MUIJ JSON surfaces: emitters set
	 * `props.icon` to a $lib/shared/icons name instead of embedding emoji or
	 * raw SVG strings in `label` (labels render as plain text). The icon is
	 * decorative (aria-hidden); icon-only buttons must pass `title` so the
	 * button keeps an accessible name.
	 */
	export let icon: IconName | '' = '';
	export let variant: 'primary' | 'secondary' | 'outline' = 'primary';
	export let disabled: boolean = false;
	export let size: 'sm' | 'md' | 'lg' = 'md';
	export let className: string = '';
	/** R657: Accessible label for screen readers (required for icon-only buttons). */
	export let ariaLabel: string = '';
	/** Native tooltip shown on hover. */
	export let title: string = '';
	/** GAUI-β defaults to interactive when used as an action control. */
	export let interactive: boolean = true;
	export let type: 'button' | 'submit' | 'reset' = 'button';
	/** Render as a square icon-only button (no label text). */
	export let iconOnly: boolean = false;
	/** Stop the underlying DOM click from bubbling past this button. */
	export let stopPropagation: boolean = false;

	const dispatch = createEventDispatcher<{ click: MouseEvent }>();

	$: safeVariant = (
		variant === 'secondary' || variant === 'outline'
			? variant
			: 'primary'
	) as 'primary' | 'secondary' | 'outline';

	$: safeSize = (
		size === 'sm' || size === 'lg'
			? size
			: 'md'
	) as 'sm' | 'md' | 'lg';

	$: isDisabled = disabled || !interactive;

	function handleClick(event: MouseEvent): void {
		if (isDisabled) {
			event.preventDefault();
			return;
		}
		if (stopPropagation) {
			event.stopPropagation();
		}
		if (type !== 'submit') {
			dispatch('click', event);
		}
	}
</script>

<button
	{type}
	class="muij-button muij-button-{safeVariant} muij-button-{safeSize} {iconOnly ? 'muij-button-icon' : ''} {className}"
	disabled={isDisabled}
	title={title || undefined}
	aria-label={ariaLabel || title || undefined}
	on:click={handleClick}
>
	<slot name="icon" />
	{#if icon}<Icon name={icon} size={14} />{/if}
	{#if !iconOnly}{label}{/if}
</button>

<style>
	.muij-button {
		font-family: var(--font-primary);
		font-weight: 500;
		border-radius: var(--radius-md);
		cursor: pointer;
		transition: background 0.15s, color 0.15s, border-color 0.15s, box-shadow 0.15s, transform 0.15s, opacity 0.15s;
		border: none;
		overflow-wrap: anywhere;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
	}

	.muij-button:disabled {
		opacity: 0.5;
		cursor: default;
	}

	/* Sizes */
	.muij-button-sm {
		font-size: 0.75rem;
		padding: 4px 10px;
	}

	.muij-button-md {
		font-size: 0.8125rem;
		padding: 6px 16px;
	}

	.muij-button-lg {
		font-size: 0.875rem;
		padding: 10px 24px;
	}

	/* Icon-only: square sizing */
	.muij-button-icon.muij-button-sm {
		padding: 4px;
		width: 24px;
		height: 24px;
	}
	.muij-button-icon.muij-button-md {
		padding: 6px;
		width: 32px;
		height: 32px;
	}
	.muij-button-icon.muij-button-lg {
		padding: 8px;
		width: 40px;
		height: 40px;
	}

	/* Variants */
	.muij-button-primary {
		background: var(--button-primary-bg, var(--accent-primary));
		color: var(--button-primary-color, var(--text-on-accent, #fff));
		box-shadow: var(--button-primary-shadow, none);
	}

	.muij-button-primary:hover:not(:disabled) {
		box-shadow: var(--button-primary-shadow-hover, var(--button-primary-shadow, none));
		transform: translateY(-1px);
	}

	.muij-button-secondary {
		background: var(--button-secondary-bg, var(--bg-soft));
		color: var(--button-secondary-color, var(--text-secondary));
		border: 1px solid var(--button-secondary-border, transparent);
		box-shadow: var(--button-secondary-shadow, none);
	}

	.muij-button-secondary:hover:not(:disabled) {
		background: var(--button-secondary-hover-bg, var(--border-soft));
		transform: translateY(-1px);
	}

	.muij-button-outline {
		background: transparent;
		color: var(--text-secondary);
		border: 1px solid var(--button-outline-border, var(--border-soft));
	}

	.muij-button-outline:hover:not(:disabled) {
		background: var(--button-outline-hover-bg, transparent);
		border-color: var(--button-outline-border, var(--text-muted));
		color: var(--button-outline-hover-color, var(--text-primary));
	}

	:global([data-theme^="magican"]) .muij-button {
		letter-spacing: -0.01em;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme="retro-16bit"]) .muij-button {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 1px solid #ffb000;
		text-transform: uppercase;
		box-shadow: 4px 4px 0px #805800;
	}

	:global([data-theme="retro-16bit"]) .muij-button:hover:not(:disabled) {
		transform: translate(2px, 2px);
		box-shadow: 2px 2px 0px #805800;
		background: #ffb000;
		color: #000;
	}

	:global([data-theme="retro-16bit"]) .muij-button-primary {
		background: #ffb000;
		color: #000;
	}

	:global([data-theme="retro-16bit"]) .muij-button-secondary,
	:global([data-theme="retro-16bit"]) .muij-button-outline {
		background: #000;
		color: #ffb000;
		border: 1px solid #ffb000;
	}

	/* Icon-only retro: smaller box-shadow */
	:global([data-theme="retro-16bit"]) .muij-button-icon {
		box-shadow: 2px 2px 0px #805800;
	}
	:global([data-theme="retro-16bit"]) .muij-button-icon:hover:not(:disabled) {
		transform: translate(1px, 1px);
		box-shadow: 1px 1px 0px #805800;
	}

	/* Retro 16-bit Light Theme Overrides */
	:global([data-theme="retro-16bit-light"]) .muij-button {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 2px solid #1a1a1a;
		text-transform: uppercase;
		box-shadow: 4px 4px 0px #999999;
	}

	:global([data-theme="retro-16bit-light"]) .muij-button:hover:not(:disabled) {
		transform: translate(2px, 2px);
		box-shadow: 2px 2px 0px #999999;
		background: #1a1a1a;
		color: #f5f5f0;
	}

	:global([data-theme="retro-16bit-light"]) .muij-button:active:not(:disabled) {
		transform: translate(4px, 4px);
		box-shadow: none;
	}

	:global([data-theme="retro-16bit-light"]) .muij-button-primary {
		background: #1a1a1a;
		color: #f5f5f0;
	}

	:global([data-theme="retro-16bit-light"]) .muij-button-secondary,
	:global([data-theme="retro-16bit-light"]) .muij-button-outline {
		background: #f5f5f0;
		color: #1a1a1a;
		border: 2px solid #1a1a1a;
	}

	/* Icon-only retro-light: smaller box-shadow */
	:global([data-theme="retro-16bit-light"]) .muij-button-icon {
		box-shadow: 2px 2px 0px #999999;
	}
	:global([data-theme="retro-16bit-light"]) .muij-button-icon:hover:not(:disabled) {
		transform: translate(1px, 1px);
		box-shadow: 1px 1px 0px #999999;
	}
</style>
