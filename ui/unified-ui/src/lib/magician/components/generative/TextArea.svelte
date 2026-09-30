<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let value: string = '';
	export let placeholder: string = '';
	export let rows: number = 4;
	export let maxLength: number | undefined = undefined;
	export let label: string = '';
	export let ariaLabel: string = '';
	export let disabled: boolean = false;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{
		change: { value: string };
		keydown: KeyboardEvent;
	}>();
	let localValue = value;
	let lastExternalValue = value;

	$: textareaId = buildStableDomId('muij-ta', idBase);
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || placeholder.trim() || 'Text area';
	$: safeRows = Number.isFinite(rows) && rows > 0 ? Math.floor(rows) : 4;
	$: safeMaxLength = Number.isFinite(maxLength) && (maxLength as number) > 0
		? Math.floor(maxLength as number)
		: undefined;
	$: if (value !== lastExternalValue) {
		lastExternalValue = value;
		if (value !== localValue) {
			localValue = value;
		}
	}

	function onInput(event: Event): void {
		const next = (event.currentTarget as HTMLTextAreaElement).value;
		localValue = next;
		lastExternalValue = next;
		// Propagate the typed value back to the parent so `bind:value`
		// on this component stays in sync. Without this, the parent's
		// `value` prop never updated on keystrokes, and the reactive
		// block above ($: if (value !== lastExternalValue)) would re-fire
		// on each input — resetting `localValue` back to the parent's
		// stale value and visibly erasing what the user just typed.
		// Observed in HITL guidance prompts: typing felt like nothing
		// was happening because every keystroke immediately reverted.
		value = next;
		dispatch('change', { value: next });
	}

	function onKeydown(event: KeyboardEvent): void {
		dispatch('keydown', event);
	}
</script>

<div class="muij-textarea">
	{#if label}
		<label class="muij-textarea-label" for={textareaId}>{label}</label>
	{/if}
	<textarea
		id={textareaId}
		class="muij-textarea-input"
		bind:value={localValue}
		{placeholder}
			rows={safeRows}
			maxlength={safeMaxLength}
			{disabled}
			aria-label={!label ? resolvedAriaLabel : undefined}
			on:input={onInput}
			on:keydown={onKeydown}
		></textarea>
	{#if safeMaxLength}
		<div class="muij-textarea-meta">{localValue.length} / {safeMaxLength}</div>
	{/if}
</div>

<style>
	.muij-textarea {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-textarea-label,
	.muij-textarea-meta {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-textarea-input {
		width: 100%;
		min-height: 72px;
		resize: vertical;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-body);
		padding: 8px 10px;
	}

	.muij-textarea-input:disabled {
		opacity: 0.55;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-textarea-input {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 1px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-textarea-label,
	:global([data-theme^="retro-16bit"]) .muij-textarea-meta {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.7rem;
	}
</style>
