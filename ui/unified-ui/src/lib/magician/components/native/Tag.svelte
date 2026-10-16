<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	type TagColor = 'default' | 'success' | 'warning' | 'error' | 'info';

	export let text = '';
	export let color: TagColor = 'default';
	export let className = '';
	export let dismissible = false;

	const dispatch = createEventDispatcher<{ dismiss: void }>();
	const COLORS = new Set<TagColor>(['default', 'success', 'warning', 'error', 'info']);

	$: safeColor = COLORS.has(color) ? color : 'default';
	$: tagClass = ['native-tag', `native-tag--${safeColor}`, className].filter(Boolean).join(' ');
</script>

<span class={tagClass} role={safeColor !== 'default' ? 'status' : undefined}>
	<span class="native-tag__text">{text}</span>
	{#if dismissible}
		<button
			type="button"
			class="native-tag__dismiss"
			aria-label="Remove {text}"
			on:click|stopPropagation={() => dispatch('dismiss')}
		>
			x
		</button>
	{/if}
</span>

<style>
	.native-tag {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		padding: 0.1875rem 0.5rem;
		background: var(--bg-soft);
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		line-height: 1.35;
	}

	.native-tag__text {
		overflow-wrap: anywhere;
	}

	.native-tag__dismiss {
		border: 0;
		background: transparent;
		color: inherit;
		cursor: pointer;
		font: inherit;
		opacity: 0.65;
		padding: 0;
	}

	.native-tag__dismiss:hover {
		opacity: 1;
	}

	.native-tag--success {
		border-color: color-mix(in srgb, var(--color-success) 32%, transparent);
		background: color-mix(in srgb, var(--color-success) 12%, transparent);
		color: var(--color-success);
	}

	.native-tag--warning {
		border-color: color-mix(in srgb, var(--color-warning) 32%, transparent);
		background: color-mix(in srgb, var(--color-warning) 12%, transparent);
		color: var(--color-warning);
	}

	.native-tag--error {
		border-color: color-mix(in srgb, var(--color-error) 32%, transparent);
		background: color-mix(in srgb, var(--color-error) 12%, transparent);
		color: var(--color-error);
	}

	.native-tag--info {
		border-color: color-mix(in srgb, var(--accent-primary) 32%, transparent);
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		color: var(--accent-primary);
	}

	:global([data-theme^='retro-16bit']) .native-tag {
		border-radius: 0;
		border-color: var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
		font-family: var(--font-mono);
		text-transform: uppercase;
	}
</style>
