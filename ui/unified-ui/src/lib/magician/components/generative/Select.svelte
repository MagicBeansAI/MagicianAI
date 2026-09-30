<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';

	export let options: Array<{value: string, label: string, group?: string}> = [];
	export let value: string = '';
	export let disabled: boolean = false;
	export let label: string = '';
	export let placeholder: string = 'Select...';
	export let idBase: string = '';
	/** R655: Accessible label for screen readers when no visible label is present. */
	export let ariaLabel: string = '';
	/** GAUI-β is display-only; enable interactivity only when explicitly opted in. */
	export let interactive: boolean = false;
	const dispatch = createEventDispatcher<{ change: { value: string } }>();
	$: selectId = buildStableDomId('muij-sel', idBase);
	$: isDisabled = disabled || !interactive;
	// R375: Treat whitespace-only placeholder payloads as empty and fall back.
	$: safePlaceholder = typeof placeholder === 'string' && placeholder.trim() !== ''
		? placeholder
		: 'Select...';
	// R408: Trim selected value to match option canonicalization (optionsFromProps trims values).
	$: trimmedValue = typeof value === 'string' ? value.trim() : value;
	// R574: Use a stable option lookup keyed by value to avoid reset on options array identity change.
	$: optionValueSet = new Set(options.map((opt) => opt.value));
	$: hasMatchingValue = optionValueSet.has(trimmedValue);
	$: hasEmptyOption = optionValueSet.has('');
	// R331/R355: If value doesn't match, prefer explicit empty-value option when present.
	// Otherwise use placeholder sentinel or the first option fallback.
	$: fallbackValue = hasEmptyOption ? '' : (options.length > 0 ? options[0].value : '');
	$: showPlaceholder = !hasMatchingValue && !hasEmptyOption;
	$: effectiveValue = hasMatchingValue ? trimmedValue : (showPlaceholder ? '' : fallbackValue);

	interface NormalizedOption {
		value: string;
		label: string;
		group: string;
	}

	interface OptionBucket {
		key: string;
		label: string;
		grouped: boolean;
		options: NormalizedOption[];
	}

	$: normalizedOptions = options
		.map((opt) => ({
			value: typeof opt.value === 'string' ? opt.value.trim() : '',
			label: typeof opt.label === 'string' ? opt.label.trim() : '',
			group: typeof opt.group === 'string' ? opt.group.trim() : ''
		}))
		.filter((opt) => opt.value.length > 0 || opt.label.length > 0);

	$: optionBuckets = (() => {
		const buckets: OptionBucket[] = [];
		const groupedByName = new Map<string, OptionBucket>();
		let ungroupedBucket: OptionBucket | null = null;
		for (const option of normalizedOptions) {
			if (!option.group) {
				if (!ungroupedBucket) {
					ungroupedBucket = {
						key: '__ungrouped__',
						label: '',
						grouped: false,
						options: []
					};
					buckets.push(ungroupedBucket);
				}
				ungroupedBucket.options.push(option);
				continue;
			}
			const existingBucket = groupedByName.get(option.group);
			if (existingBucket) {
				existingBucket.options.push(option);
				continue;
			}
			const nextBucket: OptionBucket = {
				key: option.group,
				label: option.group,
				grouped: true,
				options: [option]
			};
			groupedByName.set(option.group, nextBucket);
			buckets.push(nextBucket);
		}
		return buckets.filter((bucket) => bucket.options.length > 0);
	})();

	function handleChange(event: Event): void {
		const nextValue = (event.currentTarget as HTMLSelectElement).value;
		dispatch('change', { value: nextValue });
	}
</script>

<div class="muij-select">
	{#if label}
		<label class="muij-select-label" for={selectId}>{label}</label>
	{/if}
		<select
			id={selectId}
			class="muij-select-input"
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
					{#each bucket.options as opt (`${bucket.key}:${opt.value}`)}
						<option value={opt.value}>{opt.label || opt.value}</option>
					{/each}
				</optgroup>
			{:else}
				{#each bucket.options as opt (`${bucket.key}:${opt.value}`)}
					<option value={opt.value}>{opt.label || opt.value}</option>
				{/each}
			{/if}
		{/each}
	</select>
</div>

<style>
	.muij-select {
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-select-label {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 500;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.muij-select-input {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		padding: 6px 10px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		color: var(--text-body);
		outline: none;
		cursor: default;
	}

	.muij-select-input:disabled {
		opacity: 0.5;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-select-input {
		border-radius: 0;
		font-family: var(--font-mono);
		border: 1px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-select-label {
		font-family: var(--font-mono);
		color: var(--text-primary);
		text-transform: uppercase;
		font-size: 0.7rem;
	}
</style>
