<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';

	export let label = '';
	export let icon: IconName | '' = '';
	export let variant: 'primary' | 'secondary' | 'outline' = 'primary';
	export let disabled = false;
	export let size: 'sm' | 'md' | 'lg' = 'md';
	export let className = '';
	export let ariaLabel = '';
	export let title = '';
	export let interactive = true;
	export let type: 'button' | 'submit' | 'reset' = 'button';
	export let iconOnly = false;
	export let stopPropagation = false;
	/**
	 * The two attributes a **disclosure** needs, so one does not have to be
	 * hand-rolled to get them.
	 *
	 * `aria-expanded` is what carries "there is more of this" to a screen reader,
	 * and a control that toggles a region is otherwise announced as a plain
	 * button — which is why the task panel's `Details` and `Preview` controls were
	 * hand-written `<button>`s approximating this component's look. `null` omits
	 * the attribute entirely rather than rendering `aria-expanded="false"` on a
	 * button that expands nothing.
	 */
	export let ariaExpanded: boolean | null = null;
	/** The id of the region this control expands. Omitted when empty. */
	export let ariaControls = '';
	/**
	 * **Where this control goes, when going somewhere is what it does.**
	 *
	 * A non-empty `href` renders an `<a>` instead of a `<button>`, carrying the
	 * same classes and therefore the same treatment in every theme. That is not a
	 * convenience: opening a file in a tab and downloading one are things the
	 * *browser* performs, so they are navigations and must stay anchors — middle
	 * click, `Copy link address` and the download attribute all stop working on a
	 * `<button>`. Without this prop the only way to make a navigation look like the
	 * rest of a control row was to hand-roll the component's CSS beside it, which
	 * is exactly the duplication this component exists to prevent.
	 *
	 * `disabled` still renders the `<button>` branch: there is no way to make an
	 * anchor inert that a keyboard user cannot walk straight past.
	 */
	export let href = '';
	/** Only meaningful with `href`. `download` may carry a filename or be bare. */
	export let target = '';
	export let rel = '';
	export let download: string | boolean = false;

	const dispatch = createEventDispatcher<{ click: MouseEvent }>();

	$: safeVariant = variant === 'secondary' || variant === 'outline' ? variant : 'primary';
	$: safeSize = size === 'sm' || size === 'lg' ? size : 'md';
	$: isDisabled = disabled || !interactive;
	$: buttonClass = [
		'native-button',
		`native-button--${safeVariant}`,
		`native-button--${safeSize}`,
		iconOnly ? 'native-button--icon' : '',
		className
	]
		.filter(Boolean)
		.join(' ');

	// A link, only while it has somewhere to go and is allowed to go there. See
	// the note on `href` for why `disabled` falls back to the button branch.
	$: isLink = href !== '' && !isDisabled;

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

<!--
	Two elements, one appearance. The contents are duplicated rather than lifted
	into a snippet because this component is Svelte 4 syntax and has no `{#snippet}`
	— three lines each, and the class list that carries every treatment is computed
	once above, so the two branches cannot look different.
-->
{#if isLink}
	<a
		{href}
		class={buttonClass}
		target={target || undefined}
		rel={rel || undefined}
		download={download === false ? undefined : download}
		title={title || undefined}
		aria-label={ariaLabel || title || (iconOnly ? label : undefined) || undefined}
		on:click={handleClick}
	>
		<slot name="icon" />
		{#if icon}
			<Icon name={icon} size={14} />
		{/if}
		{#if !iconOnly}
			<span class="native-button__label">{label}</span>
		{/if}
	</a>
{:else}
	<button
		{type}
		class={buttonClass}
		disabled={isDisabled}
		title={title || undefined}
		aria-label={ariaLabel || title || (iconOnly ? label : undefined) || undefined}
		aria-expanded={ariaExpanded === null ? undefined : ariaExpanded}
		aria-controls={ariaControls || undefined}
		on:click={handleClick}
	>
		<slot name="icon" />
		{#if icon}
			<Icon name={icon} size={14} />
		{/if}
		{#if !iconOnly}
			<span class="native-button__label">{label}</span>
		{/if}
	</button>
{/if}

<style>
	.native-button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.375rem;
		border: 1px solid transparent;
		border-radius: var(--radius-md, 8px);
		font-family: var(--font-primary);
		font-weight: 600;
		line-height: 1.2;
		text-align: center;
		/* For the `<a>` branch: an underline is the web's word for navigation, and a
		   control that already looks like a control must not say it twice. The
		   `<button>` branch is unaffected — it has no underline to remove. */
		text-decoration: none;
		color: var(--text-primary);
		cursor: pointer;
		transition:
			background 140ms ease,
			border-color 140ms ease,
			color 140ms ease,
			box-shadow 140ms ease,
			transform 140ms ease,
			opacity 140ms ease;
	}

	.native-button:disabled {
		cursor: default;
		opacity: 0.5;
	}

	.native-button:not(:disabled):hover {
		transform: translateY(-1px);
	}

	.native-button:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 58%, transparent);
		outline-offset: 2px;
	}

	.native-button--sm {
		min-height: 1.75rem;
		padding: 0.25rem 0.625rem;
		font-size: 0.75rem;
	}

	.native-button--md {
		min-height: 2rem;
		padding: 0.375rem 0.875rem;
		font-size: 0.8125rem;
	}

	.native-button--lg {
		min-height: 2.5rem;
		padding: 0.625rem 1.25rem;
		font-size: 0.875rem;
	}

	.native-button--icon.native-button--sm {
		width: 1.75rem;
		padding: 0;
	}

	.native-button--icon.native-button--md {
		width: 2rem;
		padding: 0;
	}

	.native-button--icon.native-button--lg {
		width: 2.5rem;
		padding: 0;
	}

	.native-button--primary {
		background: var(--button-primary-bg, var(--accent-primary));
		color: var(--button-primary-color, var(--text-on-accent));
		box-shadow: var(--button-primary-shadow, none);
	}

	.native-button--primary:not(:disabled):hover {
		box-shadow: var(--button-primary-shadow-hover, var(--button-primary-shadow, none));
	}

	.native-button--secondary {
		background: var(--button-secondary-bg, var(--bg-soft));
		color: var(--button-secondary-color, var(--text-secondary));
		border-color: var(--button-secondary-border, var(--border-soft));
		box-shadow: var(--button-secondary-shadow, none);
	}

	.native-button--secondary:not(:disabled):hover {
		background: var(--button-secondary-hover-bg, color-mix(in srgb, var(--bg-soft) 82%, var(--accent-primary)));
		color: var(--text-primary);
	}

	.native-button--outline {
		background: transparent;
		color: var(--text-secondary);
		border-color: var(--button-outline-border, var(--border-soft));
	}

	.native-button--outline:not(:disabled):hover {
		background: var(--button-outline-hover-bg, var(--bg-soft));
		border-color: var(--button-outline-border-hover, var(--text-muted));
		color: var(--button-outline-hover-color, var(--text-primary));
	}

	.native-button__label {
		overflow-wrap: anywhere;
	}

	:global([data-theme^='retro-16bit']) .native-button {
		border-radius: 0;
		border: 1px solid var(--text-primary);
		font-family: var(--font-mono);
		text-transform: uppercase;
		box-shadow: 3px 3px 0 color-mix(in srgb, var(--text-primary) 42%, transparent);
	}

	:global([data-theme^='retro-16bit']) .native-button:not(:disabled):hover {
		transform: translate(1px, 1px);
		box-shadow: 2px 2px 0 color-mix(in srgb, var(--text-primary) 42%, transparent);
	}
</style>
