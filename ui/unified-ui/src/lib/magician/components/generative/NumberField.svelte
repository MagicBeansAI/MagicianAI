<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let value: number = 0;
	export let min: number | undefined = undefined;
	export let max: number | undefined = undefined;
	export let step: number = 1;
	export let label: string = '';
	export let ariaLabel: string = '';
	export let disabled: boolean = false;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ change: { value: number } }>();
	let localValue = Number.isFinite(value) ? value : 0;
	let lastExternalValue = localValue;

	$: inputId = buildStableDomId('muij-num', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Number input';
	$: safeStep = Number.isFinite(step) && step > 0 ? step : 1;
	$: safeMin = Number.isFinite(min) ? min : undefined;
	$: safeMax = Number.isFinite(max) ? max : undefined;
	$: {
		const nextValue = Number.isFinite(value) ? value : 0;
		if (nextValue !== lastExternalValue) {
			lastExternalValue = nextValue;
			if (nextValue !== localValue) {
				localValue = nextValue;
			}
		}
	}

	function clamp(next: number): number {
		let clamped = next;
		if (safeMin !== undefined && clamped < safeMin) clamped = safeMin;
		if (safeMax !== undefined && clamped > safeMax) clamped = safeMax;
		return clamped;
	}

	function onInput(event: Event): void {
		const raw = Number((event.currentTarget as HTMLInputElement).value);
		if (!Number.isFinite(raw)) return;
		const next = clamp(raw);
		localValue = next;
		lastExternalValue = next;
		dispatch('change', { value: next });
	}
</script>

<div class="muij-number-field">
	{#if label}
		<label class="muij-number-label" for={inputId}>{label}</label>
	{/if}
	<input
		id={inputId}
		class="muij-number-input"
		type="number"
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
	.muij-number-field {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-number-label {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-number-input {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-body);
		padding: 6px 10px;
	}

	.muij-number-input:disabled {
		opacity: 0.55;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-number-input {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 1px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-number-label {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.7rem;
	}
</style>
