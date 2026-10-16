import { parseAppNavigationEntries, type AppNavigationEntry } from './appNavigation';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export type AppDirectorySection =
	| 'installed'
	| 'pinned'
	| 'recent'
	| 'needs_attention'
	| 'disabled'
	| 'recovery';
export type AppDirectoryPinnedTargetKind = 'view' | 'action';

// Keep the browser and native clients on the same strict transport envelope.
// The server budgets directory entries below 3 MiB so the remaining space is
// reserved for the response envelope rather than accepted as payload growth.
const MAX_DIRECTORY_RESPONSE_BYTES = 4 * 1024 * 1024;
const MAX_DIRECTORY_VIEWS_PER_APP = 128;
const MAX_DIRECTORY_ACTIONS_PER_APP = 256;

export interface AppDirectoryView {
	view_id: string;
	label: string;
	route: string;
	pinned: boolean;
}

export interface AppDirectoryAction {
	action_id: string;
	label: string;
	pinned: boolean;
}

export type AppActionInputFieldType =
	| 'text'
	| 'markdown'
	| 'integer'
	| 'decimal'
	| 'boolean'
	| 'timestamp'
	| 'enum'
	| 'reference';

export interface AppActionInputField {
	type: AppActionInputFieldType;
	required: boolean;
	nullable: boolean;
	values?: string[];
	entity?: string;
}

export interface AppDirectActionContract {
	action_id: string;
	input: {
		type: 'object';
		fields: Record<string, AppActionInputField>;
	};
}

export interface AppActionLaunch {
	run_handle: AppRunHandle;
	result?: AppActionResult;
}

export interface AppRunHandle {
	protocol_version: '1';
	run_ref: string;
	installation_id: string;
	action_id: string;
}

export type AppActionErrorDisposition =
	| 'terminal'
	| 'retry_same_input'
	| 'refresh_and_retry'
	| 'reauthorize'
	| 'user_action_required'
	| 'outcome_uncertain';

export interface AppActionError {
	code: string;
	disposition: AppActionErrorDisposition;
	message: string;
	details: Record<string, unknown>;
	retry_after_ms?: number;
}

export interface AppActionSourceRef {
	kind: 'entity_record' | 'entity_field' | 'artifact' | 'external_receipt' | 'mutation_receipt';
	reference: string;
	revision?: number;
	fields: string[];
}

export interface AppActionOutputEnvelope {
	protocol_version: '1';
	source: 'app_action';
	scope_binding_ref: string;
	installation_id: string;
	package_revision_ref: string;
	schema_revision: number;
	grant_revision: number;
	value_schema_ref: string;
	value: unknown;
	source_refs: AppActionSourceRef[];
	handling_labels: {
		classification: 'public' | 'ordinary' | 'personal' | 'sensitive' | 'secret';
		model_processing: 'none' | 'local_only' | 'remote_allowed';
		policy_digest: string;
		provenance_digest: string;
	};
	content_digest: string;
	produced_at: string;
	expires_at?: string;
}

export interface AppActionResult {
	action_id: string;
	run_ref: string;
	status: 'completed' | 'waiting' | 'failed' | 'uncertain';
	output?: AppActionOutputEnvelope;
	mutation_receipt_refs: string[];
	external_effect_receipt_refs: string[];
	error?: AppActionError;
}

export type AppRunStatus =
	| 'queued'
	| 'planning'
	| 'running'
	| 'paused'
	| 'deferred'
	| 'waiting'
	| 'blocked'
	| 'cancelling'
	| 'completed'
	| 'failed'
	| 'cancelled'
	| 'archived'
	| 'uncertain';

export interface AppActionRun {
	run_handle?: AppRunHandle;
	run_ref: string;
	execution_id?: string;
	status: AppRunStatus;
	terminal: boolean;
	cancellation_generation?: number;
	result_withheld: boolean;
	result?: AppActionResult;
}

export interface AppDirectActionExpectedInstallationBinding {
	generation: number;
	package_revision_ref: string;
}

export interface AppDirectoryPermissionSummary {
	granted_tools: number;
	granted_context_reads: number;
	granted_personal_data_projections: number;
	background_execution: boolean;
	network_access: boolean;
}

export interface AppDirectoryStorageSummary {
	record_count: number;
	revision_count: number;
	payload_bytes: number;
	attachment_bytes: number;
}

export interface AppDirectoryEntry {
	installation_id: string;
	name: string;
	description: string;
	icon: { kind: 'monogram'; value: string };
	package_version: string;
	package_revision_ref: string;
	installation_generation: number;
	status: 'ready_for_review' | 'enabled' | 'disabled' | 'update_pending' | 'quarantined' | 'uninstalled_retained' | 'purged';
	default_route?: string;
	views: AppDirectoryView[];
	actions: AppDirectoryAction[];
	// Manifest-declared custom-surface entry points (additive wire field):
	// absent when an older server omits it, normalized to 0 by the parser.
	// The web directory does not gate anything on it yet; the parser accepts
	// it so the shared strict envelope keeps decoding.
	custom_surface_entry_count?: number;
	// Manifest-declared first-party navigation (additive wire field, gate S3).
	// Absent on a server that does not project the declaration yet, and
	// normalized to an empty list by the parser — an app that declares nothing
	// and a server that says nothing are the same thing here, and both mount
	// no navigation. `mountAppNavigation` is the only admission owner.
	navigation?: AppNavigationEntry[];
	last_opened_at?: string;
	attention_reason?: string;
	permissions?: AppDirectoryPermissionSummary;
	storage: AppDirectoryStorageSummary;
	record_count: number;
	payload_bytes: number;
}

export interface AppDirectoryPage {
	entries: AppDirectoryEntry[];
	next_cursor?: string;
	has_more: boolean;
}

export class AppRequestError extends Error {
	readonly status: number;
	readonly code: string | undefined;
	readonly retry_after_ms: number | undefined;

	constructor(message: string, status: number, code?: string, retryAfterMs?: number) {
		super(message);
		this.name = 'AppRequestError';
		this.status = status;
		this.code = code;
		this.retry_after_ms = retryAfterMs;
	}
}

function responseError(response: Response, body: unknown, fallback: string): AppRequestError {
	const error = record(body);
	const message = error && typeof error.message === 'string' ? error.message : fallback;
	const code = error && typeof error.error === 'string' ? error.error : undefined;
	let retryAfterMs = error && typeof error.retry_after_ms === 'number' &&
		Number.isSafeInteger(error.retry_after_ms) && error.retry_after_ms > 0
		? error.retry_after_ms : undefined;
	if (retryAfterMs === undefined) {
		const retryAfter = response.headers.get('retry-after');
		if (retryAfter) {
			const seconds = Number(retryAfter);
			if (Number.isFinite(seconds) && seconds > 0) retryAfterMs = Math.ceil(seconds * 1_000);
		}
	}
	return new AppRequestError(message, response.status, code, retryAfterMs);
}

export function isRetryableAppRequestError(error: unknown): boolean {
	if (error instanceof AppRequestError) {
		return error.status === 408 || error.status === 425 || error.status === 429 ||
			[500, 502, 503, 504].includes(error.status);
	}
	// Fetch rejects with TypeError for transport failures. Contract/parser
	// errors are ordinary Error values and must never be retried blindly.
	return error instanceof TypeError;
}

export function appRequestRetryDelayMs(error: unknown, fallbackMs: number): number {
	return error instanceof AppRequestError && error.retry_after_ms !== undefined
		? error.retry_after_ms : fallbackMs;
}

async function responseJson(response: Response): Promise<unknown> {
	const declared = response.headers.get('content-length');
	if (declared !== null) {
		const declaredLength = Number(declared);
		if (!Number.isSafeInteger(declaredLength) || declaredLength < 0) {
			throw new Error('The Apps directory returned an invalid response.');
		}
		if (declaredLength > MAX_DIRECTORY_RESPONSE_BYTES) {
			throw new Error('The Apps directory response exceeded its size limit.');
		}
	}
	if (!response.body) return null;
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;
	while (true) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_DIRECTORY_RESPONSE_BYTES) {
			try {
				await reader.cancel();
			} catch {
				// The bounded rejection remains authoritative even if the stream
				// has already failed or refuses cancellation.
			}
			throw new Error('The Apps directory response exceeded its size limit.');
		}
		chunks.push(value);
	}
	const bytes = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		bytes.set(chunk, offset);
		offset += chunk.byteLength;
	}
	const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
	if (!text) return null;
	try {
		return JSON.parse(text);
	} catch {
		throw new Error('The Apps directory returned an invalid response.');
	}
}

type JsonRecord = Record<string, unknown>;

const STATUSES = new Set<AppDirectoryEntry['status']>([
	'ready_for_review',
	'enabled',
	'disabled',
	'update_pending',
	'quarantined',
	'uninstalled_retained',
	'purged'
]);

function record(value: unknown): JsonRecord | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as JsonRecord)
		: null;
}

function exactKeys(value: JsonRecord, required: string[], optional: string[] = []): boolean {
	const accepted = new Set([...required, ...optional]);
	return required.every((key) => key in value) && Object.keys(value).every((key) => accepted.has(key));
}

function string(value: unknown, maximum = 4096): value is string {
	return typeof value === 'string' && value.length <= maximum;
}

function count(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function positiveInteger(value: unknown): number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value > 0 ? value : 0;
}

function asciiToken(value: unknown, maximum: number, additional: string): value is string {
	if (typeof value !== 'string' || value.length === 0 || value.length > maximum || !/^[A-Za-z0-9]/.test(value)) return false;
	const allowed = new Set(additional);
	return [...value].every((character) => /[A-Za-z0-9]/.test(character) || allowed.has(character));
}

function appReference(value: unknown): value is string {
	return string(value, 192) && value.length > 0 &&
		/^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/.test(value);
}

function canonicalAppRoute(value: unknown, installationId: string): value is string {
	if (typeof value !== 'string' || value.length > 1024 || value.includes('%') || value.includes('\\') || value.includes('?') || value.includes('#')) return false;
	const segments = value.split('/');
	return segments.length >= 3
		&& segments[0] === ''
		&& segments[1] === 'apps'
		&& segments[2] === installationId
		&& segments.slice(2).every((segment) => asciiToken(segment, 128, '_-.'));
}

function optionalString(value: unknown, maximum = 4096): value is string | undefined {
	return value === undefined || string(value, maximum);
}

function parseView(value: unknown): AppDirectoryView | null {
	const item = record(value);
	if (
		!item ||
		!exactKeys(item, ['view_id', 'label', 'route', 'pinned']) ||
		!string(item.view_id, 64) ||
		!string(item.label, 256) || item.label.length === 0 ||
		!string(item.route, 512) ||
		typeof item.pinned !== 'boolean'
	) return null;
	return { view_id: item.view_id, label: item.label, route: item.route, pinned: item.pinned };
}

function parseAction(value: unknown): AppDirectoryAction | null {
	const item = record(value);
	if (
		!item ||
		!exactKeys(item, ['action_id', 'label', 'pinned']) ||
		!string(item.action_id, 64) ||
		!string(item.label, 256) || item.label.length === 0 ||
		typeof item.pinned !== 'boolean'
	) return null;
	return { action_id: item.action_id, label: item.label, pinned: item.pinned };
}

const ACTION_FIELD_TYPES = new Set<AppActionInputFieldType>([
	'text', 'markdown', 'integer', 'decimal', 'boolean', 'timestamp', 'enum', 'reference'
]);

function parseActionInputField(value: unknown): AppActionInputField | null {
	const item = record(value);
	if (
		!item ||
		!exactKeys(
			item,
			['type', 'required', 'nullable'],
			['values', 'entity', 'on_delete', 'cycle_policy', 'max_traversal_depth', 'data_policy']
		) ||
		typeof item.type !== 'string' ||
		!ACTION_FIELD_TYPES.has(item.type as AppActionInputFieldType) ||
		typeof item.required !== 'boolean' ||
		typeof item.nullable !== 'boolean'
	) return null;
	const type = item.type as AppActionInputFieldType;
	if (type === 'enum') {
		if (
			!Array.isArray(item.values) || item.values.length === 0 || item.values.length > 256 ||
			item.values.some((entry) => !asciiToken(entry, 64, '_-')) ||
			new Set(item.values).size !== item.values.length
		) return null;
		return { type, required: item.required, nullable: item.nullable, values: item.values as string[] };
	}
	if (item.values !== undefined) return null;
	if (type === 'reference' && !asciiToken(item.entity, 64, '_-')) return null;
	if (type !== 'reference' && item.entity !== undefined) return null;
	return {
		type,
		required: item.required,
		nullable: item.nullable,
		...(type === 'reference' ? { entity: item.entity as string } : {})
	};
}

function parseDirectActionContract(value: unknown, actionId: string): AppDirectActionContract {
	const contract = record(value);
	const input = contract ? record(contract.input) : null;
	const rawFields = input ? record(input.fields) : null;
	if (
		!contract || !exactKeys(contract, ['action_id', 'input']) ||
		contract.action_id !== actionId || !input || !exactKeys(input, ['type', 'fields']) ||
		input.type !== 'object' || !rawFields || Object.keys(rawFields).length > 128
	) throw new Error('The app action contract was invalid.');
	const fields: Record<string, AppActionInputField> = {};
	for (const [name, rawField] of Object.entries(rawFields)) {
		const field = parseActionInputField(rawField);
		if (!asciiToken(name, 64, '_-') || !field) {
			throw new Error('The app action contract was invalid.');
		}
		fields[name] = field;
	}
	return { action_id: actionId, input: { type: 'object', fields } };
}

const ACTION_ERROR_DISPOSITIONS = new Set<AppActionErrorDisposition>([
	'terminal', 'retry_same_input', 'refresh_and_retry', 'reauthorize',
	'user_action_required', 'outcome_uncertain'
]);
const ACTION_ERROR_CODES = new Set([
	'invalid_request', 'not_authorized', 'not_found', 'conflict', 'stale_revision',
	'schema_mismatch', 'policy_denied', 'resource_exhausted', 'rate_limited',
	'unavailable', 'timeout', 'canceled', 'external_outcome_uncertain', 'internal'
]);
const ACTION_SOURCE_KINDS = new Set([
	'entity_record', 'entity_field', 'artifact', 'external_receipt', 'mutation_receipt'
]);
const ACTION_CLASSIFICATIONS = new Set(['public', 'ordinary', 'personal', 'sensitive', 'secret']);
const ACTION_MODEL_PROCESSING = new Set(['none', 'local_only', 'remote_allowed']);

function parseReferenceList(value: unknown): string[] | null {
	if (!Array.isArray(value) || value.length > 256) return null;
	if (value.some((entry) => !appReference(entry))) return null;
	const refs = value as string[];
	return new Set(refs).size === refs.length ? refs : null;
}

function parseActionOutputEnvelope(value: unknown): AppActionOutputEnvelope | null {
	const envelope = record(value);
	if (!envelope || !exactKeys(
		envelope,
		[
			'protocol_version', 'source', 'scope_binding_ref', 'installation_id',
			'package_revision_ref', 'schema_revision', 'grant_revision', 'value_schema_ref',
			'value', 'source_refs', 'handling_labels', 'content_digest', 'produced_at'
		],
		['expires_at']
	) || envelope.protocol_version !== '1' || envelope.source !== 'app_action' ||
		!string(envelope.scope_binding_ref, 192) || !string(envelope.installation_id, 128) ||
		!string(envelope.package_revision_ref, 192) || positiveInteger(envelope.schema_revision) === 0 ||
		positiveInteger(envelope.grant_revision) === 0 || !string(envelope.value_schema_ref, 192) ||
		!string(envelope.content_digest, 96) || !string(envelope.produced_at, 64) ||
		(envelope.expires_at !== undefined && !string(envelope.expires_at, 64)) ||
		!Array.isArray(envelope.source_refs) || envelope.source_refs.length > 256
	) return null;
	const sourceRefs: AppActionSourceRef[] = [];
	for (const rawSource of envelope.source_refs) {
		const source = record(rawSource);
		if (!source || !exactKeys(source, ['kind', 'reference'], ['revision', 'fields']) ||
			!ACTION_SOURCE_KINDS.has(String(source.kind)) || !string(source.reference, 192) ||
			(source.revision !== undefined && positiveInteger(source.revision) === 0) ||
			(source.fields !== undefined && (!Array.isArray(source.fields) || source.fields.length > 64 ||
				source.fields.some((field) => !string(field, 256))))
		) return null;
		sourceRefs.push({
			kind: source.kind as AppActionSourceRef['kind'],
			reference: source.reference,
			...(source.revision !== undefined ? { revision: source.revision as number } : {}),
			fields: (source.fields as string[] | undefined) ?? []
		});
	}
	const labels = record(envelope.handling_labels);
	if (!labels || !exactKeys(labels, [
		'classification', 'model_processing', 'policy_digest', 'provenance_digest'
	]) || !ACTION_CLASSIFICATIONS.has(String(labels.classification)) ||
		!ACTION_MODEL_PROCESSING.has(String(labels.model_processing)) ||
		!string(labels.policy_digest, 96) || !string(labels.provenance_digest, 96)
	) return null;
	return {
		protocol_version: '1',
		source: 'app_action',
		scope_binding_ref: envelope.scope_binding_ref,
		installation_id: envelope.installation_id,
		package_revision_ref: envelope.package_revision_ref,
		schema_revision: envelope.schema_revision as number,
		grant_revision: envelope.grant_revision as number,
		value_schema_ref: envelope.value_schema_ref,
		value: envelope.value,
		source_refs: sourceRefs,
		handling_labels: {
			classification: labels.classification as AppActionOutputEnvelope['handling_labels']['classification'],
			model_processing: labels.model_processing as AppActionOutputEnvelope['handling_labels']['model_processing'],
			policy_digest: labels.policy_digest,
			provenance_digest: labels.provenance_digest
		},
		content_digest: envelope.content_digest,
		produced_at: envelope.produced_at,
		...(envelope.expires_at !== undefined ? { expires_at: envelope.expires_at as string } : {})
	};
}

function parseActionResult(value: unknown, expectedInstallationId?: string): AppActionResult | null {
	const result = record(value);
	if (
		!result || !exactKeys(
			result,
			['protocol_version', 'action_id', 'run_ref', 'status'],
			['output', 'mutation_receipt_refs', 'external_effect_receipt_refs', 'error']
		) || result.protocol_version !== '1' || !asciiToken(result.action_id, 64, '_-') ||
		!string(result.run_ref, 256) ||
		!['completed', 'waiting', 'failed', 'uncertain'].includes(String(result.status))
	) return null;
	let output: AppActionOutputEnvelope | undefined;
	if (result.output !== undefined) {
		output = parseActionOutputEnvelope(result.output) ?? undefined;
		if (!output ||
			(expectedInstallationId !== undefined && output.installation_id !== expectedInstallationId)) return null;
	}
	const mutationReceipts = parseReferenceList(result.mutation_receipt_refs ?? []);
	const externalReceipts = parseReferenceList(result.external_effect_receipt_refs ?? []);
	if (!mutationReceipts || !externalReceipts) return null;
	let parsedError: AppActionError | undefined;
	if (result.error !== undefined) {
		const error = record(result.error);
		if (!error || !exactKeys(error, ['code', 'disposition', 'message'], ['details', 'retry_after_ms']) ||
			!ACTION_ERROR_CODES.has(String(error.code)) ||
			!ACTION_ERROR_DISPOSITIONS.has(error.disposition as AppActionErrorDisposition) ||
			!string(error.message, 4096) || (error.details !== undefined && !record(error.details)) ||
			(error.retry_after_ms !== undefined && positiveInteger(error.retry_after_ms) === 0) ||
			((error.code === 'external_outcome_uncertain') !== (error.disposition === 'outcome_uncertain'))
		) return null;
		const details = (error.details as Record<string, unknown> | undefined) ?? {};
		if (Object.keys(details).length > 256) return null;
		parsedError = {
			code: error.code as string,
			disposition: error.disposition as AppActionErrorDisposition,
			message: error.message,
			details,
			...(error.retry_after_ms !== undefined ? { retry_after_ms: error.retry_after_ms as number } : {})
		};
	}
	const status = result.status as AppActionResult['status'];
	if (
		(status === 'completed' && (parsedError !== undefined ||
			(output === undefined && mutationReceipts.length === 0 && externalReceipts.length === 0))) ||
		(status === 'waiting' && (output !== undefined || parsedError !== undefined ||
			mutationReceipts.length > 0 || externalReceipts.length > 0)) ||
		(status === 'failed' && (output !== undefined || parsedError === undefined ||
			mutationReceipts.length > 0 || externalReceipts.length > 0)) ||
		(status === 'uncertain' && (parsedError?.disposition !== 'outcome_uncertain' ||
			externalReceipts.length === 0))
	) return null;
	return {
		action_id: result.action_id as string,
		run_ref: result.run_ref as string,
		status,
		...(result.output !== undefined ? { output } : {}),
		mutation_receipt_refs: mutationReceipts,
		external_effect_receipt_refs: externalReceipts,
		...(parsedError !== undefined ? { error: parsedError } : {})
	};
}

function parseRunHandle(value: unknown): AppRunHandle | null {
	const handle = record(value);
	if (
		!handle || !exactKeys(
			handle,
			['protocol_version', 'run_ref', 'installation_id', 'action_id']
		) || handle.protocol_version !== '1' || !string(handle.run_ref, 256) ||
		!handle.run_ref.startsWith('run:app-action:') || !string(handle.installation_id, 128) ||
		handle.installation_id.length === 0 || !asciiToken(handle.action_id, 64, '_-')
	) return null;
	return {
		protocol_version: '1',
		run_ref: handle.run_ref,
		installation_id: handle.installation_id,
		action_id: handle.action_id
	};
}

function parseRunStatus(value: unknown): AppRunStatus | null {
	return typeof value === 'string' && [
		'queued', 'planning', 'running', 'paused', 'deferred', 'waiting', 'blocked', 'cancelling',
		'completed', 'failed', 'cancelled', 'archived', 'uncertain'
	].includes(value) ? value as AppRunStatus : null;
}

export function appRunStatusIsTerminal(status: AppRunStatus): boolean {
	return ['completed', 'failed', 'cancelled', 'archived', 'uncertain'].includes(status);
}

function parseStorage(value: unknown): AppDirectoryStorageSummary | null {
	const item = record(value);
	if (
		!item ||
		!exactKeys(item, ['record_count', 'revision_count', 'payload_bytes', 'attachment_bytes']) ||
		!count(item.record_count) ||
		!count(item.revision_count) ||
		!count(item.payload_bytes) ||
		!count(item.attachment_bytes)
	) return null;
	return {
		record_count: item.record_count,
		revision_count: item.revision_count,
		payload_bytes: item.payload_bytes,
		attachment_bytes: item.attachment_bytes
	};
}

function parsePermissions(value: unknown): AppDirectoryPermissionSummary | undefined | null {
	if (value === undefined) return undefined;
	const item = record(value);
	if (
		!item ||
		!exactKeys(item, [
			'granted_tools',
			'granted_context_reads',
			'granted_personal_data_projections',
			'background_execution',
			'network_access'
		]) ||
		!count(item.granted_tools) ||
		!count(item.granted_context_reads) ||
		!count(item.granted_personal_data_projections) ||
		typeof item.background_execution !== 'boolean' ||
		typeof item.network_access !== 'boolean'
	) return null;
	return {
		granted_tools: item.granted_tools,
		granted_context_reads: item.granted_context_reads,
		granted_personal_data_projections: item.granted_personal_data_projections,
		background_execution: item.background_execution,
		network_access: item.network_access
	};
}

function parseEntry(value: unknown): AppDirectoryEntry | null {
	const item = record(value);
	if (!item || !exactKeys(item, [
		'installation_id', 'name', 'description', 'icon', 'package_version',
		'package_revision_ref', 'installation_generation', 'status', 'views', 'actions',
		'storage', 'record_count', 'payload_bytes'
	], ['default_route', 'last_opened_at', 'attention_reason', 'permissions', 'custom_surface_entry_count', 'navigation'])) return null;
	const icon = record(item.icon);
	const views = Array.isArray(item.views) ? item.views.map(parseView) : [];
	const actions = Array.isArray(item.actions) ? item.actions.map(parseAction) : [];
	const storage = parseStorage(item.storage);
	const permissions = parsePermissions(item.permissions);
	const customSurfaceEntryCount = item.custom_surface_entry_count === undefined
		? 0
		: item.custom_surface_entry_count;
	// Off-shape navigation refuses the whole entry rather than mounting a
	// partial list: a half-decoded declaration is a contract mismatch, and the
	// shell must not guess which half was the real one.
	const navigation = parseAppNavigationEntries(item.navigation);
	if (
		!asciiToken(item.installation_id, 128, '_-.') || !string(item.name, 256) || item.name.length === 0 ||
		!string(item.description, 2048) || !icon ||
		!exactKeys(icon, ['kind', 'value']) || icon.kind !== 'monogram' || !string(icon.value, 8) || icon.value.length === 0 ||
		!string(item.package_version, 128) || item.package_version.length === 0 ||
		!asciiToken(item.package_revision_ref, 192, '_-.:/@#') ||
		!count(item.installation_generation) || item.installation_generation === 0 || typeof item.status !== 'string' ||
		!STATUSES.has(item.status as AppDirectoryEntry['status']) ||
		!optionalString(item.default_route, 512) || !optionalString(item.last_opened_at, 64) ||
		!optionalString(item.attention_reason, 512) || views.some((view) => view === null) ||
		actions.some((action) => action === null) || views.length > MAX_DIRECTORY_VIEWS_PER_APP ||
		actions.length > MAX_DIRECTORY_ACTIONS_PER_APP || !storage || permissions === null ||
		!count(item.record_count) || !count(item.payload_bytes) ||
		item.record_count !== storage.record_count || item.payload_bytes !== storage.payload_bytes ||
		!count(customSurfaceEntryCount) || navigation === null
	) return null;
	const parsedViews = views as AppDirectoryView[];
	const parsedActions = actions as AppDirectoryAction[];
	if (
		new Set(parsedViews.map((view) => view.view_id)).size !== parsedViews.length ||
		new Set(parsedViews.map((view) => view.route)).size !== parsedViews.length ||
		new Set(parsedActions.map((action) => action.action_id)).size !== parsedActions.length ||
		parsedViews.some((view) => !asciiToken(view.view_id, 64, '_-') || !canonicalAppRoute(view.route, item.installation_id as string)) ||
		parsedActions.some((action) => !asciiToken(action.action_id, 64, '_-')) ||
		(item.default_route !== undefined && !parsedViews.some((view) => view.route === item.default_route))
	) return null;
	return {
		installation_id: item.installation_id,
		name: item.name,
		description: item.description,
		icon: { kind: 'monogram', value: icon.value },
		package_version: item.package_version,
		package_revision_ref: item.package_revision_ref,
		installation_generation: item.installation_generation,
		status: item.status as AppDirectoryEntry['status'],
		default_route: item.default_route,
		views: parsedViews,
		actions: parsedActions,
		custom_surface_entry_count: customSurfaceEntryCount,
		navigation,
		last_opened_at: item.last_opened_at,
		attention_reason: item.attention_reason,
		permissions,
		storage,
		record_count: item.record_count,
		payload_bytes: item.payload_bytes
	};
}

export function parseAppDirectoryPage(value: unknown): AppDirectoryPage {
	const page = record(value);
	if (!page || !exactKeys(page, ['entries', 'has_more'], ['next_cursor']) ||
		!Array.isArray(page.entries) || page.entries.length > 100 ||
		typeof page.has_more !== 'boolean' || !optionalString(page.next_cursor, 512)) {
		throw new Error('The Apps directory returned an invalid response.');
	}
	const entries = page.entries.map(parseEntry);
	if (
		entries.some((entry) => entry === null) ||
		new Set((entries as AppDirectoryEntry[]).map((entry) => entry.installation_id)).size !== entries.length ||
		page.has_more !== Boolean(page.next_cursor)
	) {
		throw new Error('The Apps directory returned an invalid response.');
	}
	return { entries: entries as AppDirectoryEntry[], has_more: page.has_more, next_cursor: page.next_cursor };
}

function assertActivityReceipt(value: unknown, installationId: string): void {
	const receipt = record(value);
	if (!receipt || !exactKeys(receipt, ['installation_id', 'updated_at']) ||
		receipt.installation_id !== installationId || !string(receipt.updated_at, 64)) {
		throw new Error('The Apps directory returned an invalid activity receipt.');
	}
}

export async function fetchAppDirectory(options: {
	section: AppDirectorySection;
	pinnedTargetKind?: AppDirectoryPinnedTargetKind;
	search?: string;
	limit: number;
	cursor?: string;
	signal?: AbortSignal;
}): Promise<AppDirectoryPage> {
	if (!Number.isSafeInteger(options.limit) || options.limit < 1 || options.limit > 100) {
		throw new Error('The Apps directory page size is invalid.');
	}
	if (options.pinnedTargetKind && options.section !== 'pinned') {
		throw new Error('A pinned target filter is valid only for pinned apps.');
	}
	const normalizedSearch = options.search?.trim() ?? '';
	if (new TextEncoder().encode(normalizedSearch).byteLength > 128 || /[\u0000-\u001f\u007f]/.test(normalizedSearch)) {
		throw new Error('The Apps directory search is invalid.');
	}
	if (options.cursor !== undefined && (options.cursor.length === 0 || options.cursor.length > 512)) {
		throw new Error('The Apps directory cursor is invalid.');
	}
	const query = new URLSearchParams({
		section: options.section,
		limit: String(options.limit)
	});
	if (options.pinnedTargetKind) query.set('pinned_target_kind', options.pinnedTargetKind);
	if (normalizedSearch) query.set('search', normalizedSearch);
	if (options.cursor) query.set('cursor', options.cursor);
	const response = await fetch(`/api/magician/v2/apps/directory?${query}`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		signal: options.signal
	});
	const body = await responseJson(response);
	if (!response.ok) throw responseError(response, body, 'The Apps directory could not be loaded.');
	return parseAppDirectoryPage(body);
}

export async function recordAppDirectoryLaunch(
	installationId: string,
	viewId: string,
	signal?: AbortSignal
): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/directory-activity`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'Content-Type': 'application/json'
			}),
			body: JSON.stringify({ kind: 'opened', view_id: viewId }),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) throw responseError(response, body, 'The app launch could not be recorded.');
	assertActivityReceipt(body, installationId);
}

export async function setAppDirectoryPin(
	installationId: string,
	viewId: string,
	pinned: boolean,
	signal?: AbortSignal
): Promise<void> {
	return setAppDirectoryTargetPin(installationId, 'view', viewId, pinned, signal);
}

export async function setAppDirectoryActionPin(
	installationId: string,
	actionId: string,
	pinned: boolean,
	signal?: AbortSignal
): Promise<void> {
	return setAppDirectoryTargetPin(installationId, 'action', actionId, pinned, signal);
}

async function setAppDirectoryTargetPin(
	installationId: string,
	targetKind: AppDirectoryPinnedTargetKind,
	targetId: string,
	pinned: boolean,
	signal?: AbortSignal
): Promise<void> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/directory-activity`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({
				Accept: 'application/json',
				'Content-Type': 'application/json'
			}),
			body: JSON.stringify({ kind: 'pin', target_kind: targetKind, target_id: targetId, pinned }),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) throw responseError(response, body, 'The app pin could not be updated.');
	assertActivityReceipt(body, installationId);
}

export async function fetchAppActionContract(
	installationId: string,
	actionId: string,
	signal?: AbortSignal
): Promise<AppDirectActionContract> {
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/actions/${encodeURIComponent(actionId)}/contract`,
		{
			headers: scopedRequestHeaders({ Accept: 'application/json' }),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) throw responseError(response, body, 'The app action contract could not be loaded.');
	return parseDirectActionContract(body, actionId);
}

export async function launchAppAction(
	installationId: string,
	actionId: string,
	idempotencyKey: string,
	input: Record<string, unknown>,
	signal?: AbortSignal,
	expectedInstallationBinding?: AppDirectActionExpectedInstallationBinding
): Promise<AppActionLaunch> {
	if (expectedInstallationBinding !== undefined && (
		!count(expectedInstallationBinding.generation) || expectedInstallationBinding.generation === 0 ||
		!appReference(expectedInstallationBinding.package_revision_ref)
	)) {
		throw new Error('The expected app installation binding is invalid.');
	}
	const response = await fetch(
		`/api/magician/v2/apps/installations/${encodeURIComponent(installationId)}/actions/${encodeURIComponent(actionId)}/runs`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({
				idempotency_key: idempotencyKey,
				input,
				...(expectedInstallationBinding
					? { expected_installation_binding: expectedInstallationBinding }
					: {})
			}),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) throw responseError(response, body, 'The app action could not be launched.');
	const launch = record(body);
	const runHandle = launch ? parseRunHandle(launch.run_handle) : null;
	if (
		!launch || !exactKeys(launch, ['run_handle'], ['execution_id', 'result']) ||
		!runHandle || runHandle.installation_id !== installationId ||
		runHandle.action_id !== actionId ||
		(launch.execution_id !== undefined && (
			typeof launch.execution_id !== 'string' || launch.execution_id.length === 0 ||
			new TextEncoder().encode(launch.execution_id).length > 192 || /[\u0000-\u001f\u007f]/.test(launch.execution_id)
		))
	) throw new Error('The app action returned an invalid launch receipt.');
	const result = launch.result === undefined
		? undefined
		: parseActionResult(launch.result, installationId);
	if (
		launch.result !== undefined && (!result || result.run_ref !== runHandle.run_ref ||
			result.action_id !== runHandle.action_id)
	) {
		throw new Error('The app action returned an invalid result.');
	}
	return {
		run_handle: runHandle,
		...(result ? { result } : {})
	};
}

export async function fetchAppActionRun(runRef: string, signal?: AbortSignal): Promise<AppActionRun> {
	const response = await fetch(`/api/magician/v2/apps/action-runs/${encodeURIComponent(runRef)}`, {
		headers: scopedRequestHeaders({ Accept: 'application/json' }),
		signal
	});
	let body: unknown;
	try {
		body = await responseJson(response);
	} catch (error) {
		// Preserve HTTP retry semantics even when an upstream 5xx/429 body is
		// truncated or not JSON. A malformed successful contract remains a
		// permanent parser failure.
		if (!response.ok) {
			throw responseError(response, null, 'The app action status could not be loaded.');
		}
		throw error;
	}
	if (!response.ok) throw responseError(response, body, 'The app action status could not be loaded.');
	const actionResult = parseActionResult(body);
	if (actionResult) {
		if (actionResult.run_ref !== runRef) {
			throw new Error('The app action returned a result for another run.');
		}
		return {
			run_ref: runRef,
			status: actionResult.status,
			terminal: actionResult.status !== 'waiting',
			result_withheld: false,
			result: actionResult
		};
	}
	const snapshot = record(body);
	const runHandle = snapshot ? parseRunHandle(snapshot.run_handle) : null;
	const status = snapshot ? parseRunStatus(snapshot.status) : null;
	if (
		!snapshot || !exactKeys(
			snapshot,
			['protocol_version', 'run_handle', 'status', 'terminal', 'result_withheld'],
			['execution_id', 'cancellation_generation', 'result']
		) || snapshot.protocol_version !== '1' ||
		!runHandle || runHandle.run_ref !== runRef ||
		!status || typeof snapshot.terminal !== 'boolean' ||
		typeof snapshot.result_withheld !== 'boolean' ||
		snapshot.terminal !== appRunStatusIsTerminal(status) ||
		!optionalString(snapshot.execution_id, 256) ||
		(snapshot.cancellation_generation !== undefined &&
			(!count(snapshot.cancellation_generation) || snapshot.cancellation_generation === 0)) ||
		response.status !== (snapshot.terminal ? 200 : 202)
	) throw new Error('The app action returned an invalid status response.');
	const result = snapshot.result === undefined
		? undefined
		: parseActionResult(snapshot.result, runHandle.installation_id);
	if (
		snapshot.result !== undefined && (!result || result.run_ref !== runRef ||
			result.action_id !== runHandle.action_id || result.status !== status)
	) throw new Error('The app action returned an invalid status result.');
	if (
		(snapshot.result_withheld && (status !== 'completed' || result !== undefined)) ||
		(status === 'completed' && result === undefined && !snapshot.result_withheld)
	) throw new Error('The app action returned an invalid result-disclosure state.');
	return {
		run_handle: runHandle,
		run_ref: runRef,
		execution_id: snapshot.execution_id as string | undefined,
		status,
		terminal: snapshot.terminal,
		...(snapshot.cancellation_generation === undefined
			? {}
			: { cancellation_generation: snapshot.cancellation_generation as number }),
		result_withheld: snapshot.result_withheld,
		...(result ? { result } : {})
	};
}

export interface AppActionCancellationReceipt {
	protocol_version: '1';
	run_ref: string;
	generation: number;
	idempotency_key: string;
	status: 'cancelling' | 'cancelled';
	requested_at: string;
}

export async function cancelAppActionRun(
	runRef: string,
	expectedGeneration: number,
	idempotencyKey: string,
	signal?: AbortSignal
): Promise<AppActionCancellationReceipt> {
	if (!appReference(runRef) || !Number.isSafeInteger(expectedGeneration) || expectedGeneration < 0 ||
		!appReference(idempotencyKey)) {
		throw new Error('The app action cancellation identity is invalid.');
	}
	const response = await fetch(
		`/api/magician/v2/apps/action-runs/${encodeURIComponent(runRef)}/cancel`,
		{
			method: 'POST',
			headers: scopedRequestHeaders({ Accept: 'application/json', 'Content-Type': 'application/json' }),
			body: JSON.stringify({ expected_generation: expectedGeneration, idempotency_key: idempotencyKey }),
			signal
		}
	);
	const body = await responseJson(response);
	if (!response.ok) throw responseError(response, body, 'The app action cancellation could not be requested.');
	const receipt = record(body);
	if (!receipt || !exactKeys(receipt, [
		'protocol_version', 'run_ref', 'generation', 'idempotency_key', 'status', 'requested_at'
	]) || receipt.protocol_version !== '1' || receipt.run_ref !== runRef ||
		receipt.idempotency_key !== idempotencyKey || !count(receipt.generation) || receipt.generation === 0 ||
		(receipt.status !== 'cancelling' && receipt.status !== 'cancelled') || !string(receipt.requested_at, 64) ||
		response.status !== (receipt.status === 'cancelled' ? 200 : 202)) {
		throw new Error('The app action returned an invalid cancellation receipt.');
	}
	return receipt as unknown as AppActionCancellationReceipt;
}
