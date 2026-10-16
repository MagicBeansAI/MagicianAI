<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let icon = '';
	export let title = 'No results';
	export let description = '';
	export let actionLabel = '';
	export let className = '';

	const dispatch = createEventDispatcher<{ action: void }>();
</script>

<div class={['native-empty-state', className].filter(Boolean).join(' ')}>
	{#if icon}
		<div class="native-empty-state__icon" aria-hidden="true">{icon}</div>
	{/if}
	<div class="native-empty-state__title">{title}</div>
	{#if description.trim().length > 0}
		<div class="native-empty-state__description">{description}</div>
	{/if}
	{#if actionLabel.trim().length > 0}
		<button type="button" class="native-empty-state__action" on:click={() => dispatch('action')}>
			{actionLabel}
		</button>
	{/if}
</div>

<style>
	.native-empty-state {
		display: grid;
		justify-items: center;
		gap: 0.375rem;
		padding: 0.875rem;
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		text-align: center;
	}

	.native-empty-state__icon {
		color: var(--text-secondary);
		font-size: 1rem;
	}

	.native-empty-state__title {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 700;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.native-empty-state__description {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.native-empty-state__action {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft);
		color: var(--text-body);
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		padding: 0.375rem 0.625rem;
	}

	:global([data-theme^='retro-16bit']) .native-empty-state,
	:global([data-theme^='retro-16bit']) .native-empty-state__action {
		border-radius: 0;
	}

	:global([data-theme^='retro-16bit']) .native-empty-state__title,
	:global([data-theme^='retro-16bit']) .native-empty-state__description,
	:global([data-theme^='retro-16bit']) .native-empty-state__action {
		font-family: var(--font-mono);
		text-transform: uppercase;
	}
</style>
