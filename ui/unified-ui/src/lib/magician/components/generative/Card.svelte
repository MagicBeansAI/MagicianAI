<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let title: string = '';
	export let subtitle: string | undefined = undefined;
	export let body: string | undefined = undefined;
	export let elevation: number = 1;
	export let interactive: boolean = false;
	export let disabled: boolean = false;
	export let className: string = '';
	export let tooltip: string = '';

	const dispatch = createEventDispatcher<{ click: void }>();

	// R25/R28: round + NaN fallback. Number.isFinite guards against NaN/Infinity
	// while preserving elevation 0 (|| would treat 0 as falsy → R28 regression).
	$: clampedElevation = Math.round(Math.max(0, Math.min(3, Number.isFinite(+elevation) ? +elevation : 1)));

	function shouldIgnoreCardClick(target: EventTarget | null, currentTarget: EventTarget | null): boolean {
		const node = target as HTMLElement | null;
		if (!node) return false;
		const interactiveAncestor = node.closest(
			'button,input,select,textarea,a,label,[role="button"],[role="link"]'
		);
		if (!interactiveAncestor) return false;
		const current = currentTarget as HTMLElement | null;
		if (current && interactiveAncestor === current) {
			return false;
		}
		return true;
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
	class="muij-card {className}"
	class:muij-card-interactive={interactive && !disabled}
	class:muij-elevation-0={clampedElevation === 0}
	class:muij-elevation-1={clampedElevation === 1}
	class:muij-elevation-2={clampedElevation === 2}
	class:muij-elevation-3={clampedElevation === 3}
	data-interactive={interactive && !disabled ? 'true' : undefined}
	role={interactive ? 'button' : undefined}
	tabindex={interactive ? 0 : undefined}
	aria-disabled={interactive ? disabled : undefined}
	title={tooltip || undefined}
	on:click={handleClick}
	on:keydown={handleKeydown}
>
	{#if title}
		<div class="muij-card-title">{title}</div>
	{/if}
	{#if subtitle}
		<div class="muij-card-subtitle">{subtitle}</div>
	{/if}
	{#if body}
		<div class="muij-card-body">{body}</div>
	{/if}
	{#if title || subtitle || body}
		<div class="muij-card-slot">
			<slot />
		</div>
	{:else}
		<slot />
	{/if}
</div>

<style>
	.muij-card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg);
		padding: var(--space-lg);
		transition: var(--transition-base);
	}

	.muij-card-interactive {
		cursor: pointer;
	}

	.muij-card.muij-elevation-0 {
		box-shadow: none;
	}

	.muij-card.muij-elevation-1 {
		box-shadow: var(--shadow-sm);
	}

	.muij-card.muij-elevation-2 {
		box-shadow: var(--shadow-md);
	}

	.muij-card.muij-elevation-3 {
		box-shadow: var(--shadow-lg);
	}

	.muij-card-title {
		/* Card titles use --font-display so they read as headings (matches
		   the global h1-h6 rule in app.css). Was --font-primary which fell
		   to the body font and made every card title look like body text. */
		font-family: var(--font-display);
		color: var(--text-primary);
		font-size: 1rem;
		font-weight: 600;
		line-height: 1.4;
		overflow-wrap: anywhere;
	}

	.muij-card-subtitle {
		/* Subtitles stay on --font-primary (body) so the visual hierarchy
		   reads as Display-title → Body-subtitle. */
		font-family: var(--font-primary);
		color: var(--text-secondary);
		font-size: 0.8125rem;
		line-height: 1.4;
		margin-top: 2px;
		overflow-wrap: anywhere;
	}

	.muij-card-body {
		font-family: var(--font-primary);
		color: var(--text-primary);
		font-size: 0.875rem;
		line-height: 1.6;
		margin-top: 8px;
		overflow-wrap: anywhere;
	}

	.muij-card-slot {
		margin-top: 12px;
	}

	.muij-card-slot:empty {
		display: none;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-card {
		border-radius: 0;
		border: 2px solid var(--text-primary);
		background: var(--bg-base);
		box-shadow: 6px 6px 0px var(--text-muted);
	}

	:global([data-theme^="retro-16bit"]) .muij-card-title,
	:global([data-theme^="retro-16bit"]) .muij-card-subtitle,
	:global([data-theme^="retro-16bit"]) .muij-card-body {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-card-title {
		border-bottom: 1px dashed var(--text-muted);
		padding-bottom: 4px;
		margin-bottom: 8px;
	}
</style>
