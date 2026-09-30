import {
	appRunStatusIsTerminal,
	type AppActionRun,
	type AppRunHandle,
	type AppRunStatus
} from './appDirectory';

const STORAGE_KEY = 'magician.apps.action-runs.v1';
const LAUNCH_INTENT_STORAGE_KEY = 'magician.apps.action-launch-intents.v1';
const MAX_RUNS_PER_SCOPE = 16;
const MAX_TOTAL_RUNS = 64;
const MAX_LAUNCH_INTENTS = 8;
const MAX_AGE_MS = 7 * 24 * 60 * 60 * 1_000;
const MAX_STORED_BYTES = 64 * 1_024;
const MAX_INPUT_BYTES = 48 * 1_024;

export interface AppActionRunStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

export interface PersistedAppActionRun {
	scope_key: string;
	run_handle: AppRunHandle;
	status: AppRunStatus;
	terminal: boolean;
	result_withheld: boolean;
	updated_at_ms: number;
}

export interface PersistedAppActionLaunchIntent {
	schema: 'magician.apps.action-launch-intent.v1';
	scope_key: string;
	installation_id: string;
	installation_generation: number;
	package_revision_ref: string;
	action_id: string;
	idempotency_key: string;
	input_json: string;
	created_at_ms: number;
}

export function getAppActionRunStorage<T extends AppActionRunStorage = AppActionRunStorage>(
	globalObject: { readonly localStorage: T } | null | undefined
): T | null {
	if (!globalObject) return null;
	try {
		return globalObject.localStorage;
	} catch {
		// Privacy modes and embedded origins may reject even reading the
		// localStorage getter. Recovery is optional; launching is not.
		return null;
	}
}

/**
 * A launch response owns UI state only while both the monotonic submission
 * generation and the authenticated scope captured before dispatch still
 * match. Persistence uses the captured scope independently of this check.
 */
export function appActionSubmissionIsCurrent(
	currentGeneration: number,
	currentScopeKey: string,
	submissionGeneration: number,
	submissionScopeKey: string
): boolean {
	return currentGeneration === submissionGeneration && currentScopeKey === submissionScopeKey;
}

const RUN_STATUSES = new Set<AppRunStatus>([
	'queued', 'planning', 'running', 'paused', 'deferred', 'waiting', 'blocked',
	'cancelling', 'completed', 'failed', 'cancelled', 'archived', 'uncertain'
]);

function boundedString(value: unknown, max: number): value is string {
	return typeof value === 'string' && value.length > 0 && value.length <= max;
}

function parseHandle(value: unknown): AppRunHandle | null {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
	const handle = value as Record<string, unknown>;
	if (
		handle.protocol_version !== '1' ||
		!boundedString(handle.run_ref, 256) ||
		!handle.run_ref.startsWith('run:app-action:') ||
		!boundedString(handle.installation_id, 128) ||
		!boundedString(handle.action_id, 128)
	) return null;
	return {
		protocol_version: '1',
		run_ref: handle.run_ref,
		installation_id: handle.installation_id,
		action_id: handle.action_id
	};
}

function parseStoredRun(value: unknown, nowMs: number): PersistedAppActionRun | null {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
	const candidate = value as Record<string, unknown>;
	const runHandle = parseHandle(candidate.run_handle);
	const status = candidate.status as AppRunStatus;
	if (
		!runHandle || !boundedString(candidate.scope_key, 1_024) ||
		!RUN_STATUSES.has(status) ||
		typeof candidate.terminal !== 'boolean' ||
		typeof candidate.result_withheld !== 'boolean' ||
		candidate.terminal !== appRunStatusIsTerminal(status) ||
		(candidate.result_withheld && (status !== 'completed' || !candidate.terminal)) ||
		typeof candidate.updated_at_ms !== 'number' ||
		!Number.isSafeInteger(candidate.updated_at_ms) ||
		candidate.updated_at_ms <= 0 || candidate.updated_at_ms > nowMs + 60_000 ||
		nowMs - candidate.updated_at_ms > MAX_AGE_MS
	) return null;
	return {
		scope_key: candidate.scope_key,
		run_handle: runHandle,
		status,
		terminal: candidate.terminal,
		result_withheld: candidate.result_withheld,
		updated_at_ms: candidate.updated_at_ms
	};
}

function encodedByteLength(value: string): number {
	return new TextEncoder().encode(value).byteLength;
}

function boundRuns(entries: PersistedAppActionRun[]): PersistedAppActionRun[] {
	const perScope = new Map<string, number>();
	return [...entries]
		.sort((left, right) => right.updated_at_ms - left.updated_at_ms)
		.filter((entry) => {
			const count = perScope.get(entry.scope_key) ?? 0;
			if (count >= MAX_RUNS_PER_SCOPE) return false;
			perScope.set(entry.scope_key, count + 1);
			return true;
		})
		.slice(0, MAX_TOTAL_RUNS);
}

function readAll(storage: AppActionRunStorage, nowMs: number): PersistedAppActionRun[] {
	try {
		const raw = storage.getItem(STORAGE_KEY);
		if (!raw || raw.length > MAX_STORED_BYTES || encodedByteLength(raw) > MAX_STORED_BYTES) {
			return [];
		}
		const parsed: unknown = JSON.parse(raw);
		if (!Array.isArray(parsed)) return [];
		return boundRuns(parsed
			.map((entry) => parseStoredRun(entry, nowMs))
			.filter((entry): entry is PersistedAppActionRun => entry !== null));
	} catch {
		return [];
	}
}

function writeAll(storage: AppActionRunStorage, entries: PersistedAppActionRun[]): boolean {
	try {
		const retained = boundRuns(entries);
		let encoded = JSON.stringify(retained);
		while (retained.length > 0 && encodedByteLength(encoded) > MAX_STORED_BYTES) {
			// Entries are newest-first, so removing the tail always evicts
			// the oldest recovery hint rather than dropping the new launch.
			retained.pop();
			encoded = JSON.stringify(retained);
		}
		storage.setItem(STORAGE_KEY, encoded);
		return storage.getItem(STORAGE_KEY) === encoded;
	} catch {
		// Recovery history is a convenience only; storage failures must not affect launches.
		return false;
	}
}

export function loadAppActionRunHistory(
	storage: AppActionRunStorage,
	scopeKey: string,
	nowMs = Date.now()
): PersistedAppActionRun[] {
	const all = readAll(storage, nowMs);
	writeAll(storage, all);
	return all.filter((entry) => entry.scope_key === scopeKey).slice(0, MAX_RUNS_PER_SCOPE);
}

export function rememberAppActionRun(
	storage: AppActionRunStorage,
	scopeKey: string,
	run: AppActionRun,
	nowMs = Date.now()
): boolean {
	const runHandle = parseHandle(run.run_handle);
	if (!runHandle || runHandle.run_ref !== run.run_ref || !boundedString(scopeKey, 1_024)) {
		return false;
	}
	const next = parseStoredRun({
		scope_key: scopeKey,
		run_handle: runHandle,
		status: run.status,
		terminal: run.terminal,
		result_withheld: run.result_withheld,
		updated_at_ms: nowMs
	}, nowMs);
	if (!next) return false;
	const existing = readAll(storage, nowMs).filter((entry) =>
		entry.scope_key !== scopeKey || entry.run_handle.run_ref !== run.run_ref
	);
	return writeAll(storage, [next, ...existing]);
}

export function restoreAppActionRun(entry: PersistedAppActionRun): AppActionRun {
	return {
		run_handle: entry.run_handle,
		run_ref: entry.run_handle.run_ref,
		status: entry.status,
		terminal: entry.terminal,
		result_withheld: entry.result_withheld
	};
}

function canonicalInput(value: unknown): string {
	let nodes = 0;
	const visit = (candidate: unknown, depth: number): string => {
		if (depth > 16 || ++nodes > 512) throw new Error('The action input exceeded its local recovery bound.');
		if (candidate === null || typeof candidate === 'boolean') return JSON.stringify(candidate) as string;
		if (typeof candidate === 'number') {
			if (!Number.isFinite(candidate) || (Number.isInteger(candidate) && !Number.isSafeInteger(candidate))) {
				throw new Error('The action input contains an unsupported number.');
			}
			return JSON.stringify(candidate) as string;
		}
		if (typeof candidate === 'string') return JSON.stringify(candidate) as string;
		if (Array.isArray(candidate)) {
			if (candidate.length > 256 || Object.keys(candidate).length !== candidate.length) {
				throw new Error('The action input contains an unsupported collection.');
			}
			return `[${candidate.map((item) => visit(item, depth + 1)).join(',')}]`;
		}
		if (typeof candidate !== 'object' || Object.getPrototypeOf(candidate) !== Object.prototype) {
			throw new Error('The action input must contain only plain JSON values.');
		}
		const object = candidate as Record<string, unknown>;
		const keys = Object.keys(object).sort();
		if (keys.length > 256) throw new Error('The action input contains too many fields.');
		return `{${keys.map((key) => `${JSON.stringify(key) as string}:${visit(object[key], depth + 1)}`).join(',')}}`;
	};
	const encoded = visit(value, 0);
	if (encodedByteLength(encoded) > MAX_INPUT_BYTES) {
		throw new Error('The action input exceeded its local recovery byte bound.');
	}
	return encoded;
}

function parseLaunchIntent(value: unknown, nowMs: number): PersistedAppActionLaunchIntent | null {
	if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
	const item = value as Record<string, unknown>;
	const keys = Object.keys(item).sort();
	const expected = [
		'action_id', 'created_at_ms', 'idempotency_key', 'input_json', 'installation_generation',
		'installation_id', 'package_revision_ref', 'schema', 'scope_key'
	].sort();
	if (keys.length !== expected.length || keys.some((key, index) => key !== expected[index]) ||
		item.schema !== 'magician.apps.action-launch-intent.v1' ||
		!boundedString(item.scope_key, 1_024) || !boundedString(item.installation_id, 128) ||
		!Number.isSafeInteger(item.installation_generation) || (item.installation_generation as number) < 1 ||
		!boundedString(item.package_revision_ref, 192) || !boundedString(item.action_id, 64) ||
		!boundedString(item.idempotency_key, 192) || !boundedString(item.input_json, MAX_INPUT_BYTES) ||
		!Number.isSafeInteger(item.created_at_ms) || (item.created_at_ms as number) <= 0 ||
		(item.created_at_ms as number) > nowMs + 60_000 || nowMs - (item.created_at_ms as number) > MAX_AGE_MS
	) return null;
	try {
		const parsed: unknown = JSON.parse(item.input_json as string);
		if (canonicalInput(parsed) !== item.input_json) return null;
	} catch {
		return null;
	}
	return item as unknown as PersistedAppActionLaunchIntent;
}

function readLaunchIntents(storage: AppActionRunStorage, nowMs: number): PersistedAppActionLaunchIntent[] {
	try {
		const raw = storage.getItem(LAUNCH_INTENT_STORAGE_KEY);
		if (!raw || encodedByteLength(raw) > MAX_STORED_BYTES) return [];
		const parsed: unknown = JSON.parse(raw);
		if (!Array.isArray(parsed) || parsed.length > MAX_LAUNCH_INTENTS) return [];
		return parsed.map((item) => parseLaunchIntent(item, nowMs))
			.filter((item): item is PersistedAppActionLaunchIntent => item !== null);
	} catch {
		return [];
	}
}

function writeLaunchIntents(storage: AppActionRunStorage, intents: PersistedAppActionLaunchIntent[]): void {
	const encoded = JSON.stringify(intents.slice(0, MAX_LAUNCH_INTENTS));
	if (encodedByteLength(encoded) > MAX_STORED_BYTES) throw new Error('The durable action recovery store is full.');
	storage.setItem(LAUNCH_INTENT_STORAGE_KEY, encoded);
	if (storage.getItem(LAUNCH_INTENT_STORAGE_KEY) !== encoded) {
		throw new Error('The durable action recovery intent could not be verified.');
	}
}

export function stageAppActionLaunchIntent(
	storage: AppActionRunStorage,
	value: Omit<PersistedAppActionLaunchIntent, 'schema' | 'input_json' | 'created_at_ms'> & { input: Record<string, unknown> },
	nowMs = Date.now()
): PersistedAppActionLaunchIntent {
	const intent = parseLaunchIntent({
		schema: 'magician.apps.action-launch-intent.v1',
		scope_key: value.scope_key,
		installation_id: value.installation_id,
		installation_generation: value.installation_generation,
		package_revision_ref: value.package_revision_ref,
		action_id: value.action_id,
		idempotency_key: value.idempotency_key,
		input_json: canonicalInput(value.input),
		created_at_ms: nowMs
	}, nowMs);
	if (!intent) throw new Error('The action recovery intent is invalid.');
	const existing = readLaunchIntents(storage, nowMs);
	const conflicting = existing.find((item) => item.scope_key === intent.scope_key &&
		item.installation_id === intent.installation_id && item.action_id === intent.action_id);
	if (conflicting && conflicting.input_json !== intent.input_json) {
		throw new Error('Recover the previous action launch before submitting different input.');
	}
	const retained = existing.filter((item) => !(item.scope_key === intent.scope_key &&
		item.installation_id === intent.installation_id && item.action_id === intent.action_id));
	writeLaunchIntents(storage, [conflicting ?? intent, ...retained]);
	return conflicting ?? intent;
}

export function loadAppActionLaunchIntent(
	storage: AppActionRunStorage,
	scopeKey: string,
	installationId: string,
	actionId: string,
	nowMs = Date.now()
): PersistedAppActionLaunchIntent | null {
	return readLaunchIntents(storage, nowMs).find((item) => item.scope_key === scopeKey &&
		item.installation_id === installationId && item.action_id === actionId) ?? null;
}

export function launchIntentInput(intent: PersistedAppActionLaunchIntent): Record<string, unknown> {
	return JSON.parse(intent.input_json) as Record<string, unknown>;
}

export function clearAppActionLaunchIntent(
	storage: AppActionRunStorage,
	intent: PersistedAppActionLaunchIntent,
	nowMs = Date.now()
): void {
	const existing = readLaunchIntents(storage, nowMs);
	const retained = existing.filter((item) => !(
		item.scope_key === intent.scope_key && item.installation_id === intent.installation_id &&
		item.action_id === intent.action_id && item.idempotency_key === intent.idempotency_key &&
		item.input_json === intent.input_json
	));
	writeLaunchIntents(storage, retained);
}
