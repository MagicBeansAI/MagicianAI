<script lang="ts">
	import { createEventDispatcher } from 'svelte';

export let icon: string = '∅';
export let title: string = 'No results';
export let description: string = '';
export let actionLabel: string = '';
export let className: string = '';

	const dispatch = createEventDispatcher<{ action: void }>();
</script>

<div class="muij-empty-state {className}">
	<div class="muij-empty-icon" aria-hidden="true">{icon}</div>
	<div class="muij-empty-title">{title}</div>
	{#if description.trim().length > 0}
		<div class="muij-empty-description">{description}</div>
	{/if}
	{#if actionLabel.trim().length > 0}
		<button type="button" class="muij-empty-action" on:click={() => dispatch('action')}>
			{actionLabel}
		</button>
	{/if}
</div>

<style>
	.muij-empty-state {
		display: grid;
		justify-items: center;
		text-align: center;
		gap: 6px;
		padding: 14px;
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-md);
		background: var(--bg-card);
	}

	.muij-empty-icon {
		font-size: 1rem;
		color: var(--text-secondary);
	}

	.muij-empty-title {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 600;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.muij-empty-description {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.muij-empty-action {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
		color: var(--text-body);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		padding: 6px 10px;
		cursor: pointer;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-empty-state {
		border-radius: 0;
		border: 2px dashed var(--text-primary);
		background: var(--bg-base);
	}

	:global([data-theme^="retro-16bit"]) .muij-empty-title,
	:global([data-theme^="retro-16bit"]) .muij-empty-description {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-empty-action {
		border-radius: 0;
		font-family: var(--font-mono);
		background: var(--text-primary);
		color: var(--bg-base);
		text-transform: uppercase;
	}
</style>
