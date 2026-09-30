<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { buildStableDomId } from './idUtil';
	import {
		buildFormSchemaSignature,
		isRequiredFormValueEmpty,
		normalizeFormFieldOptions
	} from './formLogic';

	interface FormFieldOption {
		value: string;
		label: string;
	}

	interface FormField {
		id: string;
		label: string;
		type: 'text' | 'textarea' | 'number' | 'email' | 'password' | 'search' | 'date' | 'checkbox' | 'select';
		placeholder: string;
		required: boolean;
		disabled: boolean;
		value: string | number | boolean;
		rows: number;
		min: number | string | undefined;
		max: number | string | undefined;
		step: number | undefined;
		maxLength?: number | undefined;
		options: FormFieldOption[];
	}

	interface FormSubmitDetail {
		timestamp: number;
		values: Record<string, string | number | boolean>;
		errors: Record<string, string>;
	}

	export let title: string = '';
	export let showSubmit: boolean = false;
	export let submitLabel: string = 'Submit';
	export let disabled: boolean = false;
	export let fields: Array<Record<string, unknown>> = [];
	export let idBase: string = '';

	const dispatch = createEventDispatcher<{
		submit: FormSubmitDetail;
		change: { fieldId: string; values: Record<string, string | number | boolean> };
	}>();

	// Surfaces that want live field updates (e.g. an agent picker or a cron field
	// whose value must reach page state WITHOUT a submit button) listen for `change`.
	// Fired on discrete changes (select/checkbox) immediately and on text-field BLUR
	// (commit-on-blur) — NOT per keystroke, to avoid re-render churn on the host.
	function commitFieldChange(fieldId: string): void {
		dispatch('change', { fieldId, values });
	}
	let values: Record<string, string | number | boolean> = {};
	let errors: Record<string, string> = {};
	let schemaSignature = '';
	let defaultValuesByField: Record<string, string | number | boolean> = {};

	const FIELD_TYPES: ReadonlySet<string> = new Set([
		'text',
		'textarea',
		'number',
		'email',
		'password',
		'search',
		'date',
		'checkbox',
		'select'
	]);

	$: normalizedFields = normalizeFields(Array.isArray(fields) ? fields : []);
	$: {
		const nextSignature = buildFormSchemaSignature(
			normalizedFields.map((field) => ({
				id: field.id,
				type: field.type,
				min: field.min ?? null,
				max: field.max ?? null,
				step: field.step ?? null,
				options: field.options.map((option) => ({
					value: option.value,
					label: option.label
				})),
				defaultValue: field.value
			}))
		);
		if (nextSignature !== schemaSignature) {
			const previousValues = values;
			const previousDefaults = defaultValuesByField;
			schemaSignature = nextSignature;
			values = normalizedFields.reduce<Record<string, string | number | boolean>>((acc, field) => {
				const hasPreviousValue = Object.prototype.hasOwnProperty.call(previousValues, field.id);
				const previousDefault = previousDefaults[field.id];
				const defaultChanged = hasPreviousValue && previousDefault !== undefined && !Object.is(previousDefault, field.value);
				const candidate = hasPreviousValue && !defaultChanged
					? previousValues[field.id]
					: field.value;
				acc[field.id] = normalizeFieldValueForType(field, candidate);
				return acc;
			}, {});
			defaultValuesByField = normalizedFields.reduce<Record<string, string | number | boolean>>((acc, field) => {
				acc[field.id] = field.value;
				return acc;
			}, {});
			const activeFieldIds = new Set(normalizedFields.map((field) => field.id));
			errors = Object.fromEntries(
				Object.entries(errors).filter(([fieldId]) => activeFieldIds.has(fieldId))
			);
		}
	}

	function handleSubmit(): void {
		if (disabled) return;
		const nextErrors = validateRequiredFields();
		errors = nextErrors;
		if (Object.keys(nextErrors).length > 0) return;
		dispatch('submit', { timestamp: Date.now(), values, errors: {} });
	}

	function toString(value: unknown, fallback: string = ''): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		return fallback;
	}

	function toNumber(value: unknown): number | undefined {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value !== 'string') return undefined;
		const trimmed = value.trim();
		if (trimmed === '') return undefined;
		const parsed = Number(trimmed);
		return Number.isFinite(parsed) ? parsed : undefined;
	}

	function toBoolean(value: unknown, fallback: boolean = false): boolean {
		if (typeof value === 'boolean') return value;
		if (typeof value === 'number') return Number.isFinite(value) ? value !== 0 : fallback;
		if (typeof value === 'string') {
			const normalized = value.trim().toLowerCase();
			if (normalized === 'true' || normalized === '1' || normalized === 'yes' || normalized === 'on') return true;
			if (normalized === 'false' || normalized === '0' || normalized === 'no' || normalized === 'off' || normalized === '') return false;
		}
		return fallback;
	}

	function normalizeFieldType(value: unknown): FormField['type'] {
		const normalized = toString(value).trim().toLowerCase();
		if (FIELD_TYPES.has(normalized)) {
			return normalized as FormField['type'];
		}
		return 'text';
	}

	function normalizeFields(inputFields: Array<Record<string, unknown>>): FormField[] {
		const normalized: FormField[] = [];
		const seen = new Set<string>();
		for (let index = 0; index < inputFields.length; index++) {
			const field = inputFields[index];
			const baseId = toString(field.id, toString(field.name, `field-${index + 1}`)).trim() || `field-${index + 1}`;
			let nextId = baseId;
			let dedupeCounter = 1;
			while (seen.has(nextId)) {
				dedupeCounter += 1;
				nextId = `${baseId}-${dedupeCounter}`;
			}
			seen.add(nextId);
			const type = normalizeFieldType(field.type);
			const label = toString(field.label).trim();
			const placeholder = toString(field.placeholder).trim();
			const required = toBoolean(field.required, false);
			const fieldDisabled = toBoolean(field.disabled, false);
			const rows = Math.max(1, Math.floor(toNumber(field.rows) ?? 3));
			const min = type === 'number'
				? toNumber(field.min)
				: toString(field.min).trim() || undefined;
			const max = type === 'number'
				? toNumber(field.max)
				: toString(field.max).trim() || undefined;
			const step = type === 'number' ? toNumber(field.step) : undefined;
			const options = type === 'select' ? normalizeFormFieldOptions(field.options) : [];
			let fieldValue: string | number | boolean;
			if (type === 'checkbox') {
				fieldValue = toBoolean(field.value, false);
			} else if (type === 'number') {
				const numeric = toNumber(field.value);
				fieldValue = numeric !== undefined ? numeric : '';
			} else if (type === 'select') {
				const rawValue = toString(field.value).trim();
				const hasRawValue = options.some((option) => option.value === rawValue);
				if (hasRawValue) {
					fieldValue = rawValue;
				} else if (options.length === 0) {
					fieldValue = '';
				} else if (placeholder) {
					fieldValue = '';
				} else {
					fieldValue = options[0].value;
				}
			} else {
				fieldValue = toString(field.value).trim();
			}
			normalized.push({
				id: nextId,
				label,
				type,
				placeholder,
				required,
				disabled: fieldDisabled,
				value: fieldValue,
				rows,
				min,
				max,
				step,
				options
			});
		}
		return normalized;
	}

	function normalizeFieldValueForType(field: FormField, candidate: unknown): string | number | boolean {
		if (field.type === 'checkbox') {
			return toBoolean(candidate, false);
		}
		if (field.type === 'number') {
			if (typeof candidate === 'number' && Number.isFinite(candidate)) return candidate;
			if (typeof candidate === 'string') return candidate;
			const parsed = toNumber(candidate);
			return parsed !== undefined ? parsed : '';
		}
		if (field.type === 'select') {
			const normalizedCandidate = toString(candidate).trim();
			if (normalizedCandidate === '') return '';
			const isAllowed = field.options.some((option) => option.value === normalizedCandidate);
			return isAllowed ? normalizedCandidate : field.value;
		}
		return toString(candidate).trim();
	}

	function fieldInputId(fieldId: string): string {
		const scope = idBase ? `${idBase}:${fieldId}` : fieldId;
		return buildStableDomId('muij-form-field', scope);
	}

	function fieldErrorId(fieldId: string): string {
		return `${fieldInputId(fieldId)}-error`;
	}

	function updateFieldValue(fieldId: string, nextValue: string | number | boolean): void {
		values = { ...values, [fieldId]: nextValue };
		if (errors[fieldId]) {
			const { [fieldId]: _removed, ...rest } = errors;
			errors = rest;
		}
	}

	function fieldValueAsString(fieldId: string): string {
		const value = values[fieldId];
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean') return String(value);
		return '';
	}

	function fieldValueForNumberInput(fieldId: string): string | number {
		const value = values[fieldId];
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string') return value;
		return '';
	}

	function valueIsEmpty(field: FormField, value: string | number | boolean): boolean {
		return isRequiredFormValueEmpty(field.type, value);
	}

	function validateRequiredFields(): Record<string, string> {
		const nextErrors: Record<string, string> = {};
		for (const field of normalizedFields) {
			if (!field.required) continue;
			if (field.disabled) continue;
			const currentValue = values[field.id];
			if (valueIsEmpty(field, currentValue)) {
				nextErrors[field.id] = `${field.label || field.id} is required.`;
			}
		}
		return nextErrors;
	}
</script>

<form class="muij-form" on:submit|preventDefault={handleSubmit} aria-label={title || 'Form'}>
	{#if title.trim().length > 0}
		<div class="muij-form-title">{title}</div>
	{/if}
	<div class="muij-form-body">
		{#if normalizedFields.length > 0}
			<div class="muij-form-generated-fields">
				{#each normalizedFields as field (field.id)}
					<div class="muij-form-field">
						{#if field.type === 'checkbox'}
							<label class="muij-form-checkbox-row" for={fieldInputId(field.id)}>
									<input
										id={fieldInputId(field.id)}
										type="checkbox"
										checked={values[field.id] === true}
										disabled={disabled || field.disabled}
										required={field.required}
										aria-required={field.required ? 'true' : undefined}
										aria-invalid={errors[field.id] ? 'true' : undefined}
										aria-describedby={errors[field.id] ? fieldErrorId(field.id) : undefined}
										on:change={(event) => {
											updateFieldValue(field.id, (event.currentTarget as HTMLInputElement).checked);
											commitFieldChange(field.id);
										}}
									/>
								<span class="muij-form-checkbox-label">
									{field.label || field.placeholder || field.id}
									{#if field.required}<span aria-hidden="true"> *</span>{/if}
								</span>
							</label>
						{:else}
							{#if field.label}
								<label class="muij-form-field-label" for={fieldInputId(field.id)}>
									{field.label}
									{#if field.required}<span aria-hidden="true"> *</span>{/if}
								</label>
							{/if}
							{#if field.type === 'textarea'}
								<textarea
									id={fieldInputId(field.id)}
									class="muij-form-field-input"
									rows={field.rows}
									placeholder={field.placeholder}
									disabled={disabled || field.disabled}
									required={field.required}
									aria-invalid={errors[field.id] ? 'true' : undefined}
									aria-describedby={errors[field.id] ? fieldErrorId(field.id) : undefined}
									aria-label={!field.label ? (field.placeholder || field.id) : undefined}
									value={fieldValueAsString(field.id)}
									on:input={(event) => updateFieldValue(field.id, (event.currentTarget as HTMLTextAreaElement).value)}
									on:blur={() => commitFieldChange(field.id)}
								></textarea>
							{:else if field.type === 'select'}
								<select
									id={fieldInputId(field.id)}
									class="muij-form-field-input"
									disabled={disabled || field.disabled}
									required={field.required}
									aria-invalid={errors[field.id] ? 'true' : undefined}
									aria-describedby={errors[field.id] ? fieldErrorId(field.id) : undefined}
									aria-label={!field.label ? (field.placeholder || field.id) : undefined}
									value={fieldValueAsString(field.id)}
									on:change={(event) => {
										updateFieldValue(field.id, (event.currentTarget as HTMLSelectElement).value);
										commitFieldChange(field.id);
									}}
								>
									{#if field.placeholder}
										<option value="" disabled={field.required}>{field.placeholder}</option>
									{/if}
									{#each field.options as option, optionIndex (`${option.value}:${optionIndex}`)}
										<option value={option.value}>{option.label}</option>
									{/each}
								</select>
							{:else if field.type === 'number'}
								<input
									id={fieldInputId(field.id)}
									class="muij-form-field-input"
									type="number"
									min={typeof field.min === 'number' ? field.min : undefined}
									max={typeof field.max === 'number' ? field.max : undefined}
									step={field.step}
									placeholder={field.placeholder}
									disabled={disabled || field.disabled}
									required={field.required}
									aria-invalid={errors[field.id] ? 'true' : undefined}
									aria-describedby={errors[field.id] ? fieldErrorId(field.id) : undefined}
									aria-label={!field.label ? (field.placeholder || field.id) : undefined}
									value={fieldValueForNumberInput(field.id)}
									on:input={(event) => {
										const raw = (event.currentTarget as HTMLInputElement).value;
										if (raw.trim() === '') {
											updateFieldValue(field.id, '');
											return;
										}
										const parsed = Number(raw);
										updateFieldValue(field.id, Number.isFinite(parsed) ? parsed : raw);
									}}
									on:blur={() => commitFieldChange(field.id)}
								/>
							{:else}
								<input
									id={fieldInputId(field.id)}
									class="muij-form-field-input"
									type={field.type}
									min={typeof field.min === 'string' ? field.min : undefined}
									max={typeof field.max === 'string' ? field.max : undefined}
									maxlength={field.maxLength ?? undefined}
									placeholder={field.placeholder}
									disabled={disabled || field.disabled}
									required={field.required}
									aria-invalid={errors[field.id] ? 'true' : undefined}
									aria-describedby={errors[field.id] ? fieldErrorId(field.id) : undefined}
									aria-label={!field.label ? (field.placeholder || field.id) : undefined}
									value={fieldValueAsString(field.id)}
									on:input={(event) => updateFieldValue(field.id, (event.currentTarget as HTMLInputElement).value)}
									on:blur={() => commitFieldChange(field.id)}
								/>
							{/if}
						{/if}
						{#if errors[field.id]}
							<div id={fieldErrorId(field.id)} class="muij-form-field-error" role="alert">
								{errors[field.id]}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
		<slot />
	</div>
	{#if showSubmit}
		<button type="submit" class="muij-form-submit" disabled={disabled}>{submitLabel}</button>
	{/if}
</form>

<style>
	.muij-form {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		padding: var(--space-sm);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		background: var(--bg-card);
	}

	.muij-form-title {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.muij-form-body {
		display: grid;
		gap: var(--space-sm);
	}

	.muij-form-generated-fields {
		display: grid;
		gap: var(--space-sm);
	}

	.muij-form-field {
		display: grid;
		gap: 4px;
	}

	.muij-form-field-label,
	.muij-form-checkbox-label,
	.muij-form-field-error {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
	}

	.muij-form-field-label,
	.muij-form-checkbox-label {
		color: var(--text-secondary);
	}

	.muij-form-checkbox-row {
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	.muij-form-field-input {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-body);
		padding: 6px 10px;
	}

	textarea.muij-form-field-input {
		min-height: 68px;
		resize: vertical;
	}

	.muij-form-field-input:disabled {
		opacity: 0.55;
	}

	.muij-form-field-error {
		color: #b91c1c;
	}

	.muij-form-submit {
		align-self: flex-end;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--accent-primary);
		color: var(--text-on-accent, #fff);
		font-family: var(--font-primary);
		font-size: 0.75rem;
		padding: 6px 12px;
		cursor: pointer;
	}

	.muij-form-submit:disabled {
		opacity: 0.55;
		cursor: default;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme="retro-16bit"]) .muij-form {
		border-radius: 0;
		border: 2px solid #ffb000;
		background: #000;
	}

	:global([data-theme="retro-16bit"]) .muij-form-field-input {
		border-radius: 0;
		border: 1px solid #402c00;
		background: #000;
		color: #ffb000;
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .muij-form-field-input:focus {
		border-color: #ffb000;
		outline: none;
	}

	:global([data-theme="retro-16bit"]) .muij-form-submit {
		border-radius: 0;
		border: 1px solid #ffb000;
		background: #ffb000;
		color: #000;
		font-family: var(--font-mono);
		text-transform: uppercase;
		box-shadow: 4px 4px 0px #805800;
	}

	:global([data-theme="retro-16bit"]) .muij-form-submit:hover:not(:disabled) {
		transform: translate(2px, 2px);
		box-shadow: 2px 2px 0px #805800;
	}

	/* Retro 16-bit Light Theme Overrides */
	:global([data-theme="retro-16bit-light"]) .muij-form {
		border-radius: 0;
		border: 2px solid #1a1a1a;
		background: #f5f5f0;
	}

	:global([data-theme="retro-16bit-light"]) .muij-form-field-input {
		border-radius: 0;
		border: 1px solid #999999;
		background: #ffffff;
		color: #1a1a1a;
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit-light"]) .muij-form-field-input:focus {
		border-color: #1a1a1a;
		outline: none;
	}

	:global([data-theme="retro-16bit-light"]) .muij-form-submit {
		border-radius: 0;
		border: 2px solid #1a1a1a;
		background: #1a1a1a;
		color: #f5f5f0;
		font-family: var(--font-mono);
		text-transform: uppercase;
		box-shadow: 4px 4px 0px #999999;
	}

	:global([data-theme="retro-16bit-light"]) .muij-form-submit:hover:not(:disabled) {
		transform: translate(2px, 2px);
		box-shadow: 2px 2px 0px #999999;
	}

	:global([data-theme="retro-16bit-light"]) .muij-form-submit:active:not(:disabled) {
		transform: translate(4px, 4px);
		box-shadow: none;
	}
</style>
