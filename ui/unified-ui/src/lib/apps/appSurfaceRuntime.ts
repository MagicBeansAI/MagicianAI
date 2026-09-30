import type { MuijComponent, MuijDocument, MuijInteractionEventDetail } from '$lib/stores/muijStore';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

const MAX_COMPONENTS = 1_024;
const MAX_COMPONENT_DEPTH = 32;
const MAX_RECORDS = 200;
const MAX_FIELDS = 64;
const MAX_ROUTE_BINDINGS = 16;
const APP_TREE_MAX_DEPTH = 3;
const APP_SURFACE_ROW_LIMIT = 25;
const MAX_DECLARATIVE_COMPONENTS = 64;
const MAX_DECLARATIVE_DEPTH = 8;
const MAX_DECLARATIVE_BYTES = 64 * 1024;

export type AppSurfaceFieldKind =
	| 'text'
	| 'markdown'
	| 'integer'
	| 'decimal'
	| 'boolean'
	| 'timestamp'
	| 'enum'
	| 'reference';

export interface AppSurfaceFieldBinding {
	field: string;
	kind: AppSurfaceFieldKind;
	required: boolean;
	nullable: boolean;
	sortable: boolean;
	allowedValues: string[];
	referenceEntity?: string;
}

export interface AppSurfaceRouteBinding {
	parameter: string;
	field: string;
	scalarKind: 'text' | 'integer' | 'decimal' | 'boolean' | 'enum' | 'reference';
	allowedValues: string[];
}

export type AppSurfaceComponent =
	| { kind: 'detail'; id: string; label: string; fields: string[] }
	| { kind: 'form'; id: string; label: string; fields: string[] }
	| { kind: 'section'; id: string; label: string; children: AppSurfaceComponent[] }
	| { kind: 'list'; id: string; label: string; fields: string[] }
	| { kind: 'table'; id: string; label: string; columns: string[] };

export interface AppSurfaceViewBinding {
	entity: string;
	viewKind: 'list' | 'table' | 'tree' | 'timeline';
	fields: string[];
	fieldBindings: AppSurfaceFieldBinding[];
	routeBindings: AppSurfaceRouteBinding[];
	surfaceComponents: AppSurfaceComponent[];
	labelField?: string;
	partitionField?: string;
	parentField?: string;
	orderField?: string;
	statusField?: string;
	maxDepth?: number;
	timestampField?: string;
	actionField?: string;
	actorField?: string;
	typeField?: string;
	targetField?: string;
	defaultActor?: string;
	defaultType?: string;
}

export interface AppSurfaceRecord {
	entity: string;
	record_id: string;
	record_revision: number;
	fields: Record<string, unknown>;
}

export interface AppSurfaceHydration {
	binding: {
		installation_id: string;
		surface_revision: number;
		view_id: string;
		app_local_route: string;
	};
	surface: {
		installation_id: string;
		surface_revision: number;
		view_id: string;
		interaction_mode: 'app';
		muij_document: MuijDocument;
	};
	route_parameters: Record<string, string>;
	change_sequence: number;
	page: {
		envelope: {
			installation_id: string;
			schema_revision: number;
			value: AppSurfaceRecord[];
		};
		next_cursor?: string;
	};
}

export interface AppEntityChange {
	entity: string;
	record_id: string;
	record_revision: number;
	change_sequence: number;
}

export interface AppEntityChangeBatch {
	installation_id: string;
	surface_revision: number;
	after_change_sequence: number;
	through_change_sequence: number;
	current_change_sequence: number;
	changes: AppEntityChange[];
	has_more: boolean;
	reset_required: boolean;
}

export interface AppEntityChangeSignal {
	installation_id: string;
	surface_revision: number;
	first_change_sequence: number;
	last_change_sequence: number;
	changes: AppEntityChange[];
	reset_required: boolean;
}

export type AppChangeSignalDisposition = 'ignore' | 'synchronize' | 'reset';

export function appChangeSignalDisposition(
	currentChangeSequence: number,
	signal: AppEntityChangeSignal
): AppChangeSignalDisposition {
	if (!Number.isSafeInteger(currentChangeSequence) || currentChangeSequence < 0) {
		throw new AppSurfaceClientError('The app change cursor is invalid.', 500, 'invalid_app_changes');
	}
	if (signal.reset_required) return 'reset';
	if (signal.last_change_sequence <= currentChangeSequence) return 'ignore';
	// A contiguous hint and a hint with a gap both enter the same bounded,
	// authoritative HTTP reader. The hint never mutates canonical UI state.
	return 'synchronize';
}

export interface AppSurfaceRenderPage {
	components: MuijComponent[];
	view: AppSurfaceViewBinding;
	records: AppSurfaceRecord[];
	hasNextPage: boolean;
}

export interface AppSurfacePagePosition {
	page: number;
	pageSize: number;
	hasNextPage: boolean;
	sortField?: string;
	sortDirection?: 'ascending' | 'descending';
}

export function resolveAppChangeHead(
	previousHead: number,
	previousSurfaceRevision: number | undefined,
	nextSurfaceRevision: number,
	nextHead: number,
	replaceCanonicalHead: boolean
): number {
	if (
		!Number.isSafeInteger(previousHead) || previousHead < 0
		|| !Number.isSafeInteger(nextHead) || nextHead < 0
		|| !Number.isSafeInteger(nextSurfaceRevision) || nextSurfaceRevision <= 0
	) throw new Error('Invalid app change head');
	return !replaceCanonicalHead && previousSurfaceRevision === nextSurfaceRevision
		? Math.max(previousHead, nextHead)
		: nextHead;
}

export type AppSurfaceMutationOperation =
	| { kind: 'create'; values: Record<string, unknown> }
	| { kind: 'update'; record_id: string; expected_record_revision: number; patch: Record<string, unknown> }
	| { kind: 'delete'; record_id: string; expected_record_revision: number }
	| { kind: 'restore'; record_id: string; expected_record_revision: number };

export interface AppSurfaceMutationRequest {
	protocol_version: '1';
	surface_revision: number;
	view_id: string;
	client_mutation_id: string;
	operation: AppSurfaceMutationOperation;
}

export class AppSurfaceClientError extends Error {
	constructor(
		message: string,
		readonly status: number,
		readonly code: string
	) {
		super(message);
		this.name = 'AppSurfaceClientError';
	}
}

/** Another host is relevant only when this surface kind is absent. Transient
 * failures, stale revisions and denials must not multiply registry/session work. */
export function appSurfaceHostFallbackAllowed(cause: unknown, host: 'native' | 'scripted'): boolean {
	if (!(cause instanceof AppSurfaceClientError) || cause.status !== 404) return false;
	return host === 'native'
		? cause.code === 'app_surface_not_found'
		: cause.code === 'app_custom_surface_not_found' || cause.code === 'app_custom_surface_unavailable';
}

export type AppSurfaceMutationFailureKind = 'stale' | 'definite' | 'ambiguous';

export function appSurfaceMutationFailureKind(cause: unknown): AppSurfaceMutationFailureKind {
	if (!(cause instanceof AppSurfaceClientError)) return 'ambiguous';
	return cause.status === 409 || cause.status === 410 ? 'stale' : 'definite';
}

export function appSurfaceRealtimeSupported(protocol: string): boolean {
	return protocol === 'http:' || protocol === 'https:';
}

export function retainedAppSurfaceMutationRequest(
	request: AppSurfaceMutationRequest,
	cause: unknown
): AppSurfaceMutationRequest | null {
	return appSurfaceMutationFailureKind(cause) === 'ambiguous' ? request : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function string(value: unknown): string {
	return typeof value === 'string' ? value : '';
}

function isAppName(value: string): boolean {
	return /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value);
}

function isOpaqueId(value: string): boolean {
	return /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(value);
}

function isAppReference(value: string): boolean {
	return value.length <= 192 && /^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/.test(value);
}

function positiveInteger(value: unknown): number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : 0;
}

function nonNegativeInteger(value: unknown): number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : -1;
}

function hasOnlyKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
	const allowed = new Set(keys);
	return Object.keys(value).every((key) => allowed.has(key));
}

function optionalContractString(value: unknown): string | undefined {
	if (value === undefined) return undefined;
	if (typeof value !== 'string' || value.length === 0 || value.length > 128) {
		throw new AppSurfaceClientError('The app surface view contract is invalid.', 500, 'invalid_surface_contract');
	}
	return value;
}

function displayLabel(value: string): string {
	return value
		.split(/[_-]+/g)
		.filter(Boolean)
		.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
		.join(' ');
}

function parseSurfaceComponents(value: unknown, visibleFields: ReadonlySet<string>): AppSurfaceComponent[] {
	if (value === undefined) return [];
	if (!Array.isArray(value)) {
		throw new AppSurfaceClientError('The app surface component contract is invalid.', 500, 'invalid_surface_contract');
	}
	let byteLength = 0;
	try {
		byteLength = new TextEncoder().encode(JSON.stringify(value)).byteLength;
	} catch {
		throw new AppSurfaceClientError('The app surface component contract is invalid.', 500, 'invalid_surface_contract');
	}
	if (byteLength > MAX_DECLARATIVE_BYTES) {
		throw new AppSurfaceClientError('The app surface component contract exceeds its byte bound.', 500, 'invalid_surface_contract');
	}
	const roots: AppSurfaceComponent[] = [];
	const stack: Array<{ source: unknown[]; target: AppSurfaceComponent[]; depth: number }> = [
		{ source: value, target: roots, depth: 1 }
	];
	const ids = new Set<string>();
	let count = 0;
	while (stack.length > 0) {
		const frame = stack.pop();
		if (!frame || frame.depth > MAX_DECLARATIVE_DEPTH) {
			throw new AppSurfaceClientError('The app surface component contract is too deep.', 500, 'invalid_surface_contract');
		}
		for (const raw of frame.source) {
			count += 1;
			if (!isRecord(raw) || count > MAX_DECLARATIVE_COMPONENTS) {
				throw new AppSurfaceClientError('The app surface component contract exceeds its node bound.', 500, 'invalid_surface_contract');
			}
			const kind = string(raw.kind) as AppSurfaceComponent['kind'];
			const id = string(raw.id);
			const label = string(raw.label);
			if (!isAppName(id) || ids.has(id) || label !== displayLabel(id)) {
				throw new AppSurfaceClientError('The app surface component identity is invalid.', 500, 'invalid_surface_contract');
			}
			ids.add(id);
			if (kind === 'section') {
				if (!hasOnlyKeys(raw, ['kind', 'id', 'label', 'children']) || !Array.isArray(raw.children) || raw.children.length === 0) {
					throw new AppSurfaceClientError('The app surface section contract is invalid.', 500, 'invalid_surface_contract');
				}
				const children: AppSurfaceComponent[] = [];
				frame.target.push({ kind, id, label, children });
				stack.push({ source: raw.children, target: children, depth: frame.depth + 1 });
				continue;
			}
			const fieldKey = kind === 'table' ? 'columns' : 'fields';
			const rawFields = raw[fieldKey];
			if (
				!['detail', 'form', 'list', 'table'].includes(kind)
				|| !hasOnlyKeys(raw, ['kind', 'id', 'label', fieldKey])
				|| !Array.isArray(rawFields)
				|| rawFields.length === 0
				|| rawFields.length > MAX_FIELDS
				|| rawFields.some((field) => typeof field !== 'string' || !visibleFields.has(field))
				|| new Set(rawFields as string[]).size !== rawFields.length
			) {
				throw new AppSurfaceClientError('The app surface component fields are invalid.', 500, 'invalid_surface_contract');
			}
			const fields = rawFields as string[];
			frame.target.push(kind === 'table'
				? { kind, id, label, columns: fields }
				: { kind, id, label, fields } as AppSurfaceComponent);
		}
	}
	return roots;
}

function parseFieldBindings(value: unknown): AppSurfaceFieldBinding[] {
	if (!Array.isArray(value) || value.length === 0 || value.length > MAX_FIELDS) {
		throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
	}
	const seen = new Set<string>();
	return value.map((raw) => {
		if (!isRecord(raw)) throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
		if (!hasOnlyKeys(raw, ['field', 'kind', 'required', 'nullable', 'sortable', 'allowedValues', 'referenceEntity'])) {
			throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
		}
		const field = string(raw.field);
		const kind = string(raw.kind) as AppSurfaceFieldKind;
		if (
			!isAppName(field)
			|| seen.has(field)
			|| !['text', 'markdown', 'integer', 'decimal', 'boolean', 'timestamp', 'enum', 'reference'].includes(kind)
			|| typeof raw.required !== 'boolean'
			|| typeof raw.nullable !== 'boolean'
			|| typeof raw.sortable !== 'boolean'
		) {
			throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
		}
		seen.add(field);
		const rawAllowedValues = raw.allowedValues;
		if (
			rawAllowedValues !== undefined
			&& (!Array.isArray(rawAllowedValues)
				|| rawAllowedValues.length > MAX_FIELDS
				|| rawAllowedValues.some((allowed) => typeof allowed !== 'string' || !isAppName(allowed)))
		) {
			throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
		}
		const allowedValues = Array.isArray(rawAllowedValues) ? rawAllowedValues as string[] : [];
		if (
			(kind === 'enum') !== (allowedValues.length > 0)
			|| new Set(allowedValues).size !== allowedValues.length
		) {
			throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
		}
		const referenceEntity = string(raw.referenceEntity) || undefined;
		if (
			(kind === 'reference') !== (referenceEntity !== undefined)
			|| (referenceEntity !== undefined && !isAppName(referenceEntity))
		) {
			throw new AppSurfaceClientError('The app surface field contract is invalid.', 500, 'invalid_surface_contract');
		}
		return {
			field,
			kind,
			required: raw.required === true,
			nullable: raw.nullable === true,
			sortable: raw.sortable === true,
			allowedValues,
			...(referenceEntity ? { referenceEntity } : {})
		};
	});
}

function parseRouteBindings(value: unknown): AppSurfaceRouteBinding[] {
	if (value === undefined) return [];
	if (!Array.isArray(value) || value.length > MAX_ROUTE_BINDINGS) {
		throw new AppSurfaceClientError('The app surface route contract is invalid.', 500, 'invalid_surface_contract');
	}
	const parameters = new Set<string>();
	const fields = new Set<string>();
	return value.map((raw) => {
		if (
			!isRecord(raw)
			|| !hasOnlyKeys(raw, ['parameter', 'field', 'scalarKind', 'allowedValues'])
		) {
			throw new AppSurfaceClientError('The app surface route contract is invalid.', 500, 'invalid_surface_contract');
		}
		const parameter = string(raw.parameter);
		const field = string(raw.field);
		const scalarKind = string(raw.scalarKind) as AppSurfaceRouteBinding['scalarKind'];
		const rawAllowedValues = raw.allowedValues;
		const allowedValues = rawAllowedValues === undefined ? [] : rawAllowedValues;
		if (
			!isAppName(parameter)
			|| field !== parameter
			|| fields.has(field)
			|| parameters.has(parameter)
			|| !['text', 'integer', 'decimal', 'boolean', 'enum', 'reference'].includes(scalarKind)
			|| !Array.isArray(allowedValues)
			|| allowedValues.length > MAX_FIELDS
			|| allowedValues.some((allowed) => typeof allowed !== 'string' || !isAppName(allowed))
			|| (scalarKind === 'enum') !== (allowedValues.length > 0)
			|| new Set(allowedValues).size !== allowedValues.length
		) {
			throw new AppSurfaceClientError('The app surface route contract is invalid.', 500, 'invalid_surface_contract');
		}
		parameters.add(parameter);
		fields.add(field);
		return { parameter, field, scalarKind, allowedValues: allowedValues as string[] };
	});
}

function parseViewBinding(value: unknown): AppSurfaceViewBinding {
	if (!isRecord(value)) throw new AppSurfaceClientError('The app surface has no view contract.', 500, 'invalid_surface_contract');
	if (!hasOnlyKeys(value, [
		'entity', 'viewKind', 'fields', 'fieldBindings', 'routeBindings',
		'surfaceComponents',
		'labelField', 'partitionField', 'parentField', 'orderField', 'statusField', 'maxDepth',
		'timestampField', 'actionField', 'actorField', 'typeField', 'targetField', 'defaultActor', 'defaultType'
	])) {
		throw new AppSurfaceClientError('The app surface view contract is invalid.', 500, 'invalid_surface_contract');
	}
	const entity = string(value.entity);
	const viewKind = string(value.viewKind) as AppSurfaceViewBinding['viewKind'];
	const fields = Array.isArray(value.fields) && value.fields.every((field) => typeof field === 'string')
		? value.fields as string[]
		: [];
	if (
		!isAppName(entity)
		|| !['list', 'table', 'tree', 'timeline'].includes(viewKind)
		|| fields.length === 0
		|| fields.length > MAX_FIELDS
		|| fields.some((field) => !isAppName(field))
		|| new Set(fields).size !== fields.length
	) {
		throw new AppSurfaceClientError('The app surface view contract is invalid.', 500, 'invalid_surface_contract');
	}
	const fieldBindings = parseFieldBindings(value.fieldBindings);
	const routeBindings = parseRouteBindings(value.routeBindings);
	const boundFields = new Set(fieldBindings.map((binding) => binding.field));
	if (fields.some((field) => !boundFields.has(field))) {
		throw new AppSurfaceClientError('The app surface projection is not declared by its field contract.', 500, 'invalid_surface_contract');
	}
	const surfaceComponents = parseSurfaceComponents(value.surfaceComponents, new Set(fields));
	const metadata = {
		labelField: optionalContractString(value.labelField),
		partitionField: optionalContractString(value.partitionField),
		parentField: optionalContractString(value.parentField),
		orderField: optionalContractString(value.orderField),
		statusField: optionalContractString(value.statusField),
		timestampField: optionalContractString(value.timestampField),
		actionField: optionalContractString(value.actionField),
		actorField: optionalContractString(value.actorField),
		typeField: optionalContractString(value.typeField),
		targetField: optionalContractString(value.targetField),
		defaultActor: optionalContractString(value.defaultActor),
		defaultType: optionalContractString(value.defaultType)
	};
	const projectedMetadata = [
		metadata.labelField,
		metadata.partitionField,
		metadata.parentField,
		metadata.orderField,
		metadata.statusField,
		metadata.timestampField,
		metadata.actionField,
		metadata.actorField,
		metadata.typeField,
		metadata.targetField
	];
	if (projectedMetadata.some((field) => field !== undefined && !fields.includes(field))) {
		throw new AppSurfaceClientError('The app surface metadata is outside its projection.', 500, 'invalid_surface_contract');
	}
	const maxDepth = value.maxDepth === undefined ? undefined : positiveInteger(value.maxDepth);
	if (value.maxDepth !== undefined && maxDepth === 0) {
		throw new AppSurfaceClientError('The app surface view contract is invalid.', 500, 'invalid_surface_contract');
	}
	const treeMetadataAbsent = metadata.labelField === undefined
		&& metadata.partitionField === undefined
		&& metadata.parentField === undefined
		&& metadata.orderField === undefined
		&& metadata.statusField === undefined
		&& maxDepth === undefined;
	const timelineMetadataAbsent = metadata.timestampField === undefined
		&& metadata.actionField === undefined
		&& metadata.actorField === undefined
		&& metadata.typeField === undefined
		&& metadata.targetField === undefined
		&& metadata.defaultActor === undefined
		&& metadata.defaultType === undefined;
	const validShape = viewKind === 'list' || viewKind === 'table'
		? treeMetadataAbsent && timelineMetadataAbsent
		: viewKind === 'tree'
			? metadata.labelField !== undefined
				&& maxDepth === APP_TREE_MAX_DEPTH
				&& timelineMetadataAbsent
			: treeMetadataAbsent
				&& metadata.timestampField !== undefined
				&& metadata.actionField !== undefined
				&& metadata.defaultActor !== undefined
				&& metadata.defaultType === 'update';
	if (!validShape) {
		throw new AppSurfaceClientError('The app surface view shape does not match its view kind.', 500, 'invalid_surface_contract');
	}
	return {
		entity,
		viewKind,
		fields,
		fieldBindings,
		routeBindings,
		surfaceComponents,
		...metadata,
		...(maxDepth === undefined ? {} : { maxDepth })
	};
}

function cloneDocumentAndFindQuery(document: MuijDocument): { components: MuijComponent[]; query: MuijComponent } {
	if (!document || !Array.isArray(document.layout)) {
		throw new AppSurfaceClientError('The app surface document is invalid.', 500, 'invalid_surface_document');
	}
	const roots: MuijComponent[] = [];
	const stack: Array<{ source: MuijComponent[]; target: MuijComponent[]; depth: number }> = [
		{ source: document.layout, target: roots, depth: 1 }
	];
	let count = 0;
	let query: MuijComponent | null = null;
	while (stack.length > 0) {
		const frame = stack.pop();
		if (!frame || frame.depth > MAX_COMPONENT_DEPTH) throw new AppSurfaceClientError('The app surface document is too deep.', 500, 'invalid_surface_document');
		for (const component of frame.source) {
			count += 1;
			if (
				count > MAX_COMPONENTS
				|| !component
				|| typeof component.id !== 'string'
				|| component.id.length === 0
				|| typeof component.component_type !== 'string'
				|| component.component_type.length === 0
				|| !isRecord(component.props)
				|| (component.children !== undefined && !Array.isArray(component.children))
			) {
				throw new AppSurfaceClientError('The app surface document exceeds its render bounds.', 500, 'invalid_surface_document');
			}
			const children: MuijComponent[] = [];
			const cloned: MuijComponent = {
				...component,
				props: { ...component.props },
				children
			};
			frame.target.push(cloned);
			if (component.source === 'app_entity_query_v1' && typeof component.query === 'string' && component.query.startsWith('view:')) {
				if (query) throw new AppSurfaceClientError('The app surface has multiple data owners.', 500, 'invalid_surface_document');
				query = cloned;
			}
			if (Array.isArray(component.children) && component.children.length > 0) {
				stack.push({ source: component.children, target: children, depth: frame.depth + 1 });
			}
		}
	}
	if (!query) throw new AppSurfaceClientError('The app surface has no data owner.', 500, 'invalid_surface_document');
	if (!query.id.startsWith('app-')) {
		throw new AppSurfaceClientError('The app surface data owner has an invalid identity.', 500, 'invalid_surface_document');
	}
	return { components: roots, query };
}

function validateSurfaceRecords(
	records: AppSurfaceRecord[],
	view: AppSurfaceViewBinding
): void {
	const recordIds = new Set<string>();
	const boundFields = new Set(view.fieldBindings.map((binding) => binding.field));
	for (const record of records) {
		if (
			!isRecord(record)
			|| record.entity !== view.entity
			|| typeof record.record_id !== 'string'
			|| !isOpaqueId(record.record_id)
			|| recordIds.has(record.record_id)
			|| !Number.isSafeInteger(record.record_revision)
			|| record.record_revision <= 0
			|| !isRecord(record.fields)
			|| Object.keys(record.fields).length > MAX_FIELDS
			|| Object.keys(record.fields).some((field) => !boundFields.has(field))
		) {
			throw new AppSurfaceClientError('The app surface returned an invalid record page.', 500, 'invalid_surface_page');
		}
		recordIds.add(record.record_id);
	}
}

function recordRows(records: AppSurfaceRecord[]): Array<Record<string, unknown>> {
	return records.map((record) => ({
		__record_id: record.record_id,
		__record_revision: record.record_revision,
		...record.fields
	}));
}

function treeNodes(records: AppSurfaceRecord[], view: AppSurfaceViewBinding): Array<Record<string, unknown>> {
	const labelField = view.labelField;
	if (!labelField) return [];
	const byId = new Map<string, Record<string, unknown>>();
	const parentById = new Map<string, string>();
	for (const record of records) {
		const node: Record<string, unknown> = {
			id: record.record_id,
			label: String(record.fields[labelField] ?? record.record_id),
			children: []
		};
		if (view.statusField && typeof record.fields[view.statusField] === 'string') node.status = record.fields[view.statusField];
		byId.set(record.record_id, node);
		if (view.parentField && typeof record.fields[view.parentField] === 'string') parentById.set(record.record_id, record.fields[view.parentField] as string);
	}
	const roots: Array<Record<string, unknown>> = [];
	const maximumDepth = view.maxDepth ?? APP_TREE_MAX_DEPTH;
	for (const [id, node] of byId) {
		const parentId = parentById.get(id);
		if (parentId) {
			if (!byId.has(parentId)) {
				throw new AppSurfaceClientError('The app surface tree contains a missing parent.', 500, 'invalid_surface_page');
			}
			const seen = new Set<string>([id]);
			let cursor: string | undefined = parentId;
			while (cursor && byId.has(cursor)) {
				if (seen.has(cursor)) {
					throw new AppSurfaceClientError('The app surface tree contains a parent cycle.', 500, 'invalid_surface_page');
				}
				seen.add(cursor);
				if (seen.size > maximumDepth) {
					throw new AppSurfaceClientError('The app surface tree exceeds its declared depth.', 500, 'invalid_surface_page');
				}
				cursor = parentById.get(cursor);
			}
		}
		const parent = parentId && parentId !== id ? byId.get(parentId) : undefined;
		if (parent) (parent.children as Array<Record<string, unknown>>).push(node);
		else roots.push(node);
	}
	return roots;
}

function timelineItems(records: AppSurfaceRecord[], view: AppSurfaceViewBinding): Array<Record<string, unknown>> {
	const timestampField = view.timestampField;
	const actionField = view.actionField;
	if (!timestampField || !actionField) return [];
	const targetField = view.targetField;
	return records.map((record) => ({
		id: record.record_id,
		type: String(view.typeField ? record.fields[view.typeField] ?? view.defaultType ?? 'update' : view.defaultType ?? 'update'),
		actor: String(view.actorField ? record.fields[view.actorField] ?? view.defaultActor ?? 'App' : view.defaultActor ?? 'App'),
		action: String(record.fields[actionField] ?? ''),
		...(targetField && record.fields[targetField] != null ? { target: String(record.fields[targetField]) } : {}),
		timestamp: String(record.fields[timestampField] ?? '')
	}));
}

export function hydrateAppSurface(hydration: AppSurfaceHydration, position: AppSurfacePagePosition): AppSurfaceRenderPage {
	if (
		!isRecord(hydration)
		|| !isRecord(hydration.binding)
		|| !isRecord(hydration.surface)
		|| !isRecord(hydration.page)
		|| !isRecord(hydration.page.envelope)
		|| typeof hydration.binding.installation_id !== 'string'
		|| !isOpaqueId(hydration.binding.installation_id)
		|| typeof hydration.binding.view_id !== 'string'
		|| !isAppName(hydration.binding.view_id)
		|| typeof hydration.binding.app_local_route !== 'string'
		|| !isRecord(hydration.route_parameters)
		|| Object.values(hydration.route_parameters).some((parameter) => typeof parameter !== 'string')
		|| !Number.isSafeInteger(hydration.binding.surface_revision)
		|| hydration.binding.surface_revision <= 0
		|| nonNegativeInteger(hydration.change_sequence) < 0
		|| !Number.isSafeInteger(hydration.page.envelope.schema_revision)
		|| hydration.page.envelope.schema_revision <= 0
		|| (hydration.page.next_cursor !== undefined
			&& (typeof hydration.page.next_cursor !== 'string' || hydration.page.next_cursor.length === 0))
	) {
		throw new AppSurfaceClientError('The app surface response is malformed.', 500, 'invalid_surface_identity');
	}
	if (
		!Number.isSafeInteger(position.page)
		|| position.page <= 0
		|| !Number.isSafeInteger(position.pageSize)
		|| position.pageSize <= 0
		|| position.pageSize !== APP_SURFACE_ROW_LIMIT
		|| typeof position.hasNextPage !== 'boolean'
	) {
		throw new AppSurfaceClientError('The app surface page position is invalid.', 500, 'invalid_surface_page');
	}
	const records = hydration.page.envelope.value;
	if (!Array.isArray(records) || records.length > APP_SURFACE_ROW_LIMIT || records.length > position.pageSize) {
		throw new AppSurfaceClientError('The app surface page exceeds its record bound.', 500, 'invalid_surface_page');
	}
	if (
		hydration.surface.interaction_mode !== 'app'
		|| hydration.surface.installation_id !== hydration.binding.installation_id
		|| hydration.surface.surface_revision !== hydration.binding.surface_revision
		|| hydration.surface.view_id !== hydration.binding.view_id
		|| hydration.page.envelope.installation_id !== hydration.binding.installation_id
		|| Boolean(hydration.page.next_cursor) !== position.hasNextPage
	) {
		throw new AppSurfaceClientError('The app surface identity is inconsistent.', 500, 'invalid_surface_identity');
	}
	const { components, query } = cloneDocumentAndFindQuery(hydration.surface.muij_document);
	if (query.query !== `view:${hydration.binding.view_id}`) {
		throw new AppSurfaceClientError('The app surface data owner does not match its bound view.', 500, 'invalid_surface_identity');
	}
	const view = parseViewBinding(query.props.viewBinding);
	if (view.viewKind === 'tree' && position.hasNextPage) {
		throw new AppSurfaceClientError(
			'The app surface tree must fit in one canonical page.',
			422,
			'app_tree_record_limit_exceeded'
		);
	}
	const expectedComponent = view.surfaceComponents.length > 0
		? 'Stack'
		: view.viewKind === 'tree'
		? 'Tree'
		: view.viewKind === 'timeline'
			? 'ActivityFeed'
			: 'EntityGrid';
	if (query.component_type !== expectedComponent) {
		throw new AppSurfaceClientError('The app surface view type does not match its data owner.', 500, 'invalid_surface_contract');
	}
	const hasSortField = position.sortField !== undefined;
	const hasSortDirection = position.sortDirection !== undefined;
	const sortBinding = hasSortField
		? view.fieldBindings.find((binding) => binding.field === position.sortField)
		: undefined;
	if (
		hasSortField !== hasSortDirection
		|| (hasSortDirection && !['ascending', 'descending'].includes(position.sortDirection as string))
		|| (hasSortField && (!view.fields.includes(position.sortField as string) || sortBinding?.sortable !== true))
	) {
		throw new AppSurfaceClientError('The app surface sort is outside its declared view.', 500, 'invalid_surface_contract');
	}
	validateSurfaceRecords(records, view);
	const totalItems = (position.page - 1) * position.pageSize + records.length;
	if (!Number.isSafeInteger(totalItems)) {
		throw new AppSurfaceClientError('The app surface page position overflowed.', 500, 'invalid_surface_page');
	}
	const rows = recordRows(records);
	if (view.surfaceComponents.length > 0) {
		// The closed component binding reads the already-validated page directly.
		// Package content cannot inject rows, a query or a transport through MUIJ.
	} else if (view.viewKind === 'list' || view.viewKind === 'table') {
		query.props = {
			...query.props,
			rows,
			paginationMode: 'server',
			currentPage: position.page,
			pageCount: position.page + (position.hasNextPage ? 1 : 0),
			pageCountExact: !position.hasNextPage,
			totalItems,
			totalItemsExact: !position.hasNextPage,
			startItem: records.length === 0 ? 0 : (position.page - 1) * position.pageSize + 1,
			endItem: totalItems,
			sortKey: position.sortField ?? '',
			sortDir: position.sortDirection === 'descending' ? 'desc' : 'asc',
			expandable: true,
			actions: [
				{ id: 'edit', label: 'Edit' },
				{ id: 'delete', label: 'Delete', variant: 'danger' }
			]
		};
	} else if (view.viewKind === 'tree') {
		query.props = { ...query.props, nodes: treeNodes(records, view), selectable: true };
	} else {
		query.props = { ...query.props, items: timelineItems(records, view) };
	}
	return { components, view, records, hasNextPage: Boolean(hydration.page.next_cursor) };
}

export function surfaceInteractionIsAppOwned(detail: MuijInteractionEventDetail): boolean {
	return detail.sent === false && detail.componentId.startsWith('app-');
}

export function buildSurfaceMutationRequest(
	hydration: AppSurfaceHydration,
	clientMutationId: string,
	operation: AppSurfaceMutationOperation
): AppSurfaceMutationRequest {
	if (!isAppReference(clientMutationId)) throw new Error('Invalid client mutation id');
	return {
		protocol_version: '1',
		surface_revision: hydration.surface.surface_revision,
		view_id: hydration.surface.view_id,
		client_mutation_id: clientMutationId,
		operation
	};
}

export function optimisticAppSurfaceRecords(
	records: AppSurfaceRecord[],
	entity: string,
	clientMutationId: string,
	operation: AppSurfaceMutationOperation,
	pageSize: number
): AppSurfaceRecord[] {
	const next = records.flatMap((record) => {
		if (operation.kind === 'delete' && record.record_id === operation.record_id) return [];
		if (operation.kind === 'update' && record.record_id === operation.record_id) {
			return [{ ...record, fields: { ...record.fields, ...operation.patch } }];
		}
		return [record];
	});
	if (operation.kind === 'create') {
		const temporaryId = `optimistic_${clientMutationId.replace(/[^A-Za-z0-9_.-]/g, '_')}`.slice(0, 128);
		next.unshift({
			entity,
			record_id: temporaryId,
			record_revision: 1,
			fields: operation.values
		});
	}
	const boundedPageSize = Number.isSafeInteger(pageSize) && pageSize > 0
		? Math.min(pageSize, MAX_RECORDS)
		: 25;
	return next.slice(0, boundedPageSize);
}

export function coerceAppSurfaceForm(
	bindings: AppSurfaceFieldBinding[],
	raw: Record<string, string | boolean>
): Record<string, unknown> {
	const values: Record<string, unknown> = {};
	for (const binding of bindings) {
		const input = raw[binding.field];
		if (binding.kind === 'boolean') {
			if (binding.nullable) {
				if (input === '') values[binding.field] = null;
				else if (input === 'true') values[binding.field] = true;
				else if (input === 'false') values[binding.field] = false;
				else throw new Error(`${binding.field} must be Yes, No, or None.`);
			} else {
				values[binding.field] = input === true;
			}
			continue;
		}
		const rawText = typeof input === 'string' ? input : '';
		const text = rawText.trim();
		if (!text) {
			if (binding.required && !binding.nullable) throw new Error(`${binding.field} is required.`);
			if (binding.nullable) values[binding.field] = null;
			else if (binding.kind === 'text' || binding.kind === 'markdown') values[binding.field] = '';
			continue;
		}
		switch (binding.kind) {
			case 'integer': {
				const value = Number(text);
				if (!Number.isSafeInteger(value) || String(value) !== text) throw new Error(`${binding.field} must be a whole number.`);
				values[binding.field] = value;
				break;
			}
			case 'decimal': {
				const value = Number(text);
				if (!Number.isFinite(value)) throw new Error(`${binding.field} must be a number.`);
				values[binding.field] = value;
				break;
			}
			case 'timestamp': {
				const timestamp = new Date(text);
				if (!Number.isFinite(timestamp.getTime())) throw new Error(`${binding.field} must be a valid date and time.`);
				values[binding.field] = timestamp.toISOString();
				break;
			}
			case 'enum':
				if (!binding.allowedValues.includes(text)) throw new Error(`${binding.field} has an unsupported value.`);
				values[binding.field] = text;
				break;
			case 'text':
			case 'markdown':
				values[binding.field] = rawText;
				break;
			case 'reference':
				values[binding.field] = text;
				break;
		}
	}
	return values;
}

export function formatAppSurfaceTimestampInput(value: unknown): string {
	if (typeof value !== 'string') return '';
	const timestamp = new Date(value);
	if (!Number.isFinite(timestamp.getTime())) return value;
	const pad = (part: number, width = 2) => String(part).padStart(width, '0');
	const base = `${timestamp.getFullYear()}-${pad(timestamp.getMonth() + 1)}-${pad(timestamp.getDate())}T${pad(timestamp.getHours())}:${pad(timestamp.getMinutes())}:${pad(timestamp.getSeconds())}`;
	return timestamp.getMilliseconds() === 0 ? base : `${base}.${pad(timestamp.getMilliseconds(), 3)}`;
}

export function diffAppSurfaceValues(
	previous: Record<string, unknown>,
	next: Record<string, unknown>
): Record<string, unknown> {
	const patch: Record<string, unknown> = {};
	for (const [field, value] of Object.entries(next)) {
		if (JSON.stringify(previous[field]) !== JSON.stringify(value)) patch[field] = value;
	}
	return patch;
}

function surfaceApiPath(installationId: string, route: string): string {
	const normalized = route.replace(/^\/+|\/+$/g, '');
	const suffix = normalized ? `/${normalized.split('/').map(encodeURIComponent).join('/')}` : '';
	return `/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/surfaces${suffix}`;
}

const MAX_APP_SURFACE_RESPONSE_BYTES = 8 * 1024 * 1024;

async function responseJson(response: Response): Promise<unknown> {
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const declaredLength = Number(declared);
		if (!Number.isSafeInteger(declaredLength) || declaredLength < 0) {
			throw new AppSurfaceClientError('The app service returned an invalid response.', response.status, 'invalid_response');
		}
		if (declaredLength > MAX_APP_SURFACE_RESPONSE_BYTES) {
			throw new AppSurfaceClientError('The app service response exceeded its size limit.', response.status, 'app_response_too_large');
		}
	}
	if (!response.body) return {};
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_APP_SURFACE_RESPONSE_BYTES) {
			try {
				await reader.cancel();
			} catch {
				// The bounded rejection remains authoritative even if a hostile or
				// already-closed stream refuses cancellation.
			}
			throw new AppSurfaceClientError('The app service response exceeded its size limit.', response.status, 'app_response_too_large');
		}
		chunks.push(value);
	}
	const bytes = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	let text: string;
	try {
		text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
	} catch {
		throw new AppSurfaceClientError('The app service returned an invalid response.', response.status, 'invalid_response');
	}
	if (!text) return {};
	try {
		return JSON.parse(text) as unknown;
	} catch {
		throw new AppSurfaceClientError('The app service returned an invalid response.', response.status, 'invalid_response');
	}
}

function responseError(
	body: unknown,
	fallbackMessage: string,
	fallbackCode: string
): { message: string; code: string } {
	if (!isRecord(body)) return { message: fallbackMessage, code: fallbackCode };
	const nested = isRecord(body.error) ? body.error : body;
	const message = string(nested.message) || string(body.message) || fallbackMessage;
	const code = string(nested.code) || string(body.code) || string(body.error) || fallbackCode;
	return { message, code };
}

export async function fetchAppSurface(
	installationId: string,
	route: string,
	options: { cursor?: string; sortField?: string; sortDirection?: 'ascending' | 'descending'; signal?: AbortSignal } = {}
): Promise<AppSurfaceHydration> {
	const query = new URLSearchParams();
	if (options.cursor) query.set('cursor', options.cursor);
	if (options.sortField && options.sortDirection) {
		query.set('sort_field', options.sortField);
		query.set('sort_direction', options.sortDirection);
	}
	const serializedQuery = query.toString();
	const response = await fetch(`${surfaceApiPath(installationId, route)}${serializedQuery ? `?${serializedQuery}` : ''}`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		signal: options.signal
	});
	const body = await responseJson(response);
	if (!response.ok) {
		const error = responseError(body, 'The app surface could not be loaded.', 'app_surface_failed');
		throw new AppSurfaceClientError(error.message, response.status, error.code);
	}
	return body as AppSurfaceHydration;
}

function parseEntityChanges(value: unknown, maximum: number): AppEntityChange[] {
	if (!Array.isArray(value) || value.length > maximum) {
		throw new AppSurfaceClientError('The app change response is invalid.', 500, 'invalid_app_changes');
	}
	return value.map((raw) => {
		if (!isRecord(raw) || !hasOnlyKeys(raw, ['entity', 'record_id', 'record_revision', 'change_sequence'])) {
			throw new AppSurfaceClientError('The app change response is invalid.', 500, 'invalid_app_changes');
		}
		const entity = string(raw.entity);
		const recordId = string(raw.record_id);
		const recordRevision = positiveInteger(raw.record_revision);
		const changeSequence = positiveInteger(raw.change_sequence);
		if (!isAppName(entity) || !isOpaqueId(recordId) || recordRevision === 0 || changeSequence === 0) {
			throw new AppSurfaceClientError('The app change response is invalid.', 500, 'invalid_app_changes');
		}
		return {
			entity,
			record_id: recordId,
			record_revision: recordRevision,
			change_sequence: changeSequence
		};
	});
}

function parseAppEntityChangeBatch(
	value: unknown,
	installationId: string,
	surfaceRevision: number,
	afterChangeSequence: number
): AppEntityChangeBatch {
	if (!isRecord(value) || !hasOnlyKeys(value, [
		'installation_id', 'surface_revision', 'after_change_sequence',
		'through_change_sequence', 'current_change_sequence', 'changes',
		'has_more', 'reset_required'
	])) {
		throw new AppSurfaceClientError('The app change response is invalid.', 500, 'invalid_app_changes');
	}
	const after = nonNegativeInteger(value.after_change_sequence);
	const through = nonNegativeInteger(value.through_change_sequence);
	const current = nonNegativeInteger(value.current_change_sequence);
	const changes = parseEntityChanges(value.changes, 128);
	if (
		value.installation_id !== installationId
		|| positiveInteger(value.surface_revision) !== surfaceRevision
		|| after !== afterChangeSequence
		|| through < after
		|| through > current
		|| typeof value.has_more !== 'boolean'
		|| typeof value.reset_required !== 'boolean'
		|| (value.reset_required === true && (changes.length !== 0 || value.has_more === true))
		|| (changes.length > 0 && changes[0].change_sequence !== after + 1)
		|| changes.some((change, index) => change.change_sequence !== after + index + 1)
		|| (!value.reset_required && (changes.at(-1)?.change_sequence ?? after) !== through)
		|| (value.reset_required && through !== current)
		|| (!value.has_more && !value.reset_required && through !== current)
	) {
		throw new AppSurfaceClientError('The app change response is inconsistent.', 500, 'invalid_app_changes');
	}
	return {
		installation_id: installationId,
		surface_revision: surfaceRevision,
		after_change_sequence: after,
		through_change_sequence: through,
		current_change_sequence: current,
		changes,
		has_more: value.has_more,
		reset_required: value.reset_required
	};
}

export async function fetchAppEntityChanges(
	installationId: string,
	surfaceRevision: number,
	afterChangeSequence: number,
	signal?: AbortSignal
): Promise<AppEntityChangeBatch> {
	if (!isOpaqueId(installationId) || positiveInteger(surfaceRevision) === 0 || nonNegativeInteger(afterChangeSequence) < 0) {
		throw new AppSurfaceClientError('The app change request is invalid.', 400, 'invalid_app_changes');
	}
	const query = new URLSearchParams({
		surface_revision: String(surfaceRevision),
		after_change_sequence: String(afterChangeSequence),
		limit: '128'
	});
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/entity-changes?${query}`,
		{ headers: scopedRequestHeaders({ Accept: 'application/json' }), signal }
	);
	const body = await responseJson(response);
	if (!response.ok) {
		const error = responseError(body, 'The app changes could not be loaded.', 'app_entity_changes_failed');
		throw new AppSurfaceClientError(error.message, response.status, error.code);
	}
	return parseAppEntityChangeBatch(body, installationId, surfaceRevision, afterChangeSequence);
}

export function parseAppEntityChangeEvent(
	event: unknown,
	installationId: string,
	surfaceRevision?: number
): AppEntityChangeSignal | null {
	if (!isRecord(event) || event.event_type !== 'AgentEvent' || !isRecord(event.data)) return null;
	const envelope = isRecord(event.data.event) ? event.data.event : null;
	if (!envelope || envelope.event_type !== 'app.entity.changed' || !isRecord(envelope.payload)) return null;
	const payload = envelope.payload;
	if (!hasOnlyKeys(payload, [
		'installation_id', 'surface_revision', 'first_change_sequence',
		'last_change_sequence', 'changes', 'reset_required'
	])) return null;
	const eventSurfaceRevision = positiveInteger(payload.surface_revision);
	if (
		payload.installation_id !== installationId
		|| eventSurfaceRevision === 0
		|| typeof payload.reset_required !== 'boolean'
		|| (surfaceRevision !== undefined && eventSurfaceRevision !== surfaceRevision)
	) return null;
	const first = positiveInteger(payload.first_change_sequence);
	const last = positiveInteger(payload.last_change_sequence);
	if (first === 0 || last < first) return null;
	let changes: AppEntityChange[];
	try {
		changes = parseEntityChanges(payload.changes, 256);
	} catch {
		return null;
	}
	if (
		(payload.reset_required
			? changes.length !== 0
			: changes.length !== last - first + 1)
		|| changes.some((change, index) => change.change_sequence !== first + index)
	) return null;
	return {
		installation_id: installationId,
		surface_revision: eventSurfaceRevision,
		first_change_sequence: first,
		last_change_sequence: last,
		changes,
		reset_required: payload.reset_required
	};
}

export async function mutateAppSurface(
	installationId: string,
	request: AppSurfaceMutationRequest,
	signal?: AbortSignal
): Promise<unknown> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/surface-mutations`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify(request),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) {
		const error = responseError(body, 'The app edit could not be saved.', 'app_surface_mutation_failed');
		throw new AppSurfaceClientError(error.message, response.status, error.code);
	}
	return body;
}

export function newClientMutationId(): string {
	return `surface-client:${globalThis.crypto.randomUUID()}`;
}
