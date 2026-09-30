<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let label: string = '';
	export let ariaLabel: string = '';
	export let checked: boolean = false;
	export let indeterminate: boolean = false;
	export let disabled: boolean = false;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ change: { checked: boolean } }>();
	let inputEl: HTMLInputElement | null = null;
	let localChecked = checked;
	let lastExternalChecked = checked;

	$: checkboxId = buildStableDomId('muij-cb', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Checkbox';
	$: if (checked !== lastExternalChecked) {
		lastExternalChecked = checked;
		if (checked !== localChecked) {
			localChecked = checked;
		}
	}
	$: if (inputEl) {
		inputEl.indeterminate = indeterminate && !localChecked;
	}

	function onChange(event: Event): void {
		const next = (event.currentTarget as HTMLInputElement).checked;
		localChecked = next;
		lastExternalChecked = next;
		dispatch('change', { checked: next });
	}
</script>

<label class="muij-checkbox">
	<input
		bind:this={inputEl}
		id={checkboxId}
		class="muij-checkbox-input"
		type="checkbox"
		bind:checked={localChecked}
		{disabled}
		aria-label={!label ? resolvedAriaLabel : undefined}
		on:change={onChange}
	/>
	<span class="muij-checkbox-label">{label}</span>
</label>

<style>
	.muij-checkbox {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
	}

	.muij-checkbox-input {
		appearance: none;
		width: 1rem;
		height: 1rem;
		margin: 0;
		display: inline-grid;
		place-content: center;
		flex: 0 0 auto;
		border: 1.5px solid var(--border-default, color-mix(in srgb, currentColor 32%, transparent));
		border-radius: 5px;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--bg-card, #fff) 96%, transparent),
				color-mix(in srgb, var(--bg-soft, #f8fafc) 92%, transparent)
			);
		color: var(--text-on-accent, #fff);
		box-shadow:
			inset 0 1px 0 color-mix(in srgb, #fff 40%, transparent),
			0 1px 2px color-mix(in srgb, #000 10%, transparent);
		cursor: pointer;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			box-shadow 120ms ease,
			transform 120ms ease;
	}

	.muij-checkbox-input::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 120ms ease;
	}

	.muij-checkbox-input:hover:not(:disabled) {
		border-color: var(--accent-primary, currentColor);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, currentColor) 14%, transparent),
			inset 0 1px 0 color-mix(in srgb, #fff 36%, transparent);
	}

	.muij-checkbox-input:checked,
	.muij-checkbox-input:indeterminate {
		background: linear-gradient(
			135deg,
			var(--accent-primary, #2563eb),
			color-mix(in srgb, var(--accent-primary, #2563eb) 72%, var(--accent-secondary, #60a5fa))
		);
		border-color: var(--accent-primary, #2563eb);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, #2563eb) 18%, transparent),
			0 2px 8px color-mix(in srgb, var(--accent-primary, #2563eb) 24%, transparent);
	}

	.muij-checkbox-input:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.muij-checkbox-input:indeterminate::before {
		width: 0.55rem;
		height: 2px;
		border: 0;
		border-radius: 999px;
		background: currentColor;
		transform: scale(1);
	}

	.muij-checkbox-input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.muij-checkbox-input:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.muij-checkbox-input:disabled + .muij-checkbox-label {
		opacity: 0.55;
	}

	.muij-checkbox-label {
		overflow-wrap: anywhere;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-checkbox {
		font-family: var(--font-mono);
	}

	:global([data-theme^="retro-16bit"]) .muij-checkbox-label {
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-checkbox-input {
		border-radius: 0;
		border-color: var(--text-primary);
		box-shadow: 2px 2px 0 var(--text-muted);
	}
</style>
