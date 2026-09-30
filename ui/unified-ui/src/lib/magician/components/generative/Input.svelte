<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let value: string = '';
	export let placeholder: string = '';
	export let type: 'text' | 'password' | 'email' | 'date' = 'text';
	export let disabled: boolean = false;
	export let label: string = '';
	export let ariaLabel: string = '';
	export let idBase: string = '';
	export let maxlength: number | undefined = undefined;
	/**
	 * Browser hints for the value being typed. A one-time code passes
	 * `autocomplete="one-time-code"` so a phone offers the code it just
	 * received (and no `inputmode`: a code is an exact string, and a numeric
	 * keypad would lock out an alphanumeric one); a secret passes
	 * `autocomplete="off"` so no manager stores what should never be stored.
	 * Omitted attributes are not rendered.
	 */
	export let inputmode: 'text' | 'numeric' | 'decimal' | 'tel' | 'email' | 'url' | undefined =
		undefined;
	export let autocomplete: 'off' | 'on' | 'one-time-code' | undefined = undefined;

	const dispatch = createEventDispatcher<{
		change: { value: string };
		keydown: KeyboardEvent;
	}>();
	let localValue = value;
	let lastExternalValue = value;

	$: inputId = buildStableDomId('muij-inp', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || placeholder.trim() || 'Text input';
	$: safeType = (
		type === 'password' || type === 'email' || type === 'date'
			? type
			: 'text'
	);
	$: if (value !== lastExternalValue) {
		lastExternalValue = value;
		if (value !== localValue) {
			localValue = value;
		}
	}

	function onInput(event: Event): void {
		const next = (event.currentTarget as HTMLInputElement).value;
		localValue = next;
		lastExternalValue = next;
		value = next;
		dispatch('change', { value: next });
	}

	function onKeydown(event: KeyboardEvent): void {
		dispatch('keydown', event);
	}
</script>

<div class="muij-input">
	{#if label}
		<label class="muij-input-label" for={inputId}>{label}</label>
	{/if}
	<input
		id={inputId}
		class="muij-input-field"
		type={safeType}
		bind:value={localValue}
		{placeholder}
		{disabled}
		{maxlength}
		{inputmode}
		{autocomplete}
		aria-label={!label ? resolvedAriaLabel : undefined}
		on:input={onInput}
		on:keydown={onKeydown}
	/>
</div>

<style>
	.muij-input {
		display: flex;
		flex-direction: column;
		gap: 4px;
		flex: 1;
		min-width: 0;
	}

	.muij-input-label {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-input-field {
		width: 100%;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-body);
		padding: 6px 10px;
		outline: none;
	}

	.muij-input-field:focus {
		border-color: var(--accent-primary);
	}

	.muij-input-field:disabled {
		opacity: 0.55;
	}

	/* Retro 16-bit Dark Theme */
	:global([data-theme="retro-16bit"]) .muij-input-field {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 1px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
	}

	:global([data-theme="retro-16bit"]) .muij-input-label {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.7rem;
	}

	/* Retro 16-bit Light Theme */
	:global([data-theme="retro-16bit-light"]) .muij-input-field {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 2px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
	}

	:global([data-theme="retro-16bit-light"]) .muij-input-label {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.7rem;
	}
</style>
