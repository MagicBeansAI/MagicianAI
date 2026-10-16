export interface FormFieldOption {
	value: string;
	label: string;
}

export interface FormSchemaFieldEntry {
	id: string;
	type: string;
	min: number | string | null;
	max: number | string | null;
	step: number | null;
	options: FormFieldOption[];
	defaultValue: string | number | boolean;
}

function asString(value: unknown, fallback: string = ''): string {
	if (typeof value === 'string') return value;
	if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
		return String(value);
	}
	return fallback;
}

export function normalizeFormFieldOptions(value: unknown): FormFieldOption[] {
	if (!Array.isArray(value)) return [];
	const normalized: FormFieldOption[] = [];
	const seen = new Set<string>();
	for (const option of value) {
		if (option == null || typeof option !== 'object' || Array.isArray(option)) continue;
		const rec = option as Record<string, unknown>;
		const hasValue = rec.value !== undefined && rec.value !== null;
		const hasScalarValue = typeof rec.value === 'string'
			|| typeof rec.value === 'number'
			|| typeof rec.value === 'boolean'
			|| typeof rec.value === 'bigint';
		const optionValue = hasScalarValue ? asString(rec.value).trim() : '';
		const optionLabel = asString(rec.label, hasScalarValue ? optionValue : '').trim();
		const effectiveValue = hasScalarValue ? optionValue : optionLabel;
		const effectiveLabel = optionLabel || optionValue;
		const isExplicitEmptyValue = hasScalarValue && hasValue && optionValue === '';
		if (!effectiveValue && !effectiveLabel && !isExplicitEmptyValue) continue;
		const valueKey = hasScalarValue ? optionValue : optionLabel;
		if (seen.has(valueKey)) continue;
		seen.add(valueKey);
		normalized.push({
			value: effectiveValue,
			label: effectiveLabel
		});
	}
	return normalized;
}

export function buildFormSchemaSignature(fields: FormSchemaFieldEntry[]): string {
	return JSON.stringify(fields);
}

export function isRequiredFormValueEmpty(
	fieldType: string,
	value: string | number | boolean
): boolean {
	if (fieldType === 'checkbox') return value !== true;
	if (fieldType === 'number') {
		if (typeof value === 'number') return !Number.isFinite(value);
		if (typeof value === 'string') {
			const trimmed = value.trim();
			if (trimmed.length === 0) return true;
			return !Number.isFinite(Number(trimmed));
		}
		return true;
	}
	if (typeof value === 'number') return !Number.isFinite(value);
	return typeof value !== 'string' || value.trim().length === 0;
}
