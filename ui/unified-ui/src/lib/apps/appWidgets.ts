import { launchAppAction, type AppActionLaunch } from './appDirectory';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

const WIDGET_SCHEMA_VERSION = 1;
const MAX_WIDGET_TARGETS = 12;
const MAX_WIDGET_RESPONSE_BYTES = 1024 * 1024;
const MAX_INDICATOR_RESPONSE_BYTES = 256 * 1024;
const MAX_SLOT_RESPONSE_BYTES = 64 * 1024;
// A legal 100-item picker page may carry sixteen bounded slot suggestions per
// widget. Keep the transport finite without rejecting that server maximum.
const MAX_SLOT_SETTINGS_RESPONSE_BYTES = 2 * 1024 * 1024;
const MAX_SLOT_ASSIGNMENTS = 128;
const MAX_SLOT_PICKER_ITEMS = 100;
const MAX_SLOT_CURSOR_BYTES = 640;
const MAX_ROWS = 32;
const MAX_WIDGET_FIELDS = 32;
const MAX_JSON_CONTAINER_MEMBERS = 256;
const MAX_JSON_NODES = 2048;
const MAX_JSON_DEPTH = 24;
// Widget-sized, not page-sized. The mini-frame host reads this same ceiling, so
// the transport bound and the host bound cannot drift apart (gate S4).
export const APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX = 480;
const MAX_MINI_FRAME_ENTRY_POINT_SEGMENTS = 16;

export type AppWidgetClientCapability =
	| 'detail_v1'
	| 'list_v1'
	| 'table_v1'
	| 'timeline_v1'
	| 'tree_v1'
	| 'graph_v1'
	| 'governed_actions_v1';

export const WEB_APP_WIDGET_CAPABILITIES: readonly AppWidgetClientCapability[] = [
	'detail_v1',
	'list_v1',
	'table_v1',
	'timeline_v1',
	'tree_v1',
	'graph_v1',
	'governed_actions_v1'
];

export interface AppWidgetRenderTarget {
	installation_id: string;
	widget_id: string;
}

export interface AppWidgetRenderRow {
	entity: string;
	record_id: string;
	record_revision: number;
	fields: Record<string, unknown>;
}

export interface AppWidgetRenderHints {
	display_field?: string;
	partition_field?: string;
	parent_field?: string;
	order_field?: string;
	status_field?: string;
	timestamp_field?: string;
	action_field?: string;
	actor_field?: string;
	type_field?: string;
	target_field?: string;
}

export interface AppWidgetGovernedAction {
	action_id: string;
	label: string;
}

export type AppWidgetNativeModel =
	| { model: 'detail'; row: AppWidgetRenderRow | null; hints: AppWidgetRenderHints; actions: AppWidgetGovernedAction[] }
	| { model: 'list' | 'timeline' | 'tree' | 'graph'; rows: AppWidgetRenderRow[]; hints: AppWidgetRenderHints; actions: AppWidgetGovernedAction[] }
	| { model: 'table'; columns: string[]; rows: AppWidgetRenderRow[]; hints: AppWidgetRenderHints; actions: AppWidgetGovernedAction[] };

/**
 * A widget's declared escalation to a sandboxed mini-frame (gate S4). It rides
 * *beside* a complete native model, never instead of one: the runtime compiles
 * the exact same-view native fallback either way, so a client that refuses the
 * frame — for budget, for admission, or because it hosts none at all — still
 * renders the widget's real content.
 *
 * Carrying it here is transport only. The frame itself needs a separately
 * host-minted plan; a declaration alone runs nothing.
 */
export interface AppWidgetMiniFrameDeclaration {
	entry_point: string;
	max_height_px: number;
}

export type AppWidgetUnsupportedFallback =
	| { kind: 'hide' }
	| { kind: 'message'; title: string; body: string };

export type AppWidgetRenderItem = {
	installation_id: string;
	widget_id: string;
	title?: string;
	installation_generation: number | null;
	revision: string;
	rendered_at: string;
	refresh_after: string;
} & (
	| { state: 'ready'; model: AppWidgetNativeModel; mini_frame?: AppWidgetMiniFrameDeclaration }
	| { state: 'unsupported'; fallback: AppWidgetUnsupportedFallback }
	| { state: 'unavailable' }
);

export interface AppWidgetRenderBatchResponse {
	schema_version: 1;
	revision: string;
	etag: string;
	rendered_at: string;
	refresh_after: string;
	widgets: AppWidgetRenderItem[];
}

export type AppIndicatorRenderModel =
	| { kind: 'chip'; text: string }
	| { kind: 'badge'; count: number }
	| { kind: 'state'; label: string };

export interface AppMaterializedIndicator {
	installation_id: string;
	installation_generation: number;
	indicator_id: string;
	title: string;
	revision: string;
	evaluated_at: string;
	expires_at: string;
	model: AppIndicatorRenderModel;
}

export interface AppIndicatorListResponse {
	schema_version: 1;
	revision: string;
	etag: string;
	generated_at: string;
	indicators: AppMaterializedIndicator[];
}

export interface AppSlotPackageBinding {
	installation_id: string;
	package_id: string;
	package_revision_ref: string;
	package_content_digest: string;
	installation_generation: number;
}

export interface AppSlotWidgetBinding {
	package: AppSlotPackageBinding;
	widget_id: string;
}

export interface AppResolvedSlotAssignment {
	slot_id: string;
	source?: 'user' | 'workspace_default';
	pinned_system_default: boolean;
	opted_out: boolean;
	widget?: {
		pinned: AppSlotWidgetBinding;
		current: AppSlotWidgetBinding;
		restored_across_generation: boolean;
		assignment_compatibility: 'exact_digest_only';
	};
	hidden_reason?:
		| 'package_unavailable'
		| 'disabled'
		| 'quarantined'
		| 'update_pending'
		| 'package_identity_changed'
		| 'package_digest_changed'
		| 'generation_rollback'
		| 'widget_no_longer_declared';
}

export interface AppSlotSuggestion {
	page: string;
	region: string;
	slot_id: string;
	system_default: boolean;
}

export interface AppSlotPickerCandidate {
	widget: AppSlotWidgetBinding;
	title: string;
	suggested_slots: AppSlotSuggestion[];
	system_class: boolean;
}

export interface AppSlotWriteHead {
	revision: number;
	fence: number;
}

export interface AppSlotSettingsPage {
	head: AppSlotWriteHead;
	inventory_revision: string;
	assignments: AppResolvedSlotAssignment[];
	next_assignment_cursor?: string;
	assignments_truncated: boolean;
	picker: AppSlotPickerCandidate[];
	next_picker_cursor?: string;
	picker_truncated: boolean;
}

export type AppSlotAssignmentCommand =
	| {
		command: 'assign';
		slot_id: string;
		installation_id: string;
		widget_id: string;
		expected_candidate: AppSlotWidgetBinding;
	}
	| { command: 'opt_out'; slot_id: string }
	| { command: 'restore_workspace_default'; slot_id: string };

export interface AppSlotAssignmentWriteRequest {
	expected_revision: number;
	write_fence: number;
	mutation_id: string;
	command: AppSlotAssignmentCommand;
}

export interface AppSlotAssignmentMutationReceipt {
	mutation_id: string;
	head: AppSlotWriteHead;
	assignment: AppResolvedSlotAssignment;
}

export interface AppSlotSettingsQuery {
	assignmentLimit: number;
	pickerLimit: number;
	assignmentCursor?: string;
	pickerCursor?: string;
}

export class AppSlotTransportError extends Error {
	constructor(message: string, readonly status: number) {
		super(message);
		this.name = 'AppSlotTransportError';
	}
}

export interface AppResponseCache<T> {
	etag: string;
	value: T;
	/** Transport freshness can advance on 304 while the retained body stays unchanged. */
	refreshAfter?: string;
}

type JsonRecord = Record<string, unknown>;

function record(value: unknown): JsonRecord | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? value as JsonRecord
		: null;
}

function exact(value: JsonRecord, required: readonly string[], optional: readonly string[] = []): boolean {
	const accepted = new Set([...required, ...optional]);
	return required.every((key) => key in value) && Object.keys(value).every((key) => accepted.has(key));
}

function bytes(value: string): number {
	return new TextEncoder().encode(value).byteLength;
}

function text(value: unknown, maximumBytes: number, allowEmpty = false): value is string {
	return typeof value === 'string' && (allowEmpty || value.length > 0) && bytes(value) <= maximumBytes;
}

function name(value: unknown): value is string {
	return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
}

function opaqueId(value: unknown): value is string {
	return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(value);
}

function reference(value: unknown): value is string {
	return typeof value === 'string' && value.length <= 192 && /^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/.test(value);
}

function digest(value: unknown): value is string {
	return typeof value === 'string' && /^blake3:[0-9a-f]{64}$/.test(value);
}

function fieldPath(value: unknown): value is string {
	return typeof value === 'string' && bytes(value) <= 256 && value.split('.').length <= 16 &&
		value.split('.').every(name);
}

function positiveInteger(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value > 0;
}

function nonnegativeInteger(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function timestamp(value: unknown): value is string {
	return typeof value === 'string' && value.length <= 64 && Number.isFinite(Date.parse(value));
}

function sameTimestamp(left: string | null, right: string): boolean {
	return left !== null && Date.parse(left) === Date.parse(right);
}

function boundedJson(value: unknown): boolean {
	const pending: Array<{ value: unknown; depth: number }> = [{ value, depth: 0 }];
	let nodes = 0;
	while (pending.length > 0) {
		const current = pending.pop()!;
		nodes += 1;
		if (nodes > MAX_JSON_NODES || current.depth > MAX_JSON_DEPTH) return false;
		if (current.value === null || typeof current.value === 'boolean') continue;
		if (typeof current.value === 'number') {
			if (!Number.isFinite(current.value)) return false;
			continue;
		}
		if (typeof current.value === 'string') {
			if (bytes(current.value) > 64 * 1024) return false;
			continue;
		}
		if (Array.isArray(current.value)) {
			if (current.value.length > MAX_JSON_CONTAINER_MEMBERS) return false;
			for (const item of current.value) pending.push({ value: item, depth: current.depth + 1 });
			continue;
		}
		const object = record(current.value);
		if (!object || Object.keys(object).length > MAX_JSON_CONTAINER_MEMBERS) return false;
		for (const [key, item] of Object.entries(object)) {
			if (bytes(key) > 256) return false;
			pending.push({ value: item, depth: current.depth + 1 });
		}
	}
	return true;
}

async function boundedResponseJson(response: Response, maximumBytes: number): Promise<unknown> {
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const size = Number(declared);
		if (!Number.isSafeInteger(size) || size < 0 || size > maximumBytes) {
			throw new Error('The Apps response exceeded its size limit.');
		}
	}
	if (!response.body) return null;
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let size = 0;
	while (true) {
		const chunk = await reader.read();
		if (chunk.done) break;
		size += chunk.value.byteLength;
		if (size > maximumBytes) {
			try { await reader.cancel(); } catch { /* The bounded rejection is authoritative. */ }
			throw new Error('The Apps response exceeded its size limit.');
		}
		chunks.push(chunk.value);
	}
	const joined = new Uint8Array(size);
	let offset = 0;
	for (const chunk of chunks) {
		joined.set(chunk, offset);
		offset += chunk.byteLength;
	}
	const source = new TextDecoder('utf-8', { fatal: true }).decode(joined);
	if (source.length === 0) return null;
	try { return JSON.parse(source); } catch { throw new Error('The Apps response was not valid JSON.'); }
}

function parseHints(value: unknown): AppWidgetRenderHints | null {
	const hints = record(value);
	const keys = [
		'display_field', 'partition_field', 'parent_field', 'order_field', 'status_field',
		'timestamp_field', 'action_field', 'actor_field', 'type_field', 'target_field'
	] as const;
	if (!hints || !exact(hints, [], keys) || keys.some((key) => hints[key] !== undefined && !fieldPath(hints[key]))) return null;
	return hints as AppWidgetRenderHints;
}

function parseActions(value: unknown): AppWidgetGovernedAction[] | null {
	if (!Array.isArray(value) || value.length > 8) return null;
	const actions: AppWidgetGovernedAction[] = [];
	for (const valueItem of value) {
		const item = record(valueItem);
		if (!item || !exact(item, ['action_id', 'label']) || !name(item.action_id) || !text(item.label, 64)) return null;
		actions.push({ action_id: item.action_id, label: item.label });
	}
	return new Set(actions.map((action) => action.action_id)).size === actions.length ? actions : null;
}

function parseRow(value: unknown): AppWidgetRenderRow | null {
	const row = record(value);
	const fields = row ? record(row.fields) : null;
	if (!row || !exact(row, ['entity', 'record_id', 'record_revision', 'fields']) || !name(row.entity) ||
		!opaqueId(row.record_id) || !positiveInteger(row.record_revision) || !fields ||
		Object.keys(fields).length > MAX_WIDGET_FIELDS || Object.entries(fields).some(([key, item]) => !fieldPath(key) || !boundedJson(item))) return null;
	return { entity: row.entity, record_id: row.record_id, record_revision: row.record_revision, fields };
}

function parseRows(value: unknown): AppWidgetRenderRow[] | null {
	if (!Array.isArray(value) || value.length > MAX_ROWS) return null;
	const rows = value.map(parseRow);
	if (rows.some((row) => row === null)) return null;
	const parsed = rows as AppWidgetRenderRow[];
	return new Set(parsed.map((row) => `${row.entity}\u0000${row.record_id}`)).size === parsed.length ? parsed : null;
}

function parseNativeModel(value: unknown): AppWidgetNativeModel | null {
	const model = record(value);
	if (!model || typeof model.model !== 'string') return null;
	const hints = parseHints(model.hints);
	const actions = parseActions(model.actions);
	if (!hints || !actions) return null;
	if (model.model === 'detail') {
		if (!exact(model, ['model', 'row', 'hints', 'actions'])) return null;
		const row = model.row === null ? null : parseRow(model.row);
		return model.row !== null && !row ? null : { model: 'detail', row, hints, actions };
	}
	if (['list', 'timeline', 'tree', 'graph'].includes(model.model)) {
		if (!exact(model, ['model', 'rows', 'hints', 'actions'])) return null;
		const rows = parseRows(model.rows);
		return rows ? { model: model.model as 'list' | 'timeline' | 'tree' | 'graph', rows, hints, actions } : null;
	}
	if (model.model === 'table') {
		if (!exact(model, ['model', 'columns', 'rows', 'hints', 'actions']) || !Array.isArray(model.columns) ||
		model.columns.length > MAX_WIDGET_FIELDS || model.columns.some((column) => !fieldPath(column))) return null;
		const columns = model.columns as string[];
		const rows = parseRows(model.rows);
		if (!rows || new Set(columns).size !== columns.length) return null;
		return { model: 'table', columns, rows, hints, actions };
	}
	return null;
}

/**
 * `undefined` when the host declared no frame, `null` when it declared one this
 * client refuses. The two are kept apart because refusing a malformed
 * declaration must fail the whole item, while its absence is the ordinary case
 * for every widget and every server that mints no mini-frames.
 */
function parseMiniFrameDeclaration(value: unknown): AppWidgetMiniFrameDeclaration | null | undefined {
	if (value === undefined) return undefined;
	const declaration = record(value);
	if (!declaration || !exact(declaration, ['entry_point', 'max_height_px'])) return null;
	const entryPoint = declaration.entry_point;
	const maxHeight = declaration.max_height_px;
	if (typeof entryPoint !== 'string' || bytes(entryPoint) > 512 || !entryPoint.startsWith('/') ||
		/[\\?#%:]/.test(entryPoint)) return null;
	const segments = entryPoint.slice(1).split('/');
	if (segments.length === 0 || segments.length > MAX_MINI_FRAME_ENTRY_POINT_SEGMENTS ||
		segments.some((segment) => segment.length === 0 || segment.length > 128 ||
			segment === '.' || segment === '..' || !/^[A-Za-z0-9_.-]+$/.test(segment))) return null;
	if (!positiveInteger(maxHeight) || maxHeight > APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX) return null;
	return { entry_point: entryPoint, max_height_px: maxHeight };
}

function parseWidgetItem(value: unknown): AppWidgetRenderItem | null {
	const item = record(value);
	if (!item || !opaqueId(item.installation_id) || !name(item.widget_id) ||
		(item.title !== undefined && !text(item.title, 256)) ||
		(item.installation_generation !== null && !positiveInteger(item.installation_generation)) ||
		!digest(item.revision) || !timestamp(item.rendered_at) || !timestamp(item.refresh_after) ||
		Date.parse(item.refresh_after) - Date.parse(item.rendered_at) < 5_000 ||
		Date.parse(item.refresh_after) - Date.parse(item.rendered_at) > 24 * 60 * 60 * 1_000) return null;
	const base = [
		'installation_id', 'widget_id', 'installation_generation', 'revision', 'rendered_at',
		'refresh_after', 'state'
	];
	const title = item.title as string | undefined;
	const common = {
		installation_id: item.installation_id,
		widget_id: item.widget_id,
		...(title !== undefined ? { title } : {}),
		installation_generation: item.installation_generation as number | null,
		revision: item.revision,
		rendered_at: item.rendered_at,
		refresh_after: item.refresh_after
	};
	if (item.state === 'ready') {
		if (!exact(item, [...base, 'model'], ['title', 'mini_frame']) ||
			!positiveInteger(item.installation_generation)) return null;
		const model = parseNativeModel(item.model);
		const miniFrame = parseMiniFrameDeclaration(item.mini_frame);
		if (!model || miniFrame === null) return null;
		return {
			...common,
			state: 'ready',
			model,
			...(miniFrame === undefined ? {} : { mini_frame: miniFrame })
		};
	}
	if (item.state === 'unsupported') {
		if (!exact(item, [...base, 'fallback'], ['title'])) return null;
		const fallback = record(item.fallback);
		if (!fallback || typeof fallback.kind !== 'string') return null;
		if (fallback.kind === 'hide' && exact(fallback, ['kind'])) return { ...common, state: 'unsupported', fallback: { kind: 'hide' } };
		if (fallback.kind === 'message' && exact(fallback, ['kind', 'title', 'body']) && text(fallback.title, 256) && text(fallback.body, 256)) {
			return { ...common, state: 'unsupported', fallback: { kind: 'message', title: fallback.title, body: fallback.body } };
		}
		return null;
	}
	return item.state === 'unavailable' && exact(item, base, ['title'])
		? { ...common, state: 'unavailable' }
		: null;
}

export function parseAppWidgetRenderBatch(value: unknown): AppWidgetRenderBatchResponse {
	const page = record(value);
	if (!page || !exact(page, ['schema_version', 'revision', 'etag', 'rendered_at', 'refresh_after', 'widgets']) ||
		page.schema_version !== WIDGET_SCHEMA_VERSION || !digest(page.revision) || page.etag !== page.revision ||
		!timestamp(page.rendered_at) || !timestamp(page.refresh_after) ||
		Date.parse(page.refresh_after) - Date.parse(page.rendered_at) < 5_000 ||
		Date.parse(page.refresh_after) - Date.parse(page.rendered_at) > 24 * 60 * 60 * 1_000 ||
		!Array.isArray(page.widgets) || page.widgets.length > MAX_WIDGET_TARGETS) {
		throw new Error('The Apps widget renderer returned an invalid response.');
	}
	const widgets = page.widgets.map(parseWidgetItem);
	if (widgets.some((item) => item === null)) throw new Error('The Apps widget renderer returned an invalid response.');
	const parsed = widgets as AppWidgetRenderItem[];
	if (new Set(parsed.map((item) => `${item.installation_id}\u0000${item.widget_id}`)).size !== parsed.length) {
		throw new Error('The Apps widget renderer returned duplicate widgets.');
	}
	if (parsed.length > 0 && page.refresh_after !== parsed.reduce(
		(earliest, item) => Date.parse(item.refresh_after) < Date.parse(earliest) ? item.refresh_after : earliest,
		parsed[0].refresh_after
	)) throw new Error('The Apps widget renderer returned an inconsistent refresh deadline.');
	return {
		schema_version: 1, revision: page.revision, etag: page.etag,
		rendered_at: page.rendered_at, refresh_after: page.refresh_after, widgets: parsed
	};
}

function parseIndicator(value: unknown): AppMaterializedIndicator | null {
	const item = record(value);
	const model = item ? record(item.model) : null;
	if (!item || !exact(item, [
		'installation_id', 'installation_generation', 'indicator_id', 'title', 'revision',
		'evaluated_at', 'expires_at', 'model'
	]) || !opaqueId(item.installation_id) || !positiveInteger(item.installation_generation) ||
		!name(item.indicator_id) || !text(item.title, 256) || !digest(item.revision) ||
		!timestamp(item.evaluated_at) || !timestamp(item.expires_at) || Date.parse(item.expires_at) <= Date.parse(item.evaluated_at) || !model) return null;
	let parsedModel: AppIndicatorRenderModel | null = null;
	if (model.kind === 'chip' && exact(model, ['kind', 'text']) && text(model.text, 256)) parsedModel = { kind: 'chip', text: model.text };
	if (model.kind === 'state' && exact(model, ['kind', 'label']) && text(model.label, 256)) parsedModel = { kind: 'state', label: model.label };
	if (model.kind === 'badge' && exact(model, ['kind', 'count']) && typeof model.count === 'number' && Number.isSafeInteger(model.count) && model.count >= 1 && model.count <= 9_999) parsedModel = { kind: 'badge', count: model.count };
	return parsedModel ? {
		installation_id: item.installation_id,
		installation_generation: item.installation_generation,
		indicator_id: item.indicator_id,
		title: item.title,
		revision: item.revision,
		evaluated_at: item.evaluated_at,
		expires_at: item.expires_at,
		model: parsedModel
	} : null;
}

export function parseAppIndicatorList(value: unknown): AppIndicatorListResponse {
	const page = record(value);
	if (!page || !exact(page, ['schema_version', 'revision', 'etag', 'generated_at', 'indicators']) ||
		page.schema_version !== 1 || !digest(page.revision) || page.etag !== page.revision || !timestamp(page.generated_at) ||
		!Array.isArray(page.indicators) || page.indicators.length > 32) throw new Error('The Apps indicators response was invalid.');
	const indicators = page.indicators.map(parseIndicator);
	if (indicators.some((item) => item === null)) throw new Error('The Apps indicators response was invalid.');
	const parsed = indicators as AppMaterializedIndicator[];
	if (new Set(parsed.map((item) => `${item.installation_id}\u0000${item.indicator_id}`)).size !== parsed.length) {
		throw new Error('The Apps indicators response contained duplicates.');
	}
	return { schema_version: 1, revision: page.revision, etag: page.etag, generated_at: page.generated_at, indicators: parsed };
}

function parsePackageBinding(value: unknown): AppSlotPackageBinding | null {
	const binding = record(value);
	return binding && exact(binding, ['installation_id', 'package_id', 'package_revision_ref', 'package_content_digest', 'installation_generation']) &&
		opaqueId(binding.installation_id) && reference(binding.package_id) && reference(binding.package_revision_ref) &&
		digest(binding.package_content_digest) && positiveInteger(binding.installation_generation)
		? binding as unknown as AppSlotPackageBinding : null;
}

function parseWidgetBinding(value: unknown): AppSlotWidgetBinding | null {
	const binding = record(value);
	const packageBinding = binding ? parsePackageBinding(binding.package) : null;
	return binding && exact(binding, ['package', 'widget_id']) && packageBinding && name(binding.widget_id)
		? { package: packageBinding, widget_id: binding.widget_id } : null;
}

function sameWidgetBinding(left: AppSlotWidgetBinding, right: AppSlotWidgetBinding): boolean {
	return left.widget_id === right.widget_id &&
		left.package.installation_id === right.package.installation_id &&
		left.package.package_id === right.package.package_id &&
		left.package.package_revision_ref === right.package.package_revision_ref &&
		left.package.package_content_digest === right.package.package_content_digest &&
		left.package.installation_generation === right.package.installation_generation;
}

export function parseAppResolvedSlot(value: unknown, expectedSlotId: string): AppResolvedSlotAssignment {
	const item = record(value);
	const optional = ['source', 'widget', 'hidden_reason'];
	if (!item || !exact(item, ['slot_id', 'pinned_system_default', 'opted_out'], optional) || item.slot_id !== expectedSlotId ||
		typeof item.pinned_system_default !== 'boolean' || typeof item.opted_out !== 'boolean' ||
		(item.source !== undefined && !['user', 'workspace_default'].includes(String(item.source))) ||
		(item.hidden_reason !== undefined && ![
			'package_unavailable', 'disabled', 'quarantined', 'update_pending', 'package_identity_changed',
			'package_digest_changed', 'generation_rollback', 'widget_no_longer_declared'
		].includes(String(item.hidden_reason)))) throw new Error('The Apps slot response was invalid.');
	let widget: AppResolvedSlotAssignment['widget'];
	if (item.widget !== undefined) {
		const valueWidget = record(item.widget);
		const pinned = valueWidget ? parseWidgetBinding(valueWidget.pinned) : null;
		const current = valueWidget ? parseWidgetBinding(valueWidget.current) : null;
		if (!valueWidget || !exact(valueWidget, ['pinned', 'current', 'restored_across_generation', 'assignment_compatibility']) ||
			!pinned || !current || typeof valueWidget.restored_across_generation !== 'boolean' ||
			valueWidget.assignment_compatibility !== 'exact_digest_only' ||
			pinned.package.installation_id !== current.package.installation_id ||
			pinned.package.package_id !== current.package.package_id ||
			pinned.package.package_content_digest !== current.package.package_content_digest ||
			pinned.widget_id !== current.widget_id ||
			current.package.installation_generation < pinned.package.installation_generation ||
			valueWidget.restored_across_generation !==
				(current.package.installation_generation > pinned.package.installation_generation)) {
			throw new Error('The Apps slot response was invalid.');
		}
		widget = { pinned, current, restored_across_generation: valueWidget.restored_across_generation, assignment_compatibility: 'exact_digest_only' };
	}
	const hasSource = item.source !== undefined;
	const hasHiddenReason = item.hidden_reason !== undefined;
	if ((widget !== undefined && (hasHiddenReason || item.opted_out || !hasSource)) ||
		(hasHiddenReason && (widget !== undefined || item.opted_out || !hasSource)) ||
		(item.opted_out && (widget !== undefined || hasHiddenReason || hasSource)) ||
		(!widget && !hasHiddenReason && !item.opted_out && hasSource)) {
		throw new Error('The Apps slot response was contradictory.');
	}
	return {
		slot_id: expectedSlotId,
		...(item.source !== undefined ? { source: item.source as 'user' | 'workspace_default' } : {}),
		pinned_system_default: item.pinned_system_default,
		opted_out: item.opted_out,
		...(widget ? { widget } : {}),
		...(item.hidden_reason !== undefined ? { hidden_reason: item.hidden_reason as AppResolvedSlotAssignment['hidden_reason'] } : {})
	};
}

export function appPageSlotId(page: string, region: string): string {
	if (!page || page.length > 256 || !page.startsWith('/') || !/^[\x20-\x7e]+$/.test(page) || /[\\?#%:]/.test(page)) {
		throw new Error('The Apps slot page must be a bounded static route.');
	}
	if (page !== '/') {
		const segments = page.slice(1).split('/');
		if (segments.length > 16 || segments.some((segment) => !segment || segment === '.' || segment === '..' || !/^[A-Za-z0-9_.-]+$/.test(segment))) {
			throw new Error('The Apps slot page must be a bounded static route.');
		}
	}
	if (!name(region)) throw new Error('The Apps slot region is invalid.');
	const encoded = [...new TextEncoder().encode(page)].map((value) => value.toString(16).padStart(2, '0')).join('');
	return `page:${encoded}:${region}`;
}

export function appSurfaceSlotPage(installationId: string, surfacePath: string): string | null {
	if (!opaqueId(installationId)) return null;
	const page = `/apps/${installationId}${surfacePath ? `/${surfacePath}` : ''}`;
	try {
		appPageSlotId(page, 'contextual');
		return page;
	} catch {
		return null;
	}
}

function canonicalSlotId(value: unknown): value is string {
	if (typeof value !== 'string' || value.length > 600) return false;
	const match = /^page:([0-9a-f]+):([A-Za-z0-9][A-Za-z0-9_-]{0,63})$/.exec(value);
	if (!match || match[1].length % 2 !== 0 || match[1].length > 512) return false;
	try {
		const encoded = match[1];
		const pageBytes = new Uint8Array(encoded.length / 2);
		for (let index = 0; index < encoded.length; index += 2) pageBytes[index / 2] = Number.parseInt(encoded.slice(index, index + 2), 16);
		const page = new TextDecoder('utf-8', { fatal: true }).decode(pageBytes);
		return appPageSlotId(page, match[2]) === value;
	} catch {
		return false;
	}
}

function slotCursor(value: unknown): value is string {
	return typeof value === 'string' && bytes(value) >= 1 && bytes(value) <= MAX_SLOT_CURSOR_BYTES && !/[\u0000-\u001f\u007f]/.test(value);
}

function parseSlotWriteHead(value: unknown): AppSlotWriteHead | null {
	const head = record(value);
	return head && exact(head, ['revision', 'fence']) && nonnegativeInteger(head.revision) && positiveInteger(head.fence)
		? { revision: head.revision, fence: head.fence }
		: null;
}

function parseSlotSuggestion(value: unknown): AppSlotSuggestion | null {
	const suggestion = record(value);
	if (!suggestion || !exact(suggestion, ['page', 'region', 'slot_id', 'system_default']) ||
		!text(suggestion.page, 256) || !name(suggestion.region) || !canonicalSlotId(suggestion.slot_id) ||
		typeof suggestion.system_default !== 'boolean') return null;
	try {
		if (appPageSlotId(suggestion.page, suggestion.region) !== suggestion.slot_id) return null;
	} catch {
		return null;
	}
	return {
		page: suggestion.page,
		region: suggestion.region,
		slot_id: suggestion.slot_id,
		system_default: suggestion.system_default
	};
}

function parseSlotPickerCandidate(value: unknown): AppSlotPickerCandidate | null {
	const candidate = record(value);
	const widget = candidate ? parseWidgetBinding(candidate.widget) : null;
	if (!candidate || !exact(candidate, ['widget', 'title', 'suggested_slots', 'system_class']) || !widget ||
		!text(candidate.title, 256) || !Array.isArray(candidate.suggested_slots) || candidate.suggested_slots.length > 16 ||
		typeof candidate.system_class !== 'boolean') return null;
	const suggestions = candidate.suggested_slots.map(parseSlotSuggestion);
	if (suggestions.some((suggestion) => suggestion === null)) return null;
	const parsed = suggestions as AppSlotSuggestion[];
	if (new Set(parsed.map((suggestion) => suggestion.slot_id)).size !== parsed.length ||
		parsed.some((suggestion) => suggestion.system_default && !candidate.system_class)) return null;
	return { widget, title: candidate.title, suggested_slots: parsed, system_class: candidate.system_class };
}

export function parseAppSlotSettingsPage(value: unknown): AppSlotSettingsPage {
	const page = record(value);
	const head = page ? parseSlotWriteHead(page.head) : null;
	if (!page || !exact(page, ['head', 'inventory_revision', 'assignments', 'assignments_truncated', 'picker', 'picker_truncated'],
		['next_assignment_cursor', 'next_picker_cursor']) || !head || !Array.isArray(page.assignments) ||
		!digest(page.inventory_revision) ||
		page.assignments.length > MAX_SLOT_ASSIGNMENTS || typeof page.assignments_truncated !== 'boolean' ||
		!Array.isArray(page.picker) || page.picker.length > MAX_SLOT_PICKER_ITEMS || typeof page.picker_truncated !== 'boolean' ||
		(page.next_assignment_cursor !== undefined && !slotCursor(page.next_assignment_cursor)) ||
		(page.next_picker_cursor !== undefined && !slotCursor(page.next_picker_cursor)) ||
		(page.assignments_truncated !== (page.next_assignment_cursor !== undefined)) ||
		(page.picker_truncated !== (page.next_picker_cursor !== undefined))) {
		throw new Error('The Apps slot settings response was invalid.');
	}
	const assignments = page.assignments.map((assignment) => {
		const candidate = record(assignment);
		if (!candidate || !canonicalSlotId(candidate.slot_id)) throw new Error('The Apps slot settings response was invalid.');
		return parseAppResolvedSlot(assignment, candidate.slot_id);
	});
	const picker = page.picker.map(parseSlotPickerCandidate);
	if (picker.some((candidate) => candidate === null)) throw new Error('The Apps slot settings response was invalid.');
	const parsedPicker = picker as AppSlotPickerCandidate[];
	if (new Set(assignments.map((assignment) => assignment.slot_id)).size !== assignments.length ||
		new Set(parsedPicker.map((candidate) => `${candidate.widget.package.installation_id}\u0000${candidate.widget.widget_id}`)).size !== parsedPicker.length) {
		throw new Error('The Apps slot settings response contained duplicates.');
	}
	return {
		head,
		inventory_revision: page.inventory_revision,
		assignments,
		...(page.next_assignment_cursor !== undefined ? { next_assignment_cursor: page.next_assignment_cursor } : {}),
		assignments_truncated: page.assignments_truncated,
		picker: parsedPicker,
		...(page.next_picker_cursor !== undefined ? { next_picker_cursor: page.next_picker_cursor } : {}),
		picker_truncated: page.picker_truncated
	};
}

function validateSlotCommand(command: AppSlotAssignmentCommand): boolean {
	if (!canonicalSlotId(command.slot_id)) return false;
	if (command.command === 'assign') {
		const candidate = parseWidgetBinding(command.expected_candidate);
		return opaqueId(command.installation_id) && name(command.widget_id) && candidate !== null &&
			candidate.package.installation_id === command.installation_id && candidate.widget_id === command.widget_id;
	}
	return command.command === 'opt_out' || command.command === 'restore_workspace_default';
}

function parseSlotMutationReceipt(value: unknown, request: AppSlotAssignmentWriteRequest): AppSlotAssignmentMutationReceipt {
	const receipt = record(value);
	const head = receipt ? parseSlotWriteHead(receipt.head) : null;
	if (!receipt || !exact(receipt, ['mutation_id', 'head', 'assignment']) || receipt.mutation_id !== request.mutation_id || !head) {
		throw new Error('The Apps slot mutation receipt was invalid.');
	}
	const assignment = parseAppResolvedSlot(receipt.assignment, request.command.slot_id);
	if (request.expected_revision >= Number.MAX_SAFE_INTEGER ||
		head.revision !== request.expected_revision + 1 || head.fence !== request.write_fence) {
		throw new Error('The Apps slot mutation receipt was stale.');
	}
	const current = assignment.widget?.current;
	const pinned = assignment.widget?.pinned;
	if ((request.command.command === 'assign' && (
		assignment.source !== 'user' || assignment.opted_out || !current || !pinned ||
		current.package.installation_id !== request.command.installation_id ||
		current.widget_id !== request.command.widget_id ||
		!sameWidgetBinding(current, request.command.expected_candidate) ||
		!sameWidgetBinding(pinned, request.command.expected_candidate) ||
		assignment.widget?.restored_across_generation
	)) || (request.command.command === 'opt_out' && (!assignment.opted_out || assignment.widget !== undefined || assignment.source !== undefined)) ||
		(request.command.command === 'restore_workspace_default' && (assignment.opted_out || assignment.source === 'user'))) {
		throw new Error('The Apps slot mutation receipt did not match its command.');
	}
	return { mutation_id: request.mutation_id, head, assignment };
}

export function newAppSlotMutationId(): string {
	return `slot-mutation:${crypto.randomUUID()}`;
}

function etagHeader(cache?: AppResponseCache<unknown>): Record<string, string> {
	return cache && digest(cache.etag) ? { 'If-None-Match': `"${cache.etag}"` } : {};
}

function responseEtag(response: Response): string | null {
	const raw = response.headers.get('etag');
	if (!raw) return null;
	// This endpoint mints one strong, quoted digest ETag. Do not repair weak,
	// unquoted, or mismatched-quote variants into something that looks valid.
	const match = /^"(blake3:[0-9a-f]{64})"$/.exec(raw.trim());
	return match && digest(match[1]) ? match[1] : null;
}

function widgetRefreshHeader(response: Response): string | null {
	const value = response.headers.get('x-app-widget-refresh-after');
	return timestamp(value) ? value : null;
}

export async function resolveAppSlot(page: string, region: string, signal?: AbortSignal): Promise<AppResolvedSlotAssignment> {
	const slotId = appPageSlotId(page, region);
	const response = await fetch(`/api/magician/v2/apps/slots/${encodeURIComponent(slotId)}`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedResponseJson(response, MAX_SLOT_RESPONSE_BYTES);
	if (!response.ok) throw new Error('The Apps slot could not be resolved.');
	return parseAppResolvedSlot(body, slotId);
}

export async function resolveAppSlots(
	page: string,
	regions: readonly string[],
	signal?: AbortSignal
): Promise<AppResolvedSlotAssignment[]> {
	if (regions.length < 1 || regions.length > MAX_WIDGET_TARGETS || new Set(regions).size !== regions.length) {
		throw new Error('The Apps slot resolution batch was invalid.');
	}
	const slotIds = regions.map((region) => appPageSlotId(page, region));
	const response = await fetch('/api/magician/v2/apps/slots/resolve-batch', {
		method: 'POST',
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		body: JSON.stringify({ slot_ids: slotIds }), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedResponseJson(response, 128 * 1024);
	if (!response.ok) throw new Error('The Apps slots could not be resolved.');
	const batch = record(body);
	if (!batch || !exact(batch, ['assignments']) || !Array.isArray(batch.assignments) || batch.assignments.length !== slotIds.length) {
		throw new Error('The Apps slot resolution batch response was invalid.');
	}
	return batch.assignments.map((assignment, index) => parseAppResolvedSlot(assignment, slotIds[index]));
}

export async function fetchAppSlotSettings(
	query: AppSlotSettingsQuery,
	signal?: AbortSignal
): Promise<AppSlotSettingsPage> {
	if (!positiveInteger(query.assignmentLimit) || query.assignmentLimit > MAX_SLOT_ASSIGNMENTS ||
		!positiveInteger(query.pickerLimit) || query.pickerLimit > MAX_SLOT_PICKER_ITEMS ||
		(query.assignmentCursor !== undefined && !slotCursor(query.assignmentCursor)) ||
		(query.pickerCursor !== undefined && !slotCursor(query.pickerCursor))) {
		throw new Error('The Apps slot settings query was invalid.');
	}
	const parameters = new URLSearchParams({
		assignment_limit: String(query.assignmentLimit),
		picker_limit: String(query.pickerLimit)
	});
	if (query.assignmentCursor !== undefined) parameters.set('assignment_cursor', query.assignmentCursor);
	if (query.pickerCursor !== undefined) parameters.set('picker_cursor', query.pickerCursor);
	const response = await fetch(`/api/magician/v2/apps/slot-assignments?${parameters.toString()}`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedResponseJson(response, MAX_SLOT_SETTINGS_RESPONSE_BYTES);
	if (!response.ok) throw new AppSlotTransportError('The Apps slot settings could not be loaded.', response.status);
	const page = parseAppSlotSettingsPage(body);
	if (page.assignments.length > query.assignmentLimit || page.picker.length > query.pickerLimit) {
		throw new Error('The Apps slot settings response exceeded its requested page bounds.');
	}
	return page;
}

export async function mutateAppSlotAssignment(
	request: AppSlotAssignmentWriteRequest,
	signal?: AbortSignal
): Promise<AppSlotAssignmentMutationReceipt> {
	if (!nonnegativeInteger(request.expected_revision) || !positiveInteger(request.write_fence) ||
		!reference(request.mutation_id) || !request.mutation_id.startsWith('slot-mutation:') || !validateSlotCommand(request.command)) {
		throw new Error('The Apps slot mutation request was invalid.');
	}
	const response = await fetch('/api/magician/v2/apps/slot-assignments', {
		method: 'POST',
		headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
		body: JSON.stringify(request), cache: 'no-store', redirect: 'error', signal
	});
	const body = await boundedResponseJson(response, MAX_SLOT_RESPONSE_BYTES);
	if (!response.ok) throw new AppSlotTransportError('The Apps slot change could not be applied.', response.status);
	return parseSlotMutationReceipt(body, request);
}

/**
 * The shortest wait before the next widget revalidation. A batch deadline that
 * is missing or already behind the local clock is accepted and pushed out to
 * this floor rather than rejected.
 */
export const APP_WIDGET_REFRESH_FLOOR_MS = 2_000;

/**
 * How long a rendered widget stays on screen past its own deadline while a
 * refresh is in flight or failing transiently (stale-while-revalidate). The
 * render payload carries no per-widget staleness bound, so this is the cap.
 */
export const APP_WIDGET_MAX_STALENESS_MS = 5 * 60 * 1_000;

/** The next revalidation deadline: the server's, but never sooner than the floor. */
export function appWidgetRefreshDeadline(deadline: string | null | undefined, now = Date.now()): string {
	const parsed = deadline ? Date.parse(deadline) : Number.NaN;
	const floor = now + APP_WIDGET_REFRESH_FLOOR_MS;
	return Number.isFinite(parsed) && parsed >= floor ? deadline! : new Date(floor).toISOString();
}

/** Whether a rendered widget may still be shown while it is being revalidated. */
export function appWidgetWithinStaleness(widget: Pick<AppWidgetRenderItem, 'refresh_after'>, now = Date.now()): boolean {
	const deadline = Date.parse(widget.refresh_after);
	return Number.isFinite(deadline) && now < deadline + APP_WIDGET_MAX_STALENESS_MS;
}

export async function renderAppWidgetBatch(
	targets: readonly AppWidgetRenderTarget[],
	cache?: AppResponseCache<AppWidgetRenderBatchResponse>,
	signal?: AbortSignal
): Promise<AppResponseCache<AppWidgetRenderBatchResponse>> {
	if (targets.length < 1 || targets.length > MAX_WIDGET_TARGETS || targets.some((target) => !opaqueId(target.installation_id) || !name(target.widget_id)) ||
		new Set(targets.map((target) => `${target.installation_id}\u0000${target.widget_id}`)).size !== targets.length) {
		throw new Error('The Apps widget render targets are invalid.');
	}
	const response = await fetch('/api/magician/v2/apps/widgets/render-batch', {
		method: 'POST',
		headers: scopedRequestHeaders({
			Accept: 'application/json', 'Content-Type': 'application/json', ...etagHeader(cache)
		}),
		body: JSON.stringify({ schema_version: 1, client_capabilities: WEB_APP_WIDGET_CAPABILITIES, widgets: targets }),
		cache: 'no-store', redirect: 'error', signal
	});
	const headerEtag = responseEtag(response);
	const headerRefreshAfter = widgetRefreshHeader(response);
	if (response.status === 304) {
		const requested = new Set(targets.map((target) => `${target.installation_id}\u0000${target.widget_id}`));
		if (!cache || cache.value.etag !== cache.etag || !headerEtag || headerEtag !== cache.etag ||
			cache.value.widgets.length !== targets.length ||
			cache.value.widgets.some((item) => !requested.has(`${item.installation_id}\u0000${item.widget_id}`))) {
			throw new Error('The Apps widget cache response was invalid.');
		}
		// A 304 says every retained item is current until the header deadline, so
		// the per-item deadlines renew with the batch one. A missing, regressed or
		// already-passed deadline (a shared server cache can expire milliseconds
		// after now) is not a failure: the content is still current, and the next
		// revalidation waits out the floor instead of spinning or blanking.
		const renewed = appWidgetRefreshDeadline(headerRefreshAfter);
		return {
			etag: cache.etag,
			value: {
				...cache.value,
				refresh_after: renewed,
				widgets: cache.value.widgets.map((item) => ({ ...item, refresh_after: renewed }))
			},
			refreshAfter: renewed
		};
	}
	const body = await boundedResponseJson(response, MAX_WIDGET_RESPONSE_BYTES);
	if (!response.ok) throw new Error('The Apps widgets could not be rendered.');
	const value = parseAppWidgetRenderBatch(body);
	// The deadline header is advisory transport metadata (and a cross-origin
	// client may not be allowed to read it): absent, the body's deadline rules;
	// present, it must agree with the body.
	if (!headerEtag || headerEtag !== value.etag ||
		(headerRefreshAfter !== null && !sameTimestamp(headerRefreshAfter, value.refresh_after))) {
		throw new Error('The Apps widget cache metadata was invalid.');
	}
	const requested = new Set(targets.map((target) => `${target.installation_id}\u0000${target.widget_id}`));
	if (value.widgets.length !== targets.length || value.widgets.some((item) => !requested.has(`${item.installation_id}\u0000${item.widget_id}`))) {
		throw new Error('The Apps widget response did not match its request.');
	}
	return { etag: value.etag, value, refreshAfter: appWidgetRefreshDeadline(value.refresh_after) };
}

export async function fetchAppIndicators(
	cache?: AppResponseCache<AppIndicatorListResponse>,
	signal?: AbortSignal
): Promise<AppResponseCache<AppIndicatorListResponse>> {
	const response = await fetch('/api/magician/v2/apps/indicators?limit=32', {
		headers: scopedRequestHeaders({ Accept: 'application/json', ...etagHeader(cache) }),
		cache: 'no-store', redirect: 'error', signal
	});
	const headerEtag = responseEtag(response);
	if (response.status === 304) {
		if (!cache || cache.value.etag !== cache.etag || !headerEtag || headerEtag !== cache.etag) {
			throw new Error('The Apps indicator cache response was invalid.');
		}
		return cache;
	}
	const body = await boundedResponseJson(response, MAX_INDICATOR_RESPONSE_BYTES);
	if (!response.ok) throw new Error('The Apps indicators could not be loaded.');
	const value = parseAppIndicatorList(body);
	if (!headerEtag || headerEtag !== value.etag) throw new Error('The Apps indicator ETag was invalid.');
	return { etag: value.etag, value };
}

/** Native widget buttons have no input mapping in V1; the only legal input is exactly `{}`. */
export function launchEmptyInputWidgetAction(
	installationId: string,
	actionId: string,
	idempotencyKey: string,
	expectedInstallationGeneration: number,
	expectedPackageRevisionRef: string,
	signal?: AbortSignal
): Promise<AppActionLaunch> {
	if (!opaqueId(installationId) || !name(actionId) || !reference(idempotencyKey) ||
		!positiveInteger(expectedInstallationGeneration) || !reference(expectedPackageRevisionRef)) {
		throw new Error('The Apps widget action request is invalid.');
	}
	return launchAppAction(installationId, actionId, idempotencyKey, {}, signal, {
		generation: expectedInstallationGeneration,
		package_revision_ref: expectedPackageRevisionRef
	});
}
