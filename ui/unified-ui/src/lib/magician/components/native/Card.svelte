<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let title = '';
	export let subtitle: string | undefined = undefined;
	export let body: string | undefined = undefined;
	export let elevation = 1;
	export let interactive = false;
	export let disabled = false;
	export let className = '';
	export let tooltip = '';

	const dispatch = createEventDispatcher<{ click: void }>();

	$: clampedElevation = Math.round(
		Math.max(0, Math.min(3, Number.isFinite(+elevation) ? +elevation : 1))
	);
	$: cardClass = [
		'native-card',
		`native-card--elevation-${clampedElevation}`,
		interactive && !disabled ? 'native-card--interactive' : '',
		className
	]
		.filter(Boolean)
		.join(' ');

	function shouldIgnoreCardClick(target: EventTarget | null, currentTarget: EventTarget | null): boolean {
		const node = target as HTMLElement | null;
		if (!node) return false;
		const interactiveAncestor = node.closest(
			'button,input,select,textarea,a,label,[role="button"],[role="link"]'
		);
		if (!interactiveAncestor) return false;
		return interactiveAncestor !== currentTarget;
	}

	function handleClick(event: MouseEvent): void {
		if (!interactive || disabled) return;
		if (shouldIgnoreCardClick(event.target, event.currentTarget)) return;
		dispatch('click');
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (!interactive || disabled) return;
		if (event.key !== 'Enter' && event.key !== ' ') return;
		event.preventDefault();
		dispatch('click');
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	class={cardClass}
	data-interactive={interactive && !disabled ? 'true' : undefined}
	role={interactive ? 'button' : undefined}
	tabindex={interactive ? 0 : undefined}
	aria-disabled={interactive ? disabled : undefined}
	title={tooltip || undefined}
	on:click={handleClick}
	on:keydown={handleKeydown}
>
	{#if title}
		<div class="native-card__title">{title}</div>
	{/if}
	{#if subtitle}
		<div class="native-card__subtitle">{subtitle}</div>
	{/if}
	{#if body}
		<div class="native-card__body">{body}</div>
	{/if}
	{#if title || subtitle || body}
		<div class="native-card__slot">
			<slot />
		</div>
	{:else}
		<slot />
	{/if}
</div>

<style>
	.native-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		padding: var(--space-lg);
		color: var(--text-primary);
		transition:
			border-color 140ms ease,
			box-shadow 140ms ease,
			transform 140ms ease,
			background 140ms ease;
	}

	.native-card--interactive {
		cursor: pointer;
	}

	.native-card--interactive:hover {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 28%, var(--border-soft));
		background: color-mix(in srgb, var(--bg-card) 92%, var(--accent-primary, transparent));
	}

	.native-card--interactive:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.native-card--elevation-0 {
		box-shadow: none;
	}

	.native-card--elevation-1 {
		box-shadow: var(--shadow-sm, none);
	}

	.native-card--elevation-2 {
		box-shadow: var(--shadow-md, var(--shadow-sm, none));
	}

	.native-card--elevation-3 {
		box-shadow: var(--shadow-lg, var(--shadow-md, none));
	}

	.native-card__title {
		font-family: var(--font-display);
		font-size: 1rem;
		font-weight: 650;
		line-height: 1.35;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.native-card__subtitle {
		margin-top: 0.125rem;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		line-height: 1.45;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.native-card__body {
		margin-top: 0.5rem;
		font-family: var(--font-primary);
		font-size: 0.875rem;
		line-height: 1.6;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.native-card__slot {
		margin-top: 0.75rem;
	}

	.native-card__slot:empty {
		display: none;
	}

	:global([data-theme^='retro-16bit']) .native-card {
		border-radius: 0;
		border: 2px solid var(--text-primary);
		background: var(--bg-base);
		box-shadow: 5px 5px 0 color-mix(in srgb, var(--text-primary) 34%, transparent);
	}

	:global([data-theme^='retro-16bit']) .native-card__title,
	:global([data-theme^='retro-16bit']) .native-card__subtitle,
	:global([data-theme^='retro-16bit']) .native-card__body {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
	}
</style>
