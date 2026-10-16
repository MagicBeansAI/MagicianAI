<script lang="ts">
	import { buildStableDomId } from './idUtil';

	export let value: string = '';
	export let placeholder: string = '';
	export let disabled: boolean = false;
	export let label: string = '';
	/** R656: Accessible label for screen readers when no visible label is present. */
	export let ariaLabel: string = '';
	export let idBase: string = '';
	$: inputId = buildStableDomId('muij-tf', idBase);
</script>

<div class="muij-textfield">
	{#if label}
		<label class="muij-textfield-label" for={inputId}>{label}</label>
	{/if}
	<input
		id={inputId}
		class="muij-textfield-input"
		type="text"
		{value}
		{placeholder}
		{disabled}
		aria-label={!label && ariaLabel ? ariaLabel : undefined}
		aria-readonly="true"
		readonly
	/>
</div>

<style>
	.muij-textfield {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-textfield-label {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 500;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.muij-textfield-input {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		padding: 6px 10px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		color: var(--text-body);
		outline: none;
		overflow-wrap: anywhere;
	}

	/* R675: Visual distinction for readonly state */
	.muij-textfield-input:read-only {
		background: var(--bg-soft, #f5f5f5);
		cursor: default;
	}

	.muij-textfield-input:disabled {
		opacity: 0.5;
	}
</style>
