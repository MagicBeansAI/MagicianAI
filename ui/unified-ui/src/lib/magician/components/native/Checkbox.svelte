<script lang="ts" context="module">
	let nativeCheckboxSequence = 0;

	function nextNativeCheckboxId(): string {
		nativeCheckboxSequence += 1;
		return `native-checkbox-${nativeCheckboxSequence}`;
	}

	function idPart(value: string): string {
		return value.trim().replace(/[^a-zA-Z0-9_-]+/g, '-').replace(/^-+|-+$/g, '') || 'control';
	}
</script>

<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let label = '';
	export let ariaLabel = '';
	export let checked = false;
	export let indeterminate = false;
	export let disabled = false;
	export let idBase = '';
	export let className = '';

	const generatedId = nextNativeCheckboxId();
	const dispatch = createEventDispatcher<{ change: { checked: boolean } }>();

	let inputEl: HTMLInputElement | null = null;
	let localChecked = checked;
	let lastExternalChecked = checked;

	$: checkboxId = idBase ? `native-checkbox-${idPart(idBase)}` : generatedId;
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Checkbox';
	$: if (checked !== lastExternalChecked) {
		lastExternalChecked = checked;
		localChecked = checked;
	}
	$: if (inputEl) {
		inputEl.indeterminate = indeterminate && !localChecked;
	}

	function handleChange(event: Event): void {
		const next = (event.currentTarget as HTMLInputElement).checked;
		localChecked = next;
		lastExternalChecked = next;
		dispatch('change', { checked: next });
	}
</script>

<label class={['native-checkbox', className].filter(Boolean).join(' ')}>
	<input
		bind:this={inputEl}
		id={checkboxId}
		class="native-checkbox__input"
		type="checkbox"
		bind:checked={localChecked}
		{disabled}
		aria-label={!label ? resolvedAriaLabel : undefined}
		on:change={handleChange}
	/>
	{#if label}
		<span class="native-checkbox__label">{label}</span>
	{/if}
</label>

<style>
	.native-checkbox {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		min-width: 0;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
	}

	.native-checkbox__input {
		appearance: none;
		width: 1rem;
		height: 1rem;
		margin: 0;
		display: inline-grid;
		place-content: center;
		flex: 0 0 auto;
		border: 1.5px solid var(--input-border, var(--border-soft));
		border-radius: 5px;
		background:
			linear-gradient(
				180deg,
				color-mix(in srgb, var(--input-bg, var(--bg-card)) 96%, transparent),
				color-mix(in srgb, var(--bg-soft) 92%, transparent)
			);
		color: var(--text-on-accent);
		cursor: pointer;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			box-shadow 120ms ease;
	}

	.native-checkbox__input::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 120ms ease;
	}

	.native-checkbox__input:hover:not(:disabled) {
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary, currentColor) 13%, transparent);
	}

	.native-checkbox__input:checked,
	.native-checkbox__input:indeterminate {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
	}

	.native-checkbox__input:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.native-checkbox__input:indeterminate::before {
		width: 0.55rem;
		height: 2px;
		border: 0;
		border-radius: 999px;
		background: currentColor;
		transform: scale(1);
	}

	.native-checkbox__input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 2px;
	}

	.native-checkbox__input:disabled,
	.native-checkbox__input:disabled + .native-checkbox__label {
		opacity: 0.55;
	}

	.native-checkbox__label {
		overflow-wrap: anywhere;
	}

	:global([data-theme^='retro-16bit']) .native-checkbox {
		font-family: var(--font-mono);
		text-transform: uppercase;
	}

	:global([data-theme^='retro-16bit']) .native-checkbox__input {
		border-radius: 0;
		border-color: var(--text-primary);
		background: var(--bg-base);
	}
</style>
