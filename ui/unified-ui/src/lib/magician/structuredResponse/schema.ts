import type {
	StructuredArtifactRefV1,
	StructuredResponseActionV1,
	StructuredResponseBlockV1,
	StructuredResponseV1,
	StructuredSourceRefV1
} from './types';

type ValidationOk = {
	ok: true;
	reason?: never;
};

type ValidationErr = {
	ok: false;
	reason: string;
};

export type StructuredResponseValidation = ValidationOk | ValidationErr;

const MAX_BLOCKS = 32;
const MAX_ACTIONS = 8;
const MAX_ITEMS = 100;
const MAX_COLUMNS = 12;
const MAX_ROWS = 100;
const MAX_PLAIN_TEXT_BYTES = 32 * 1024;
const MAX_PRESENTATION_BYTES = 64 * 1024;
const MAX_VALUE_BYTES = 2 * 1024;
const MAX_LABEL_BYTES = 160;
const MAX_ARTIFACT_SIZE = 2_147_483_647;

const ACCEPTED_TONES = new Set(['neutral', 'success', 'warning', 'danger', 'info']);
const ACCEPTED_ALIGNMENT = new Set(['start', 'center', 'end']);
const ACCEPTED_LIST_STYLES = new Set(['bullets', 'steps', 'checks']);
const ACCEPTED_ACTION_KINDS = new Set([
	'copy_text',
	'open_url',
	'open_task',
	'open_artifact',
	'send_follow_up'
]);
const ACCEPTED_RESPONSE_KINDS = new Set([
	'markdown',
	'text',
	'callout',
	'key_values',
	'table',
	'list',
	'artifacts',
	'sources',
	'metrics'
]);
const ACCEPTED_CALLOUT_TONES = new Set(['info', 'success', 'warning', 'danger']);
const ACCEPTED_METRIC_TREND = new Set(['up', 'down', 'flat']);

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function mustBeString(value: unknown): value is string {
	return typeof value === 'string';
}

function mustBeStringOrUndefined(value: unknown): value is string | undefined {
	return value === undefined || typeof value === 'string';
}

function mustBeBooleanOrUndefined(value: unknown): value is boolean | undefined {
	return value === undefined || typeof value === 'boolean';
}

function mustBeNumberOrUndefined(value: unknown): value is number | undefined {
	return value === undefined || (typeof value === 'number' && Number.isFinite(value));
}

function hasDisallowedControlCharacters(value: string): boolean {
	for (const character of value) {
		const code = character.codePointAt(0);
		if (code === undefined) continue;
		if (code <= 0x08 || code === 0x0b || code === 0x0c) return true;
		if ((code >= 0x0e && code <= 0x1f) || (code >= 0x7f && code <= 0x9f)) return true;
	}
	return false;
}

function isSafeText(value: unknown, maxBytes = MAX_VALUE_BYTES): value is string {
	return (
		typeof value === 'string' &&
		value.trim().length > 0 &&
		new TextEncoder().encode(value).length <= maxBytes &&
		!hasDisallowedControlCharacters(value)
	);
}

function isOptionalSafeText(value: unknown, maxBytes = MAX_VALUE_BYTES): value is string | undefined {
	return value === undefined || isSafeText(value, maxBytes);
}

function isSafeReference(value: unknown): value is string {
	return isSafeText(value, MAX_VALUE_BYTES);
}

function isSafeHttpUrl(value: unknown): value is string {
	if (!isSafeReference(value)) return false;
	try {
		const url = new URL(value);
		return url.protocol === 'http:' || url.protocol === 'https:';
	} catch {
		return false;
	}
}

function validateSource(value: unknown): value is StructuredSourceRefV1 {
	if (!isRecord(value)) return false;
	if (!isSafeText(value.label, MAX_LABEL_BYTES)) return false;
	if (!isSafeHttpUrl(value.href)) return false;
	return true;
}

function validateArtifactRef(value: unknown): value is StructuredArtifactRefV1 {
	if (!isRecord(value)) return false;
	if (!isSafeText(value.label, MAX_LABEL_BYTES)) return false;
	if (value.href !== undefined && !isSafeHttpUrl(value.href)) return false;
	if (value.artifact_id !== undefined && !isSafeText(value.artifact_id, MAX_LABEL_BYTES)) return false;
	if (value.mime_type !== undefined && !isSafeText(value.mime_type, MAX_LABEL_BYTES)) return false;
	if (
		value.size !== undefined &&
		(typeof value.size !== 'number' ||
			!Number.isSafeInteger(value.size) ||
			value.size < 0 ||
			value.size > MAX_ARTIFACT_SIZE)
	) {
		return false;
	}
	if (value.source !== undefined || value.absolute_path !== undefined || value.relative_path !== undefined) {
		return false;
	}
	return true;
}

function validateStringMapRecord(
	value: unknown,
	label: string,
	blockIndex: number
): string | null {
	if (!isRecord(value)) {
		return `block[${blockIndex}].${label} row is not an object`;
	}
	for (const cell of Object.values(value)) {
		if (!isSafeText(cell)) {
			return `block[${blockIndex}].${label} contains invalid cell text`;
		}
	}
	return null;
}

function validateBlock(block: unknown, index: number): string | null {
	if (!isRecord(block)) {
		return `block[${index}] is not an object`;
	}

	const candidate = block as { kind: string; [key: string]: unknown };
	if (!mustBeString(candidate.kind)) {
		return `block[${index}].kind must be a string`;
	}
	if (!ACCEPTED_RESPONSE_KINDS.has(candidate.kind)) {
		return `block[${index}].kind unsupported: ${candidate.kind}`;
	}

	switch (candidate.kind) {
		case 'markdown': {
			if (!isSafeText(candidate.text, MAX_PLAIN_TEXT_BYTES)) {
				return `block[${index}].text must be a string`;
			}
			return null;
		}
		case 'text': {
			if (!isSafeText(candidate.text)) {
				return `block[${index}].text must be a string`;
			}
			return null;
		}
		case 'callout': {
			if (!mustBeString(candidate.tone) || !ACCEPTED_CALLOUT_TONES.has(candidate.tone)) {
				return `block[${index}].tone invalid`;
			}
			if (!isSafeText(candidate.text, MAX_PLAIN_TEXT_BYTES)) {
				return `block[${index}].text must be a string`;
			}
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			return null;
		}
		case 'key_values': {
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			if (!Array.isArray(candidate.items) || candidate.items.length > MAX_ITEMS) {
				return `block[${index}].items is invalid or too large`;
			}
			for (let i = 0; i < candidate.items.length; i += 1) {
				const item = candidate.items[i];
				if (!isRecord(item)) {
					return `block[${index}].items[${i}] must be an object`;
				}
				if (!isSafeText(item.label, MAX_LABEL_BYTES) || !isSafeText(item.value)) {
					return `block[${index}].items[${i}] must include string label/value`;
				}
				if (!isOptionalSafeText(item.hint)) {
					return `block[${index}].items[${i}].hint must be a string`;
				}
			}
			return null;
		}
		case 'table': {
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			if (!Array.isArray(candidate.columns) || candidate.columns.length === 0 || candidate.columns.length > MAX_COLUMNS) {
				return `block[${index}].columns is invalid`;
			}
			for (let i = 0; i < candidate.columns.length; i += 1) {
				const column = candidate.columns[i];
				if (!isRecord(column)) {
					return `block[${index}].columns[${i}] must be an object`;
				}
				if (!isSafeText(column.key, MAX_LABEL_BYTES) || !isSafeText(column.label, MAX_LABEL_BYTES)) {
					return `block[${index}].columns[${i}] must include string key/label`;
				}
				if (!mustBeStringOrUndefined(column.alignment)) {
					return `block[${index}].columns[${i}].alignment invalid`;
				}
				if (typeof column.alignment === 'string' && !ACCEPTED_ALIGNMENT.has(column.alignment)) {
					return `block[${index}].columns[${i}].alignment invalid`;
				}
			}
			if (!Array.isArray(candidate.rows) || candidate.rows.length > MAX_ROWS) {
				return `block[${index}].rows is invalid`;
			}
			const columnKeys = new Set(candidate.columns.map((column) => (column as Record<string, unknown>).key));
			if (columnKeys.size !== candidate.columns.length) {
				return `block[${index}].columns contains duplicate keys`;
			}
			for (let i = 0; i < candidate.rows.length; i += 1) {
				const rowError = validateStringMapRecord(candidate.rows[i], `rows[${i}]`, index);
				if (rowError) return rowError;
				const row = candidate.rows[i] as Record<string, unknown>;
				if (Object.keys(row).length !== columnKeys.size || Object.keys(row).some((key) => !columnKeys.has(key))) {
					return `block[${index}].rows[${i}] does not match table columns`;
				}
			}
			return null;
		}
		case 'list': {
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			if (!Array.isArray(candidate.items) || candidate.items.length > MAX_ITEMS) {
				return `block[${index}].items is invalid or too large`;
			}
			if (
				candidate.style !== undefined &&
				(!mustBeString(candidate.style) || !ACCEPTED_LIST_STYLES.has(candidate.style))
			) {
				return `block[${index}].style invalid`;
			}
			for (let i = 0; i < candidate.items.length; i += 1) {
				const item = candidate.items[i];
				if (!isRecord(item)) {
					return `block[${index}].items[${i}] must be an object`;
				}
				if (!isSafeText(item.text)) {
					return `block[${index}].items[${i}].text must be a string`;
				}
				if (!isOptionalSafeText(item.detail)) {
					return `block[${index}].items[${i}].detail invalid`;
				}
				if (!mustBeBooleanOrUndefined(item.checked)) {
					return `block[${index}].items[${i}].checked invalid`;
				}
			}
			return null;
		}
		case 'artifacts': {
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			if (!Array.isArray(candidate.items) || candidate.items.length > MAX_ITEMS) {
				return `block[${index}].items is invalid or too large`;
			}
			for (let i = 0; i < candidate.items.length; i += 1) {
				if (!validateArtifactRef(candidate.items[i])) {
					return `block[${index}].items[${i}] must be an artifact reference`;
				}
			}
			return null;
		}
		case 'sources': {
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			if (!Array.isArray(candidate.items) || candidate.items.length > MAX_ITEMS) {
				return `block[${index}].items is invalid or too large`;
			}
			for (let i = 0; i < candidate.items.length; i += 1) {
				if (!validateSource(candidate.items[i])) {
					return `block[${index}].items[${i}] must be a source reference`;
				}
			}
			return null;
		}
		case 'metrics': {
			if (!isOptionalSafeText(candidate.title, MAX_LABEL_BYTES)) {
				return `block[${index}].title must be a string`;
			}
			if (!Array.isArray(candidate.items) || candidate.items.length > MAX_ITEMS) {
				return `block[${index}].items is invalid or too large`;
			}
			for (let i = 0; i < candidate.items.length; i += 1) {
				const item = candidate.items[i];
				if (!isRecord(item)) {
					return `block[${index}].items[${i}] must be an object`;
				}
				if (!isSafeText(item.label, MAX_LABEL_BYTES) || !isSafeText(item.value)) {
					return `block[${index}].items[${i}] must include string label/value`;
				}
				if (!isOptionalSafeText(item.unit)) {
					return `block[${index}].items[${i}].unit must be a string`;
				}
				if (!mustBeStringOrUndefined(item.trend)) {
					return `block[${index}].items[${i}].trend must be a string`;
				}
				if (typeof item.trend === 'string' && !ACCEPTED_METRIC_TREND.has(item.trend)) {
					return `block[${index}].items[${i}].trend invalid`;
				}
			}
			return null;
		}
	}

	return `block[${index}].kind unsupported`;
}

function validateAction(action: unknown, index: number): string | null {
	if (!isRecord(action)) return `action[${index}] is not an object`;
	if (!mustBeString(action.kind) || !ACCEPTED_ACTION_KINDS.has(action.kind)) {
		return `action[${index}].kind unsupported`;
	}
	if (!isSafeText(action.label, MAX_LABEL_BYTES)) return `action[${index}].label must be a safe string`;
	switch (action.kind) {
		case 'copy_text':
			if (!isSafeText(action.text)) return `action[${index}].text must be a string`;
			return null;
		case 'open_url':
			if (!isSafeHttpUrl(action.url)) return `action[${index}].url must be an http(s) URL`;
			return null;
		case 'open_task':
			if (!isSafeReference(action.task_id)) return `action[${index}].task_id must be a string`;
			return null;
		case 'open_artifact':
			if (!isSafeText(action.artifact_id, MAX_LABEL_BYTES)) return `action[${index}].artifact_id must be a string`;
			return null;
		case 'send_follow_up':
			if (!isSafeText(action.prompt)) return `action[${index}].prompt must be a string`;
			return null;
		case 'invoke_server_action':
			return `action[${index}].kind is unsupported`;
		default:
			return null;
	}
}

function validateModelContext(value: unknown): string | null {
	if (value === undefined) return null;
	if (!isRecord(value)) return 'model_context is invalid';
	if (!isSafeText(value.summary)) return 'model_context.summary must be a safe string';
	if (value.visible_facts !== undefined) {
		if (!Array.isArray(value.visible_facts) || value.visible_facts.some((fact) => !isSafeText(fact))) {
			return 'model_context.visible_facts is invalid';
		}
	}
	if (!isOptionalSafeText(value.selected_item, MAX_LABEL_BYTES)) {
		return 'model_context.selected_item must be a safe string';
	}
	if (value.privacy !== 'model_visible' && value.privacy !== 'local_only') {
		return 'model_context.privacy is invalid';
	}
	return null;
}

function validateMeta(value: unknown): string | null {
	if (value === undefined) return null;
	if (!isRecord(value)) return 'meta is invalid';
	for (const key of ['task_id', 'execution_id', 'chat_turn_id', 'source_surface', 'response_id']) {
		if (!isOptionalSafeText(value[key], MAX_LABEL_BYTES)) return `meta.${key} must be a safe string`;
	}
	if (value.provenance !== undefined) {
		if (!Array.isArray(value.provenance)) return 'meta.provenance is invalid';
		for (const item of value.provenance) {
			if (!isRecord(item) || !isSafeText(item.id, MAX_LABEL_BYTES)) return 'meta.provenance item is invalid';
			if (!isOptionalSafeText(item.label, MAX_LABEL_BYTES)) return 'meta.provenance label is invalid';
			if (!isOptionalSafeText(item.ref)) return 'meta.provenance ref is invalid';
		}
	}
	if (
		value.confidence !== undefined &&
		(typeof value.confidence !== 'number' ||
			!Number.isFinite(value.confidence) ||
			value.confidence < 0 ||
			value.confidence > 1)
	) {
		return 'meta.confidence is invalid';
	}
	if (value.cost !== undefined) {
		if (!isRecord(value.cost)) return 'meta.cost is invalid';
		for (const key of ['input_tokens', 'output_tokens', 'total_tokens']) {
			const tokenCount = value.cost[key];
			if (
				tokenCount !== undefined &&
				(typeof tokenCount !== 'number' || !Number.isInteger(tokenCount) || tokenCount < 0)
			) {
				return `meta.cost.${key} is invalid`;
			}
		}
		if (!mustBeNumberOrUndefined(value.cost.cost_usd)) return 'meta.cost.cost_usd is invalid';
		if (!isOptionalSafeText(value.cost.model, MAX_LABEL_BYTES)) return 'meta.cost.model is invalid';
	}
	return null;
}

export function validateStructuredResponse(response: unknown): StructuredResponseValidation {
	if (!isRecord(response)) {
		return {
			ok: false,
			reason: 'response is not an object'
		};
	}

	if ((response as { schema?: unknown }).schema !== 'magician.structured_response') {
		return {
			ok: false,
			reason: 'schema must be magician.structured_response'
		};
	}

	if ((response as { version?: unknown }).version !== 1) {
		return {
			ok: false,
			reason: 'version must be 1'
		};
	}

	const castResponse = response as Record<string, unknown>;

	if (!isOptionalSafeText(castResponse.title, MAX_LABEL_BYTES)) {
		return {
			ok: false,
			reason: 'title must be a safe string'
		};
	}
	if (!isOptionalSafeText(castResponse.summary)) {
		return {
			ok: false,
			reason: 'summary must be a safe string'
		};
	}
	if (!isSafeText(castResponse.plain_text, MAX_PLAIN_TEXT_BYTES)) {
		return {
			ok: false,
			reason: 'plain_text must be non-empty safe text'
		};
	}
	if (castResponse.tone !== undefined && (!mustBeString(castResponse.tone) || !ACCEPTED_TONES.has(castResponse.tone))) {
		return {
			ok: false,
			reason: 'tone invalid'
		};
	}
	if (!Array.isArray(castResponse.blocks) || castResponse.blocks.length === 0 || castResponse.blocks.length > MAX_BLOCKS) {
		return {
			ok: false,
			reason: 'blocks must be an array with max size 32'
		};
	}

	for (let i = 0; i < castResponse.blocks.length; i += 1) {
		const reason = validateBlock(castResponse.blocks[i], i);
		if (reason) {
			return { ok: false, reason };
		}
	}

	const actions = castResponse.actions;
	if (actions !== undefined) {
		if (!Array.isArray(actions) || actions.length > MAX_ACTIONS) {
			return {
				ok: false,
				reason: 'actions must be an array with max size 8'
			};
		}
		for (let i = 0; i < actions.length; i += 1) {
			const reason = validateAction(actions[i], i);
			if (reason) {
				return { ok: false, reason };
			}
		}
	}

	const metaReason = validateMeta(castResponse.meta);
	if (metaReason) {
		return {
			ok: false,
			reason: metaReason
		};
	}
	const modelContextReason = validateModelContext(castResponse.model_context);
	if (modelContextReason) {
		return {
			ok: false,
			reason: modelContextReason
		};
	}

	let serialized: string | undefined;
	try {
		serialized = JSON.stringify(castResponse);
	} catch {
		return {
			ok: false,
			reason: 'presentation must be JSON serializable'
		};
	}
	if (typeof serialized !== 'string' || new TextEncoder().encode(serialized).length > MAX_PRESENTATION_BYTES) {
		return {
			ok: false,
			reason: 'presentation exceeds 64 KiB'
		};
	}

	return { ok: true };
}

export function isStructuredResponseV1(value: unknown): value is StructuredResponseV1 {
	return validateStructuredResponse(value).ok;
}

export function safeStructuredResponse(response: StructuredResponseV1): response is StructuredResponseV1 {
	return validateStructuredResponse(response).ok;
}

export function typedBlocksFromSchema(
	response: StructuredResponseV1
): StructuredResponseBlockV1[] {
	if (response.blocks.length === 0) {
		return [];
	}
	return response.blocks;
}

export function typedActionsFromSchema(
	response: StructuredResponseV1
): StructuredResponseActionV1[] {
	return response.actions ?? [];
}
