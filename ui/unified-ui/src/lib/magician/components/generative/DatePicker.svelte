<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let value: string = '';
	export let min: string = '';
	export let max: string = '';
	export let label: string = '';
	export let ariaLabel: string = '';
	export let disabled: boolean = false;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ change: { value: string } }>();
	let localValue = value;

	$: dateId = buildStableDomId('muij-date', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Date';
	$: if (value !== localValue) {
		localValue = value;
	}

	function onInput(event: Event): void {
		const next = (event.currentTarget as HTMLInputElement).value;
		localValue = next;
		dispatch('change', { value: next });
	}
</script>

<div class="muij-date-picker">
	{#if label}
		<label class="muij-date-label" for={dateId}>{label}</label>
	{/if}
	<input
		id={dateId}
		class="muij-date-input"
		type="date"
		bind:value={localValue}
			{min}
			{max}
			{disabled}
			aria-label={!label ? resolvedAriaLabel : undefined}
			on:input={onInput}
		/>
	</div>

<style>
	.muij-date-picker {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-date-label {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-date-input {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-body);
		padding: 6px 10px;
	}

	.muij-date-input:disabled {
		opacity: 0.55;
	}
</style>
