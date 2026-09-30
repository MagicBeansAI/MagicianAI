<script lang="ts" context="module">
	let nativeSelectSequence = 0;

	function nextNativeSelectId(): string {
		nativeSelectSequence += 1;
		return `native-select-${nativeSelectSequence}`;
	}

	function idPart(value: string): string {
		return value.trim().replace(/[^a-zA-Z0-9_-]+/g, '-').replace(/^-+|-+$/g, '') || 'control';
	}
</script>

<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	type SelectOption = { value: string; label: string; group?: string };
	type OptionBucket = { key: string; label: string; grouped: boolean; options: SelectOption[] };

	export let options: SelectOption[] = [];
	export let value = '';
	export let disabled = false;
	export let label = '';
	export let placeholder = 'Select...';
	export let idBase = '';
	export let ariaLabel = '';
	export let interactive = false;

	const generatedId = nextNativeSelectId();
	const dispatch = createEventDispatcher<{ change: { value: string } }>();

	$: selectId = idBase ? `native-select-${idPart(idBase)}` : generatedId;
	$: isDisabled = disabled || !interactive;
	$: safePlaceholder = typeof placeholder === 'string' && placeholder.trim() ? placeholder : 'Select...';
	$: normalizedOptions = Array.isArray(options)
		? options
				.map((option) => ({
					value: typeof option.value === 'string' ? option.value.trim() : '',
					label: typeof option.label === 'string' ? option.label.trim() : '',
					group: typeof option.group === 'string' ? option.group.trim() : ''
				}))
				.filter((option) => option.value.length > 0 || option.label.length > 0)
		: [];
	$: optionValueSet = new Set(normalizedOptions.map((option) => option.value));
	$: trimmedValue = typeof value === 'string' ? value.trim() : '';
	$: hasMatchingValue = optionValueSet.has(trimmedValue);
	$: hasEmptyOption = optionValueSet.has('');
	$: showPlaceholder = !hasMatchingValue && !hasEmptyOption;
	$: fallbackValue = hasEmptyOption ? '' : normalizedOptions[0]?.value ?? '';
	$: effectiveValue = hasMatchingValue ? trimmedValue : showPlaceholder ? '' : fallbackValue;
	$: optionBuckets = buildBuckets(normalizedOptions);

	function buildBuckets(input: SelectOption[]): OptionBucket[] {
		const buckets: OptionBucket[] = [];
		const grouped = new Map<string, OptionBucket>();
		let ungrouped: OptionBucket | null = null;
		for (const option of input) {
			if (!option.group) {
				if (!ungrouped) {
					ungrouped = { key: '__ungrouped__', label: '', grouped: false, options: [] };
					buckets.push(ungrouped);
				}
				ungrouped.options.push(option);
				continue;
			}
			const existing = grouped.get(option.group);
			if (existing) {
				existing.options.push(option);
				continue;
			}
			const next = { key: option.group, label: option.group, grouped: true, options: [option] };
			grouped.set(option.group, next);
			buckets.push(next);
		}
		return buckets.filter((bucket) => bucket.options.length > 0);
	}

	function handleChange(event: Event): void {
		dispatch('change', { value: (event.currentTarget as HTMLSelectElement).value });
	}
</script>

<div class="native-select">
	{#if label}
		<label class="native-select__label" for={selectId}>{label}</label>
	{/if}
	<select
		id={selectId}
		class="native-select__input"
		disabled={isDisabled}
		aria-label={!label && ariaLabel ? ariaLabel : undefined}
		value={effectiveValue}
		on:change={handleChange}
	>
		{#if showPlaceholder}
			<option value="" disabled>{safePlaceholder}</option>
		{/if}
		{#each optionBuckets as bucket (bucket.key)}
			{#if bucket.grouped}
				<optgroup label={bucket.label}>
					{#each bucket.options as option (`${bucket.key}:${option.value}`)}
						<option value={option.value}>{option.label || option.value}</option>
					{/each}
				</optgroup>
			{:else}
				{#each bucket.options as option (`${bucket.key}:${option.value}`)}
					<option value={option.value}>{option.label || option.value}</option>
				{/each}
			{/if}
		{/each}
	</select>
</div>

<style>
	.native-select {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		min-width: 0;
	}

	.native-select__label {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.native-select__input {
		width: 100%;
		min-height: 2rem;
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: var(--radius-md, 8px);
		background: var(--input-bg, var(--bg-card));
		color: var(--text-body);
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		padding: 0.375rem 0.625rem;
		outline: none;
	}

	.native-select__input:focus-visible {
		border-color: var(--input-focus-border, var(--accent-primary));
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary, currentColor) 14%, transparent);
	}

	.native-select__input:disabled {
		opacity: 0.55;
	}

	:global([data-theme^='retro-16bit']) .native-select__label,
	:global([data-theme^='retro-16bit']) .native-select__input {
		border-radius: 0;
		font-family: var(--font-mono);
		text-transform: uppercase;
	}

	:global([data-theme^='retro-16bit']) .native-select__input {
		border-color: var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
	}
</style>
