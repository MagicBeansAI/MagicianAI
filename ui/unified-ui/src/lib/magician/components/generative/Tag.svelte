<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let text: string = '';
	export let color: 'default' | 'success' | 'warning' | 'error' | 'info' = 'default';
	export let className: string = '';
	export let dismissible: boolean = false;

	const dispatch = createEventDispatcher<{ dismiss: void }>();

	$: safeColor = (
		color === 'success' || color === 'warning' || color === 'error' || color === 'info'
			? color
			: 'default'
	) as 'default' | 'success' | 'warning' | 'error' | 'info';
</script>

<!-- R661: aria-label conveys semantic color meaning to screen readers (WCAG 1.4.1) -->
<span class="muij-tag muij-tag-{safeColor} {className}" role={safeColor !== 'default' ? 'status' : undefined} aria-label={safeColor !== 'default' ? `${text} (${safeColor})` : undefined}>
	<span class="muij-tag-text">{text}</span>
	{#if dismissible}
		<button type="button" class="muij-tag-dismiss" aria-label="Remove {text}" on:click|stopPropagation={() => dispatch('dismiss')}>×</button>
	{/if}
</span>

<style>
	.muij-tag {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 500;
		padding: 3px 10px;
		border-radius: var(--radius-sm);
		line-height: 1.4;
	}

	.muij-tag-text {
		overflow-wrap: anywhere;
	}

	.muij-tag-dismiss {
		all: unset;
		cursor: pointer;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		font-size: 0.875em;
		line-height: 1;
		opacity: 0.6;
		padding: 0 1px;
		margin-left: -2px;
		border-radius: 2px;
	}

	.muij-tag-dismiss:hover {
		opacity: 1;
	}

	.muij-tag-default {
		background: var(--bg-soft);
		color: var(--text-secondary);
		border: 1px solid var(--border-soft);
	}

	.muij-tag-success {
		background: color-mix(in srgb, var(--color-success) 12%, transparent);
		color: var(--color-success);
		border: 1px solid color-mix(in srgb, var(--color-success) 30%, transparent);
	}

	.muij-tag-warning {
		background: color-mix(in srgb, var(--color-warning, #f59e0b) 12%, transparent);
		color: var(--color-warning, #f59e0b);
		border: 1px solid color-mix(in srgb, var(--color-warning, #f59e0b) 30%, transparent);
	}

	.muij-tag-error {
		background: color-mix(in srgb, var(--color-error) 12%, transparent);
		color: var(--color-error);
		border: 1px solid color-mix(in srgb, var(--color-error) 30%, transparent);
	}

	.muij-tag-info {
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		color: var(--accent-primary);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 30%, transparent);
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-tag {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 1px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.7rem;
	}

	:global([data-theme^="retro-16bit"]) .muij-tag::before {
		content: '[';
		margin-right: 2px;
	}

	:global([data-theme^="retro-16bit"]) .muij-tag::after {
		content: ']';
		margin-left: 2px;
	}
</style>
