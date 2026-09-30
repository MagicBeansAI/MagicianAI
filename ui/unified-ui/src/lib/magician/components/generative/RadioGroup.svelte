<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	interface RadioOption {
		value: string;
		label: string;
		disabled?: boolean;
	}

	interface NormalizedRadioOption {
		key: string;
		value: string;
		label: string;
		disabled: boolean;
	}

	export let label: string = '';
	export let options: RadioOption[] = [];
	export let value: string = '';
	export let disabled: boolean = false;
	export let name: string = '';
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ change: { value: string } }>();
	let localValue = value;

	$: normalizedOptions = normalizeOptions(options);
	$: groupName = asTrimmedString(name) || buildStableDomId('muij-rg-name', idBase || label || 'group');
	$: if (value !== localValue) {
		localValue = value;
	}

	function asTrimmedString(value: unknown): string {
		if (typeof value === 'string') return value.trim();
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value).trim();
		}
		return '';
	}

	function normalizeOptions(input: RadioOption[]): NormalizedRadioOption[] {
		if (!Array.isArray(input)) return [];
		const seen = new Set<string>();
		const normalized: NormalizedRadioOption[] = [];
		let ordinal = 0;
		for (const candidate of input as unknown[]) {
			if (candidate == null || typeof candidate !== 'object' || Array.isArray(candidate)) continue;
			const rec = candidate as Record<string, unknown>;
			const hasValue = rec.value !== undefined && rec.value !== null;
			const hasScalarValue = typeof rec.value === 'string'
				|| typeof rec.value === 'number'
				|| typeof rec.value === 'boolean'
				|| typeof rec.value === 'bigint';
			const optionValue = hasScalarValue ? asTrimmedString(rec.value) : '';
			const optionLabel = asTrimmedString(rec.label ?? (hasScalarValue ? optionValue : ''));
			const effectiveValue = hasScalarValue ? optionValue : optionLabel;
			const effectiveLabel = optionLabel || optionValue;
			const isExplicitEmptyValue = hasScalarValue && hasValue && optionValue === '';
			if (!effectiveValue && !effectiveLabel && !isExplicitEmptyValue) continue;
			if (seen.has(effectiveValue)) continue;
			seen.add(effectiveValue);
			ordinal += 1;
			normalized.push({
				key: `${effectiveValue}:${ordinal}`,
				value: effectiveValue,
				label: effectiveLabel,
				disabled: rec.disabled === true
			});
		}
		return normalized;
	}

	function onSelect(next: string): void {
		localValue = next;
		dispatch('change', { value: next });
	}
</script>

<fieldset class="muij-radio-group" disabled={disabled}>
	{#if label}
		<legend class="muij-radio-legend">{label}</legend>
	{/if}
	<div class="muij-radio-options">
		{#each normalizedOptions as option, idx (option.key)}
			<label class="muij-radio-option" for={buildStableDomId(`muij-rg-${idx}`, idBase)}>
				<input
					id={buildStableDomId(`muij-rg-${idx}`, idBase)}
					type="radio"
					name={groupName}
					value={option.value}
					checked={localValue === option.value}
					disabled={disabled || !!option.disabled}
					on:change={() => onSelect(option.value)}
				/>
				<span>{option.label}</span>
			</label>
		{/each}
	</div>
</fieldset>

<style>
	.muij-radio-group {
		border: none;
		padding: 0;
		margin: 0;
		display: grid;
		gap: 6px;
	}

	.muij-radio-legend {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
		padding: 0;
	}

	.muij-radio-options {
		display: grid;
		gap: 6px;
	}

	.muij-radio-option {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
	}
</style>
