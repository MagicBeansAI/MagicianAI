<script lang="ts" context="module">
	const multiFilterByComponentId = new Map<string, string>();
</script>

<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type {
		CrewNativeComponent,
		CrewNativeInteractionEventDetail,
		CrewNativeInteractionKind
	} from './nativeSurface';

	export let component: CrewNativeComponent;

	type NativeOption = { value: string; label: string; disabled: boolean };
	type NativeField = Record<string, unknown>;
	type NativeTableColumn = { key: string; label: string };

	const dispatch = createEventDispatcher<{ interaction: CrewNativeInteractionEventDetail }>();

	let formValues: Record<string, string> = {};
	let formSignature = '';
	let multiValues: string[] = [];
	let multiSignature = '';
	let multiFilter = '';
	let multiFilterSignature = '';
	let activeTabIndex = 0;
	let tabSignature = '';

	$: props = component.props || {};
	$: children = Array.isArray(component.children) ? component.children : [];
	$: fields = normalizeFields(props.fields);
	$: options = normalizeOptions(props.options);
	$: tableColumns = normalizeColumns(props.columns);
	$: tableRows = normalizeRows(props.rows);
	$: dataItems = normalizeDataItems(props.items);
	$: tabs = normalizeTabs(props.tabs);
	$: cardTitle = asString(props.title);
	$: cardSubtitle = asString(props.subtitle);
	$: cardBody = asString(props.body);
	$: componentLabel = component.label || asString(props.label);
	$: stackStyle = stackStyleFor(props);
	$: gridStyle = gridStyleFor(props);
	$: formDisabled = asBoolean(props.disabled);
	$: formTitle = asString(props.title);
	$: showSubmit = props.showSubmit !== false;
	$: submitLabel = asString(props.submitLabel, 'Submit');
	$: codeText = asString(props.code ?? props.value);
	$: textContent = asString(props.children ?? props.text ?? component.label);
	$: alertType = asString(props.type, 'info');
	$: alertMessage = asString(props.message);
	$: buttonVariant = asString(props.variant, 'secondary');
	$: buttonSize = asString(props.size, 'md');
	$: buttonDisabled = asBoolean(props.disabled);
	$: buttonLabel = component.label || asString(props.label) || component.id;
	$: badgeText = asString(props.text ?? component.label);
	$: badgeColor = asString(props.color, 'default');
	$: textareaValue = asString(props.value);
	$: textareaRows = asNumber(props.rows, 8);
	$: multiSearchable = asBoolean(props.searchable);
	$: multiSearchPlaceholder = asString(props.searchPlaceholder, 'Filter options');
	$: multiFilteredOptions = filteredMultiOptions(options, multiFilter);

	$: {
		if (component.component_type === 'Form') {
			const nextSignature = `${component.id}:${fields
				.map((field) => `${fieldId(field)}=${asString(field.value)}`)
				.join('\u0001')}`;
			if (nextSignature !== formSignature) {
				formSignature = nextSignature;
				formValues = Object.fromEntries(
					fields.map((field) => [fieldId(field), asString(field.value)])
				);
			}
		}
	}

	$: {
		if (component.component_type === 'MultiSelect') {
			if (component.id !== multiFilterSignature) {
				multiFilterSignature = component.id;
				multiFilter = multiFilterByComponentId.get(component.id) || '';
			}
			const rawValues = Array.isArray(props.values) ? props.values : [];
			const nextValues = rawValues
				.map((value) => asString(value))
				.filter((value) => value.length > 0);
			const nextSignature = `${component.id}:${nextValues.join('\u0001')}`;
			if (nextSignature !== multiSignature) {
				multiSignature = nextSignature;
				multiValues = nextValues;
			}
		}
	}

	$: {
		if (component.component_type === 'Tabs') {
			const nextSignature = `${component.id}:${tabs.length}:${asNumber(props.activeIndex, 0)}`;
			if (nextSignature !== tabSignature) {
				tabSignature = nextSignature;
				activeTabIndex = Math.max(0, Math.min(asNumber(props.activeIndex, 0), tabs.length - 1));
			}
		}
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return value != null && typeof value === 'object' && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function asString(value: unknown, fallback = ''): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		return fallback;
	}

	function asBoolean(value: unknown): boolean {
		if (typeof value === 'boolean') return value;
		if (typeof value === 'string') return value.toLowerCase() === 'true';
		return false;
	}

	function asNumber(value: unknown, fallback: number): number {
		const parsed = typeof value === 'number' ? value : Number.parseFloat(asString(value));
		return Number.isFinite(parsed) ? parsed : fallback;
	}

	function normalizeFields(value: unknown): NativeField[] {
		return Array.isArray(value) ? value.filter((field): field is NativeField => !!asRecord(field)) : [];
	}

	function normalizeOptions(value: unknown): NativeOption[] {
		if (!Array.isArray(value)) return [];
		return value
			.map((option) => {
				const record = asRecord(option);
				if (!record) return null;
				const value = asString(record.value ?? record.id ?? record.label);
				if (!value) return null;
				return {
					value,
					label: asString(record.label, value),
					disabled: asBoolean(record.disabled)
				};
			})
			.filter((option): option is NativeOption => option !== null);
	}

	function normalizeColumns(value: unknown): NativeTableColumn[] {
		if (!Array.isArray(value)) return [];
		return value
			.map((column) => {
				const record = asRecord(column);
				if (!record) return null;
				const key = asString(record.key);
				if (!key) return null;
				return { key, label: asString(record.label, key) };
			})
			.filter((column): column is NativeTableColumn => column !== null);
	}

	function normalizeRows(value: unknown): Array<Record<string, unknown>> {
		return Array.isArray(value)
			? value.map((row) => asRecord(row)).filter((row): row is Record<string, unknown> => row !== null)
			: [];
	}

	function normalizeDataItems(value: unknown): Array<{ id: string; key: string; value: string }> {
		if (!Array.isArray(value)) return [];
		return value
			.map((item, index) => {
				const record = asRecord(item);
				if (!record) return null;
				const key = asString(record.key, `Item ${index + 1}`);
				return {
					id: asString(record.id, `${component.id}-item-${index}`),
					key,
					value: formatValue(record.value)
				};
			})
			.filter((item): item is { id: string; key: string; value: string } => item !== null);
	}

	function normalizeTabs(value: unknown): Array<{ label: string }> {
		if (!Array.isArray(value)) return [];
		return value.map((tab, index) => {
			const record = asRecord(tab);
			return { label: record ? asString(record.label, `Tab ${index + 1}`) : `Tab ${index + 1}` };
		});
	}

	function fieldId(field: NativeField): string {
		return asString(field.id || field.name);
	}

	function fieldType(field: NativeField): string {
		return asString(field.type, 'text');
	}

	function fieldLabel(field: NativeField): string {
		return asString(field.label, fieldId(field));
	}

	function fieldHint(field: NativeField): string {
		return asString(field.hint ?? field.description ?? field.helpText);
	}

	function fieldOptions(field: NativeField): NativeOption[] {
		return normalizeOptions(field.options);
	}

	function fieldRows(field: NativeField): number {
		return Math.max(2, Math.round(asNumber(field.rows, 3)));
	}

	function fieldSpan(field: NativeField): number {
		return Math.max(1, Math.min(3, Math.round(asNumber(field.span, 1))));
	}

	function fieldValue(id: string): string {
		return formValues[id] ?? '';
	}

	function setFieldValue(id: string, value: string, emitChange = false): void {
		formValues = { ...formValues, [id]: value };
		if (emitChange) {
			emitInteraction('change', { values: { ...formValues }, changedField: id });
		}
	}

	function updateTextField(field: NativeField, event: Event): void {
		const id = fieldId(field);
		if (!id) return;
		setFieldValue(id, (event.currentTarget as HTMLInputElement | HTMLTextAreaElement).value, true);
	}

	function updateSelectField(field: NativeField, event: Event): void {
		const id = fieldId(field);
		if (!id) return;
		setFieldValue(id, (event.currentTarget as HTMLSelectElement).value, true);
	}

	function handleFormSubmit(): void {
		emitInteraction('submit', { values: { ...formValues } });
	}

	function toggleMultiValue(value: string, checked: boolean): void {
		const next = checked
			? Array.from(new Set([...multiValues, value]))
			: multiValues.filter((entry) => entry !== value);
		multiValues = next;
		emitInteraction('change', { values: next });
	}

	function updateMultiValue(option: NativeOption, event: Event): void {
		toggleMultiValue(option.value, (event.currentTarget as HTMLInputElement).checked);
	}

	function updateMultiFilter(event: Event): void {
		multiFilter = (event.currentTarget as HTMLInputElement).value;
		if (multiFilter.trim().length > 0) {
			multiFilterByComponentId.set(component.id, multiFilter);
		} else {
			multiFilterByComponentId.delete(component.id);
		}
	}

	function filteredMultiOptions(rawOptions: NativeOption[], query: string): NativeOption[] {
		const normalizedQuery = query.trim().toLowerCase();
		if (!normalizedQuery) return rawOptions;
		return rawOptions.filter((option) => {
			const label = option.label.toLowerCase();
			const value = option.value.toLowerCase();
			return label.includes(normalizedQuery) || value.includes(normalizedQuery);
		});
	}

	function emitInteraction(
		interaction: CrewNativeInteractionKind,
		detail: Record<string, unknown> = {}
	): void {
		dispatch('interaction', {
			componentId: component.id,
			interaction,
			detail,
			sent: false
		});
	}

	function forwardInteraction(event: CustomEvent<CrewNativeInteractionEventDetail>): void {
		dispatch('interaction', event.detail);
	}

	function formatValue(value: unknown): string {
		if (value == null) return '';
		if (
			typeof value === 'string'
			|| typeof value === 'number'
			|| typeof value === 'boolean'
			|| typeof value === 'bigint'
		) {
			return String(value);
		}
		try {
			return JSON.stringify(value, null, 2);
		} catch {
			return String(value);
		}
	}

	function rowValue(row: Record<string, unknown>, key: string): string {
		return formatValue(row[key]);
	}

	function tableKeys(): NativeTableColumn[] {
		if (tableColumns.length > 0) return tableColumns;
		const keys = Array.from(new Set(tableRows.flatMap((row) => Object.keys(row))));
		return keys.map((key) => ({ key, label: key }));
	}

	function stackStyleFor(record: Record<string, unknown>): string {
		const direction = asString(record.direction, 'column') === 'row' ? 'row' : 'column';
		const gap = asString(record.gap, '0.65rem');
		const wrap = asBoolean(record.wrap) ? 'wrap' : 'nowrap';
		const align = asString(record.align, 'stretch');
		return `--crew-stack-direction:${direction};--crew-stack-gap:${gap};--crew-stack-wrap:${wrap};--crew-stack-align:${align};`;
	}

	function gridStyleFor(record: Record<string, unknown>): string {
		const minColumnWidth = asString(record.minColumnWidth, '280px');
		const gap = asString(record.gap, '0.85rem');
		return `--crew-grid-min:${minColumnWidth};--crew-grid-gap:${gap};`;
	}
</script>

{#if component.component_type === 'Card'}
	<article class="crew-native-card" data-component-id={component.id}>
		{#if cardTitle || cardSubtitle || cardBody}
			<header class="crew-native-card__header">
				{#if cardTitle}<h2>{cardTitle}</h2>{/if}
				{#if cardSubtitle}<p class="crew-native-subtitle">{cardSubtitle}</p>{/if}
				{#if cardBody}<p class="crew-native-body">{cardBody}</p>{/if}
			</header>
		{/if}
		{#if children.length > 0}
			<div class="crew-native-card__content">
				{#each children as child (child.id)}
					<svelte:self component={child} on:interaction={forwardInteraction} />
				{/each}
			</div>
		{/if}
	</article>
{:else if component.component_type === 'Stack'}
	<div class="crew-native-stack" data-component-id={component.id} style={stackStyle}>
		{#each children as child (child.id)}
			<svelte:self component={child} on:interaction={forwardInteraction} />
		{/each}
	</div>
{:else if component.component_type === 'Grid'}
	<div class="crew-native-grid" data-component-id={component.id} style={gridStyle}>
		{#each children as child (child.id)}
			<svelte:self component={child} on:interaction={forwardInteraction} />
		{/each}
	</div>
{:else if component.component_type === 'Button'}
	<button
		class="crew-native-button"
		class:crew-native-button--primary={buttonVariant === 'primary'}
		class:crew-native-button--secondary={buttonVariant === 'secondary'}
		class:crew-native-button--outline={buttonVariant === 'outline'}
		class:crew-native-button--sm={buttonSize === 'sm'}
		type="button"
		data-component-id={component.id}
		disabled={buttonDisabled}
		on:click={() => emitInteraction('action')}
	>
		{buttonLabel}
	</button>
{:else if component.component_type === 'Badge' || component.component_type === 'Tag'}
	<span
		class="crew-native-chip"
		class:crew-native-chip--tag={component.component_type === 'Tag'}
		class:crew-native-chip--success={badgeColor === 'success'}
		class:crew-native-chip--warning={badgeColor === 'warning'}
		class:crew-native-chip--error={badgeColor === 'error'}
		class:crew-native-chip--info={badgeColor === 'info'}
		data-component-id={component.id}
	>
		{badgeText}
	</span>
{:else if component.component_type === 'DataList'}
	<dl class="crew-native-data-list" data-component-id={component.id}>
		{#each dataItems as item (item.id)}
			<div>
				<dt>{item.key}</dt>
				<dd>{item.value}</dd>
			</div>
		{/each}
	</dl>
{:else if component.component_type === 'Table'}
	<section class="crew-native-table-section" data-component-id={component.id}>
		{#if componentLabel}<h3>{componentLabel}</h3>{/if}
		{#if tableRows.length === 0}
			<div class="crew-native-empty crew-native-empty--compact">
				<p>No rows</p>
			</div>
		{:else}
			<div class="crew-native-table-wrap">
				<table>
					<thead>
						<tr>
							{#each tableKeys() as column (column.key)}
								<th>{column.label}</th>
							{/each}
						</tr>
					</thead>
					<tbody>
						{#each tableRows as row, rowIndex (`${component.id}-${rowIndex}`)}
							<tr>
								{#each tableKeys() as column (column.key)}
									<td>{rowValue(row, column.key)}</td>
								{/each}
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}
	</section>
{:else if component.component_type === 'Alert'}
	<div
		class="crew-native-alert"
		class:crew-native-alert--error={alertType === 'error'}
		class:crew-native-alert--warning={alertType === 'warning'}
		class:crew-native-alert--success={alertType === 'success'}
		role="alert"
		data-component-id={component.id}
	>
		{alertMessage}
	</div>
{:else if component.component_type === 'EmptyState'}
	<section class="crew-native-empty" data-component-id={component.id}>
		<h3>{asString(props.title, 'Nothing here')}</h3>
		{#if asString(props.description)}<p>{asString(props.description)}</p>{/if}
		{#if asString(props.actionLabel)}
			<button class="crew-native-button crew-native-button--primary" type="button" on:click={() => emitInteraction('action')}>
				{asString(props.actionLabel)}
			</button>
		{/if}
	</section>
{:else if component.component_type === 'CodeBlock'}
	<section class="crew-native-code" data-component-id={component.id}>
		{#if componentLabel}<h3>{componentLabel}</h3>{/if}
		<pre><code>{codeText}</code></pre>
	</section>
{:else if component.component_type === 'TextArea'}
	<label class="crew-native-field" data-component-id={component.id}>
		{#if componentLabel}<span>{componentLabel}</span>{/if}
		<textarea rows={textareaRows} readonly value={textareaValue}></textarea>
	</label>
{:else if component.component_type === 'Text'}
	<p class="crew-native-text" data-component-id={component.id}>{textContent}</p>
{:else if component.component_type === 'Markdown'}
	<div class="crew-native-markdown" data-component-id={component.id}>{asString(props.markdown ?? props.text ?? props.children)}</div>
{:else if component.component_type === 'MetricCard'}
	<article class="crew-native-metric" data-component-id={component.id}>
		<span>{componentLabel || asString(props.label)}</span>
		<strong>{formatValue(props.value)}</strong>
		{#if asString(props.detail)}<small>{asString(props.detail)}</small>{/if}
	</article>
{:else if component.component_type === 'ScrollArea'}
	<div class="crew-native-scroll" data-component-id={component.id}>
		{#each children as child (child.id)}
			<svelte:self component={child} on:interaction={forwardInteraction} />
		{/each}
	</div>
{:else if component.component_type === 'Tabs'}
	<section class="crew-native-tabs" data-component-id={component.id}>
		{#if componentLabel}<h3>{componentLabel}</h3>{/if}
		<div class="crew-native-tab-strip">
			{#each tabs as tab, index (`${component.id}-tab-${index}`)}
				<button
					type="button"
					class="crew-native-tab"
					class:active={index === activeTabIndex}
					on:click={() => (activeTabIndex = index)}
				>
					{tab.label}
				</button>
			{/each}
		</div>
		{#if children[activeTabIndex]}
			<div class="crew-native-tab-panel">
				<svelte:self component={children[activeTabIndex]} on:interaction={forwardInteraction} />
			</div>
		{/if}
	</section>
{:else if component.component_type === 'Form'}
	<form class="crew-native-form" data-component-id={component.id} on:submit|preventDefault={handleFormSubmit}>
		{#if formTitle}<h2>{formTitle}</h2>{/if}
		<div class="crew-native-form-grid">
			{#each fields as field (fieldId(field))}
				{@const id = fieldId(field)}
				{@const span = fieldSpan(field)}
				<label
					class="crew-native-field"
					class:crew-native-field--section={fieldLabel(field).startsWith('---')}
					class:crew-native-field--span-2={span === 2}
					class:crew-native-field--span-3={span >= 3}
				>
					<span>{fieldLabel(field)}</span>
					{#if fieldType(field) === 'textarea'}
						<textarea
							rows={fieldRows(field)}
							required={asBoolean(field.required)}
							disabled={formDisabled || asBoolean(field.disabled)}
							placeholder={asString(field.placeholder)}
							value={fieldValue(id)}
							on:input={(event) => updateTextField(field, event)}
						></textarea>
					{:else if fieldType(field) === 'select'}
						<select
							required={asBoolean(field.required)}
							disabled={formDisabled || asBoolean(field.disabled)}
							value={fieldValue(id)}
							on:change={(event) => updateSelectField(field, event)}
						>
							{#if asString(field.placeholder)}
								<option value="">{asString(field.placeholder)}</option>
							{/if}
							{#each fieldOptions(field) as option (option.value)}
								<option value={option.value} disabled={option.disabled}>{option.label}</option>
							{/each}
						</select>
					{:else}
						<input
							type="text"
							required={asBoolean(field.required)}
							disabled={formDisabled || asBoolean(field.disabled)}
							placeholder={asString(field.placeholder)}
							value={fieldValue(id)}
							on:input={(event) => updateTextField(field, event)}
						/>
					{/if}
					{#if fieldHint(field)}<small class="crew-native-field-hint">{fieldHint(field)}</small>{/if}
				</label>
			{/each}
		</div>
		{#if showSubmit}
			<div class="crew-native-form-actions">
				<button class="crew-native-button crew-native-button--primary" type="submit" disabled={formDisabled}>
					{submitLabel}
				</button>
			</div>
		{/if}
	</form>
{:else if component.component_type === 'MultiSelect'}
	<section class="crew-native-multiselect" data-component-id={component.id}>
		<div class="crew-native-field-head">
			<h3>{componentLabel || 'Select values'}</h3>
			{#if asString(props.placeholder)}<p>{asString(props.placeholder)}</p>{/if}
		</div>
		{#if multiSearchable}
			<label class="crew-native-filter">
				<span class="crew-native-sr-only">{multiSearchPlaceholder}</span>
				<input
					type="search"
					placeholder={multiSearchPlaceholder}
					value={multiFilter}
					on:input={updateMultiFilter}
				/>
			</label>
		{/if}
		{#if options.length === 0}
			<div class="crew-native-empty crew-native-empty--compact"><p>No options loaded</p></div>
		{:else if multiFilteredOptions.length === 0}
			<div class="crew-native-empty crew-native-empty--compact"><p>No matching options</p></div>
		{:else}
			<div class="crew-native-option-grid">
				{#each multiFilteredOptions as option (option.value)}
					<label class="crew-native-check-option" class:disabled={asBoolean(props.disabled) || option.disabled}>
						<input
							type="checkbox"
							checked={multiValues.includes(option.value)}
							disabled={asBoolean(props.disabled) || option.disabled}
							on:change={(event) => updateMultiValue(option, event)}
						/>
						<span>{option.label}</span>
					</label>
				{/each}
			</div>
		{/if}
	</section>
{:else}
	<section class="crew-native-card">
		<header class="crew-native-card__header">
			<h2>{componentLabel || component.component_type}</h2>
		</header>
		{#if children.length > 0}
			<div class="crew-native-card__content">
				{#each children as child (child.id)}
					<svelte:self component={child} on:interaction={forwardInteraction} />
				{/each}
			</div>
		{:else}
			<pre class="crew-native-unknown">{formatValue(props)}</pre>
		{/if}
	</section>
{/if}

<style>
	.crew-native-card,
	.crew-native-form,
	.crew-native-multiselect,
	.crew-native-tabs,
	.crew-native-code,
	.crew-native-table-section,
	.crew-native-metric,
	.crew-native-empty,
	.crew-native-alert {
		box-sizing: border-box;
		min-width: 0;
		max-width: 100%;
		border: 1px solid var(--border-soft, #ebe7e0);
		border-radius: 8px;
		background: var(--bg-card, #fff);
		box-shadow: var(--shadow-sm, 0 1px 3px rgba(0, 0, 0, 0.08));
	}

	.crew-native-card {
		max-width: 100%;
		overflow: hidden;
	}

	.crew-native-card,
	.crew-native-form,
	.crew-native-multiselect,
	.crew-native-tabs,
	.crew-native-code,
	.crew-native-table-section,
	.crew-native-empty,
	.crew-native-alert {
		padding: 1rem;
	}

	.crew-native-card__header,
	.crew-native-card__content,
	.crew-native-form,
	.crew-native-multiselect,
	.crew-native-tabs,
	.crew-native-code,
	.crew-native-table-section,
	.crew-native-empty {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		min-width: 0;
		max-width: 100%;
		gap: 0.75rem;
	}

	.crew-native-card__content {
		margin-top: 0.85rem;
	}

	h2,
	h3,
	p,
	dl {
		margin: 0;
	}

	h2 {
		font: 700 var(--text-lg, 1.1rem) var(--font-display, inherit);
		color: var(--text-primary, #2d2a26);
		letter-spacing: 0;
	}

	h3 {
		font: 700 var(--text-sm, 0.85rem) var(--font-display, inherit);
		color: var(--text-primary, #2d2a26);
		letter-spacing: 0;
	}

	.crew-native-subtitle,
	.crew-native-body,
	.crew-native-text,
	.crew-native-field-head p,
	.crew-native-field-hint,
	.crew-native-empty p,
	.crew-native-metric span,
	.crew-native-metric small {
		color: var(--text-secondary, #6b665e);
		line-height: var(--leading-normal, 1.5);
	}

	.crew-native-stack {
		display: flex;
		flex-direction: var(--crew-stack-direction);
		flex-wrap: var(--crew-stack-wrap);
		gap: var(--crew-stack-gap);
		align-items: var(--crew-stack-align);
		min-width: 0;
	}

	.crew-native-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, var(--crew-grid-min)), 1fr));
		gap: var(--crew-grid-gap);
		min-width: 0;
	}

	.crew-native-button {
		appearance: none;
		border: 1px solid var(--border-soft, #d8d0c5);
		border-radius: 6px;
		background: var(--bg-card, #fff);
		color: var(--text-primary, #2d2a26);
		cursor: pointer;
		font: 700 var(--text-xs, 0.78rem) var(--font-primary, inherit);
		padding: 0.5rem 0.75rem;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			color 120ms ease,
			box-shadow 120ms ease;
	}

	.crew-native-button:hover:not(:disabled) {
		border-color: var(--accent-primary, #ff6b6b);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary, #ff6b6b) 13%, transparent);
	}

	.crew-native-button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.crew-native-button--primary {
		background: var(--accent-primary, #ff6b6b);
		border-color: var(--accent-primary, #ff6b6b);
		color: var(--button-primary-color, #fff);
	}

	.crew-native-button--secondary {
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 12%, var(--bg-card, #fff));
	}

	.crew-native-button--outline {
		background: transparent;
	}

	.crew-native-button--sm {
		padding: 0.38rem 0.62rem;
	}

	.crew-native-chip {
		display: inline-flex;
		align-items: center;
		width: fit-content;
		border: 1px solid var(--border-soft, #d8d0c5);
		border-radius: 999px;
		background: var(--bg-soft, #f6f1e8);
		color: var(--text-primary, #2d2a26);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		line-height: 1;
		padding: 0.28rem 0.5rem;
	}

	.crew-native-chip--tag {
		border-radius: 6px;
	}

	.crew-native-chip--info {
		background: var(--color-info-soft, rgba(77, 157, 224, 0.14));
		color: var(--color-info, #4d9de0);
	}

	.crew-native-chip--success {
		background: var(--color-success-soft, rgba(0, 187, 127, 0.14));
		color: var(--color-success, #00bb7f);
	}

	.crew-native-chip--warning {
		background: var(--color-warning-soft, rgba(255, 230, 109, 0.22));
		color: color-mix(in srgb, var(--color-warning, #ffe66d) 55%, var(--text-primary, #2d2a26));
	}

	.crew-native-chip--error {
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
		color: var(--color-error, #ff6b6b);
	}

	.crew-native-data-list {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 11rem), 1fr));
		gap: 0.65rem;
		min-width: 0;
		max-width: 100%;
	}

	.crew-native-data-list div {
		display: grid;
		gap: 0.15rem;
		min-width: 0;
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 70%, transparent);
		border-radius: 6px;
		background: color-mix(in srgb, var(--bg-card, #fff) 86%, var(--bg-soft, #f6f1e8) 14%);
		padding: 0.6rem;
	}

	.crew-native-data-list dt {
		color: var(--text-secondary, #6b665e);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		text-transform: uppercase;
	}

	.crew-native-data-list dd {
		overflow-wrap: anywhere;
		word-break: break-word;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 650;
	}

	.crew-native-table-wrap {
		overflow-x: auto;
		min-width: 0;
		max-width: 100%;
		-webkit-overflow-scrolling: touch;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-xs, 0.78rem);
	}

	th,
	td {
		border-bottom: 1px solid var(--border-soft, #ebe7e0);
		padding: 0.55rem 0.45rem;
		text-align: left;
		vertical-align: top;
	}

	th {
		color: var(--text-secondary, #6b665e);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 800;
		text-transform: uppercase;
	}

	td {
		overflow-wrap: anywhere;
		white-space: pre-wrap;
	}

	.crew-native-alert {
		color: var(--text-primary, #2d2a26);
		font-size: var(--text-sm, 0.85rem);
	}

	.crew-native-alert--error {
		border-color: var(--color-error, #ff6b6b);
		background: var(--color-error-soft, rgba(255, 107, 107, 0.14));
	}

	.crew-native-alert--warning {
		border-color: var(--color-warning, #ffe66d);
		background: var(--color-warning-soft, rgba(255, 230, 109, 0.18));
	}

	.crew-native-alert--success {
		border-color: var(--color-success, #00bb7f);
		background: var(--color-success-soft, rgba(0, 187, 127, 0.14));
	}

	.crew-native-empty {
		border-style: dashed;
		color: var(--text-secondary, #6b665e);
	}

	.crew-native-empty--compact {
		padding: 0.75rem;
	}

	.crew-native-code pre,
	.crew-native-unknown {
		overflow-x: auto;
		margin: 0;
		border-radius: 6px;
		background: var(--bg-soft, #f6f1e8);
		padding: 0.85rem;
		font: var(--text-xs, 0.78rem) var(--font-mono, monospace);
		white-space: pre-wrap;
	}

	.crew-native-form-grid {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 0.9rem;
		align-items: start;
	}

	.crew-native-field {
		display: grid;
		gap: 0.35rem;
		min-width: 0;
		align-content: start;
	}

	.crew-native-field--span-2 {
		grid-column: span 2;
	}

	.crew-native-field--span-3 {
		grid-column: 1 / -1;
	}

	.crew-native-field--section {
		grid-column: 1 / -1;
		border-top: 1px solid var(--border-soft, #ebe7e0);
		padding-top: 0.85rem;
	}

	.crew-native-field span,
	.crew-native-field-head h3 {
		color: var(--text-primary, #2d2a26);
		font-size: var(--text-xs, 0.78rem);
		font-weight: 800;
	}

	.crew-native-field-hint {
		font-size: var(--text-xs, 0.78rem);
	}

	.crew-native-field input,
	.crew-native-field select {
		min-height: 3.05rem;
	}

	input,
	select,
	textarea {
		box-sizing: border-box;
		width: 100%;
		border: 1px solid var(--border-soft, #d8d0c5);
		border-radius: 6px;
		background: var(--bg-card, #fff);
		color: var(--text-primary, #2d2a26);
		font: 500 var(--text-sm, 0.85rem) var(--font-primary, inherit);
		padding: 0.55rem 0.65rem;
	}

	textarea {
		min-height: 6.5rem;
		resize: vertical;
		font-family: var(--font-mono, monospace);
	}

	input:focus-visible,
	select:focus-visible,
	textarea:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, #ff6b6b) 55%, transparent);
		outline-offset: 2px;
	}

	input:disabled,
	select:disabled,
	textarea:disabled {
		cursor: not-allowed;
		opacity: 0.58;
	}

	.crew-native-form-actions {
		display: flex;
		justify-content: flex-start;
	}

	.crew-native-filter input {
		min-height: 2.65rem;
	}

	.crew-native-sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
	}

	.crew-native-option-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
		gap: 0.55rem;
		max-height: 24rem;
		overflow-y: auto;
		padding-right: 0.2rem;
	}

	.crew-native-check-option {
		display: grid;
		grid-template-columns: 1rem minmax(0, 1fr);
		gap: 0.55rem;
		align-items: center;
		border: 1px solid var(--border-soft, #d8d0c5);
		border-radius: 6px;
		background: color-mix(in srgb, var(--bg-card, #fff) 88%, var(--bg-soft, #f6f1e8));
		padding: 0.55rem 0.65rem;
	}

	.crew-native-check-option.disabled {
		opacity: 0.6;
	}

	.crew-native-check-option input {
		appearance: none;
		display: grid;
		place-content: center;
		width: 1rem;
		height: 1rem;
		margin: 0;
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
		padding: 0;
	}

	.crew-native-check-option input::before {
		content: '';
		width: 0.32rem;
		height: 0.56rem;
		border-right: 2px solid currentColor;
		border-bottom: 2px solid currentColor;
		transform: rotate(42deg) scale(0);
		transform-origin: center;
		transition: transform 120ms ease;
	}

	.crew-native-check-option input:checked {
		background: linear-gradient(
			135deg,
			var(--accent-primary, #ff6b6b),
			color-mix(in srgb, var(--accent-primary, #ff6b6b) 72%, var(--accent-secondary, #4d9de0))
		);
		border-color: var(--accent-primary, #ff6b6b);
		box-shadow:
			0 0 0 3px color-mix(in srgb, var(--accent-primary, #ff6b6b) 18%, transparent),
			0 2px 8px color-mix(in srgb, var(--accent-primary, #ff6b6b) 24%, transparent);
	}

	.crew-native-check-option input:checked::before {
		transform: rotate(42deg) scale(1);
	}

	.crew-native-check-option span {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-xs, 0.78rem);
		font-weight: 700;
	}

	.crew-native-tab-strip {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		border-bottom: 1px solid var(--border-soft, #ebe7e0);
		padding-bottom: 0.35rem;
	}

	.crew-native-tab {
		appearance: none;
		border: 1px solid transparent;
		border-radius: 6px;
		background: transparent;
		color: var(--text-secondary, #6b665e);
		cursor: pointer;
		font: 700 var(--text-xs, 0.78rem) var(--font-primary, inherit);
		padding: 0.42rem 0.7rem;
	}

	.crew-native-tab:hover,
	.crew-native-tab.active {
		border-color: var(--border-soft, #d8d0c5);
		background: color-mix(in srgb, var(--accent-primary, #ff6b6b) 10%, var(--bg-card, #fff));
		color: var(--text-primary, #2d2a26);
	}

	.crew-native-tab-panel {
		min-width: 0;
	}

	.crew-native-metric {
		display: grid;
		gap: 0.2rem;
		padding: 0.9rem;
	}

	.crew-native-metric strong {
		font-size: var(--text-xl, 1.35rem);
		line-height: 1.1;
	}

	.crew-native-scroll {
		display: grid;
		gap: 0.65rem;
		max-height: 34rem;
		overflow-y: auto;
		padding-right: 0.2rem;
	}

	.crew-native-markdown {
		white-space: pre-wrap;
		line-height: var(--leading-normal, 1.5);
		color: var(--text-secondary, #6b665e);
	}

	@media (max-width: 980px) {
		.crew-native-form-grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.crew-native-field--span-3 {
			grid-column: 1 / -1;
		}
	}

	@media (max-width: 720px) {
		.crew-native-form-grid,
		.crew-native-option-grid {
			grid-template-columns: 1fr;
		}

		.crew-native-field--span-2,
		.crew-native-field--span-3 {
			grid-column: 1 / -1;
		}

		.crew-native-data-list {
			grid-template-columns: 1fr;
		}
	}
</style>
