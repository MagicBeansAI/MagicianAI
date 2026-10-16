<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	interface MultiSelectOption {
		value: string;
		label: string;
		disabled?: boolean;
	}

	interface NormalizedMultiSelectOption {
		key: string;
		value: string;
		label: string;
		disabled: boolean;
	}

	export let label: string = '';
	export let options: MultiSelectOption[] = [];
	export let values: string[] = [];
	export let disabled: boolean = false;
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{ change: { values: string[] } }>();
	let selected = new Set<string>(values);
	$: normalizedOptions = normalizeOptions(options);

	function setEquals(left: Set<string>, right: Set<string>): boolean {
		if (left.size !== right.size) return false;
		for (const value of left) {
			if (!right.has(value)) return false;
		}
		return true;
	}

	$: normalizedValues = new Set(
		Array.isArray(values) ? values.filter((value): value is string => typeof value === 'string') : []
	);
	$: if (!setEquals(selected, normalizedValues)) {
		selected = new Set(normalizedValues);
	}

	function asTrimmedString(value: unknown): string {
		if (typeof value === 'string') return value.trim();
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value).trim();
		}
		return '';
	}

	function normalizeOptions(input: MultiSelectOption[]): NormalizedMultiSelectOption[] {
		if (!Array.isArray(input)) return [];
		const seen = new Set<string>();
		const normalized: NormalizedMultiSelectOption[] = [];
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

	function toggleSelection(optionValue: string): void {
		const next = new Set(selected);
		if (next.has(optionValue)) next.delete(optionValue);
		else next.add(optionValue);
		selected = next;
		dispatch('change', { values: [...next] });
	}
</script>

<fieldset class="muij-multiselect" disabled={disabled}>
	{#if label}
		<legend class="muij-multiselect-legend">{label}</legend>
	{/if}
	<div class="muij-multiselect-options">
		{#each normalizedOptions as option, idx (option.key)}
			<label class="muij-multiselect-option" for={buildStableDomId(`muij-ms-${idx}`, idBase)}>
				<input
					id={buildStableDomId(`muij-ms-${idx}`, idBase)}
					type="checkbox"
					checked={selected.has(option.value)}
					disabled={disabled || !!option.disabled}
					on:change={() => toggleSelection(option.value)}
				/>
				<span>{option.label}</span>
			</label>
		{/each}
	</div>
</fieldset>

<style>
	.muij-multiselect {
		border: none;
		padding: 0;
		margin: 0;
		display: grid;
		gap: 6px;
	}

	.muij-multiselect-legend {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
		padding: 0;
	}

	.muij-multiselect-options {
		display: grid;
		gap: 6px;
	}

	.muij-multiselect-option {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-body);
	}
</style>
