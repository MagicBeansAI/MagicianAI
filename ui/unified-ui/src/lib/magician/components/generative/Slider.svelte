<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let value: number = 0;
	export let min: number = 0;
	export let max: number = 100;
	export let step: number = 1;
	export let label: string = '';
	export let ariaLabel: string = '';
	export let disabled: boolean = false;
	export let showValue: boolean = true;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ change: { value: number } }>();
	let localValue = Number.isFinite(value) ? value : 0;

	$: sliderId = buildStableDomId('muij-sld', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Slider';
	$: safeMin = Number.isFinite(min) ? min : 0;
	$: safeMax = Number.isFinite(max) && max > safeMin ? max : safeMin + 1;
	$: safeStep = Number.isFinite(step) && step > 0 ? step : 1;
	$: if (Number.isFinite(value) && value !== localValue) {
		localValue = value;
	}

	function onInput(event: Event): void {
		const next = Number((event.currentTarget as HTMLInputElement).value);
		if (!Number.isFinite(next)) return;
		localValue = next;
		dispatch('change', { value: next });
	}
</script>

<div class="muij-slider">
	{#if label || showValue}
		<div class="muij-slider-head">
			{#if label}<label class="muij-slider-label" for={sliderId}>{label}</label>{/if}
			{#if showValue}<span class="muij-slider-value">{localValue}</span>{/if}
		</div>
	{/if}
	<input
		id={sliderId}
		class="muij-slider-input"
		type="range"
		bind:value={localValue}
		min={safeMin}
			max={safeMax}
			step={safeStep}
			{disabled}
			aria-label={!label ? resolvedAriaLabel : undefined}
			on:input={onInput}
		/>
	</div>

<style>
	.muij-slider {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-slider-head {
		display: flex;
		justify-content: space-between;
		gap: 10px;
	}

	.muij-slider-label,
	.muij-slider-value {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-slider-value {
		font-variant-numeric: tabular-nums;
	}

	.muij-slider-input {
		width: 100%;
	}

	.muij-slider-input:disabled {
		opacity: 0.55;
	}
</style>
