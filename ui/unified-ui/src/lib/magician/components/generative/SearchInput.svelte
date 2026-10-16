<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let value: string = '';
	export let placeholder: string = 'Search...';
	export let label: string = '';
	export let ariaLabel: string = '';
	export let disabled: boolean = false;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ search: { value: string } }>();
	let localValue = value;

	$: inputId = buildStableDomId('muij-search', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || placeholder.trim() || 'Search';
	$: if (value !== localValue) {
		localValue = value;
	}

	function submitSearch(): void {
		if (disabled) return;
		dispatch('search', { value: localValue.trim() });
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Enter') return;
		event.preventDefault();
		submitSearch();
	}
</script>

<div class="muij-search">
	{#if label}
		<label class="muij-search-label" for={inputId}>{label}</label>
	{/if}
	<div class="muij-search-row">
		<input
			id={inputId}
			class="muij-search-input"
			type="search"
			bind:value={localValue}
			{placeholder}
			{disabled}
			aria-label={!label ? resolvedAriaLabel : undefined}
			on:keydown={handleKeydown}
		/>
		<button type="button" class="muij-search-button" on:click={submitSearch} disabled={disabled}>
			Search
		</button>
	</div>
</div>

<style>
	.muij-search {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-search-label {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-search-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 6px;
	}

	.muij-search-input {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-body);
		padding: 6px 10px;
	}

	.muij-search-button {
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
		color: var(--text-body);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		padding: 6px 10px;
		cursor: pointer;
	}

	.muij-search-button:disabled,
	.muij-search-input:disabled {
		opacity: 0.55;
		cursor: default;
	}
</style>
