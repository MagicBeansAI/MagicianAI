export type CompleteResultEntryKind = 'complete_value' | 'container' | 'string_fragment';

export interface CompleteResultStringFragment {
	byte_start: number;
	byte_end: number;
	total_bytes: number;
}

export interface CompleteResultEntry {
	field_path: string;
	source_index?: number;
	reconstruction_path?: string;
	kind?: CompleteResultEntryKind;
	string_fragment?: CompleteResultStringFragment;
	value: unknown;
}

/** Canonical lifecycle owner emitted with `tool.result.projected`.
 *
 * Activity rows may also carry a task id for navigation/inspection. That is
 * deliberately separate: a Chat-owned `delegate_to_agent` result can be
 * associated with a child task without becoming owned by that task.
 */
export type CompleteResultOwner =
	| { kind: 'chat'; sessionId: string }
	| { kind: 'task'; taskId: string; executionId: string | null }
	| { kind: 'ephemeral_voice'; voiceSessionId: string };

export interface CompleteResultReadTarget {
	path: string;
	executionId: string | null;
}

function nonEmptyString(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value : null;
}

export function parseCompleteResultOwner(value: unknown): CompleteResultOwner | null {
	if (value == null || typeof value !== 'object' || Array.isArray(value)) return null;
	const wire = value as Record<string, unknown>;
	switch (wire.kind) {
		case 'chat': {
			const sessionId = nonEmptyString(wire.session_id);
			return sessionId ? { kind: 'chat', sessionId } : null;
		}
		case 'task': {
			const taskId = nonEmptyString(wire.task_id);
			return taskId
				? { kind: 'task', taskId, executionId: nonEmptyString(wire.execution_id) }
				: null;
		}
		case 'ephemeral_voice': {
			const voiceSessionId = nonEmptyString(wire.voice_session_id);
			return voiceSessionId ? { kind: 'ephemeral_voice', voiceSessionId } : null;
		}
		default:
			return null;
	}
}

export function resolveCompleteResultReadTarget(input: {
	owner: CompleteResultOwner | null | undefined;
	hostSessionId: string | null | undefined;
	legacyTaskId?: string | null;
	legacyExecutionId?: string | null;
}): CompleteResultReadTarget | null {
	const { owner } = input;
	if (owner?.kind === 'chat') {
		return {
			path: `/api/magician/v2/chat/sessions/${encodeURIComponent(owner.sessionId)}/results/read`,
			executionId: null,
		};
	}
	if (owner?.kind === 'task') {
		return {
			path: `/api/magician/v3/tasks/${encodeURIComponent(owner.taskId)}/results/read`,
			executionId: owner.executionId,
		};
	}
	if (owner?.kind === 'ephemeral_voice') return null;

	// Compatibility for events persisted before canonical owner metadata was
	// added. New events never infer ownership from these navigation fields.
	const legacyTaskId = nonEmptyString(input.legacyTaskId);
	if (legacyTaskId) {
		return {
			path: `/api/magician/v3/tasks/${encodeURIComponent(legacyTaskId)}/results/read`,
			executionId: nonEmptyString(input.legacyExecutionId),
		};
	}
	const hostSessionId = nonEmptyString(input.hostSessionId);
	return hostSessionId
		? {
				path: `/api/magician/v2/chat/sessions/${encodeURIComponent(hostSessionId)}/results/read`,
				executionId: null,
			}
		: null;
}

interface FragmentAssembly {
	nextByte: number;
	totalBytes: number;
	text: string;
}

function decodePointer(path: string): string[] {
	if (path === '') return [];
	if (!path.startsWith('/')) throw new Error('Complete result contained an invalid reconstruction path.');
	const encodedTokens = path
		.slice(1)
		.split('/');
	if (encodedTokens.some((token) => /~(?:[^01]|$)/.test(token))) {
		throw new Error('Complete result contained an invalid JSON-pointer escape.');
	}
	return encodedTokens.map((token) => token.replace(/~1/g, '/').replace(/~0/g, '~'));
}

function arrayIndex(token: string): number | null {
	if (!/^(0|[1-9][0-9]*)$/.test(token)) return null;
	const parsed = Number(token);
	return Number.isSafeInteger(parsed) ? parsed : null;
}

function objectOwnValue(object: Record<string, unknown>, key: string): unknown {
	return Object.prototype.hasOwnProperty.call(object, key) ? object[key] : undefined;
}

function setObjectOwnValue(object: Record<string, unknown>, key: string, value: unknown): void {
	// Tool results are untrusted data. Assignment through `object[key]` would
	// invoke Object.prototype.__proto__ for a perfectly valid JSON key and let a
	// result page mutate the viewer object's prototype. Define an enumerable own
	// data property instead so every JSON key remains inert data.
	Object.defineProperty(object, key, {
		value,
		writable: true,
		enumerable: true,
		configurable: true,
	});
}

function setAtPointer(currentRoot: unknown, path: string, value: unknown): unknown {
	const tokens = decodePointer(path);
	if (tokens.length === 0) return value;
	let root = currentRoot;
	if (root === undefined) root = arrayIndex(tokens[0]) == null ? {} : [];
	if (root === null || typeof root !== 'object') {
		throw new Error('Complete result reconstruction tried to descend through a scalar.');
	}

	let parent = root as Record<string, unknown> | unknown[];
	for (let index = 0; index < tokens.length - 1; index += 1) {
		const token = tokens[index];
		const nextToken = tokens[index + 1];
		let child: unknown;
		if (Array.isArray(parent)) {
			const key = arrayIndex(token);
			if (key == null) {
				throw new Error('Complete result contained a non-numeric array path.');
			}
			if (key > parent.length) {
				throw new Error('Complete result array entries were missing or out of order.');
			}
			child = parent[key];
			if (child === undefined) {
				child = arrayIndex(nextToken) == null ? {} : [];
				parent[key] = child;
			}
		} else {
			child = objectOwnValue(parent, token);
			if (child === undefined) {
				child = arrayIndex(nextToken) == null ? {} : [];
				setObjectOwnValue(parent, token, child);
			}
		}
		if (child === null || typeof child !== 'object') {
			throw new Error('Complete result reconstruction contained overlapping scalar paths.');
		}
		parent = child as Record<string, unknown> | unknown[];
	}
	const finalToken = tokens[tokens.length - 1];
	if (Array.isArray(parent)) {
		const finalKey = arrayIndex(finalToken);
		if (finalKey == null) {
			throw new Error('Complete result contained a non-numeric array path.');
		}
		if (finalKey > parent.length) {
			throw new Error('Complete result array entries were missing or out of order.');
		}
		parent[finalKey] = value;
	} else {
		setObjectOwnValue(parent, finalToken, value);
	}
	return root;
}

function isTokenPrefix(prefix: string[], value: string[]): boolean {
	return prefix.length <= value.length && prefix.every((token, index) => token === value[index]);
}

function assertCompatiblePath(
	path: string,
	assignedPaths: Map<string, CompleteResultEntryKind>,
	fragmentPaths: Iterable<string>,
	continuingFragment = false
): void {
	const tokens = decodePointer(path);
	for (const [assignedPath, assignedKind] of assignedPaths) {
		const assignedTokens = decodePointer(assignedPath);
		if (assignedPath === path) {
			throw new Error('Complete result contained a duplicate reconstruction path.');
		}
		if (isTokenPrefix(assignedTokens, tokens) && assignedKind !== 'container') {
			throw new Error('Complete result contained overlapping scalar reconstruction paths.');
		}
		if (isTokenPrefix(tokens, assignedTokens)) {
			throw new Error('Complete result contained an out-of-order parent reconstruction path.');
		}
	}
	for (const fragmentPath of fragmentPaths) {
		if (fragmentPath === path && continuingFragment) continue;
		const fragmentTokens = decodePointer(fragmentPath);
		if (fragmentPath === path || isTokenPrefix(fragmentTokens, tokens) || isTokenPrefix(tokens, fragmentTokens)) {
			throw new Error('Complete result contained overlapping string-fragment reconstruction paths.');
		}
	}
}

/**
 * Reconstruct the server's versioned, lossless preorder result stream.
 * Version zero is the legacy complete-value-only representation. Version one
 * additionally carries typed container and UTF-8 string-fragment units.
 */
export function reconstructCompleteResult(
	entries: CompleteResultEntry[],
	reconstructionVersion: number
): unknown {
	if (reconstructionVersion !== 0 && reconstructionVersion !== 1) {
		throw new Error(`Unsupported complete-result reconstruction version: ${reconstructionVersion}`);
	}
	let root: unknown = undefined;
	const fragments = new Map<string, FragmentAssembly>();
	const assignedPaths = new Map<string, CompleteResultEntryKind>();

	for (const entry of entries) {
		const kind = entry.kind ?? 'complete_value';
		const path =
			entry.reconstruction_path ??
			(entry.source_index == null
				? entry.field_path
				: `${entry.field_path}/${entry.source_index}`);
		if (kind === 'string_fragment') {
			if (reconstructionVersion !== 1 || typeof entry.value !== 'string' || !entry.string_fragment) {
				throw new Error('Complete result contained an invalid string fragment.');
			}
			const metadata = entry.string_fragment;
			assertCompatiblePath(path, assignedPaths, fragments.keys(), fragments.has(path));
			const state = fragments.get(path) ?? {
				nextByte: 0,
				totalBytes: metadata.total_bytes,
				text: '',
			};
			const fragmentBytes = new TextEncoder().encode(entry.value).byteLength;
			if (
				metadata.total_bytes !== state.totalBytes ||
				metadata.byte_start !== state.nextByte ||
				metadata.byte_end !== metadata.byte_start + fragmentBytes ||
				metadata.byte_end > metadata.total_bytes
			) {
				throw new Error('Complete result string fragments were missing, duplicated, or out of order.');
			}
			state.text += entry.value;
			state.nextByte = metadata.byte_end;
			fragments.set(path, state);
			if (state.nextByte === state.totalBytes) {
				root = setAtPointer(root, path, state.text);
				fragments.delete(path);
				assignedPaths.set(path, 'string_fragment');
			}
			continue;
		}
		if (kind !== 'complete_value' && kind !== 'container') {
			throw new Error('Complete result contained an unknown entry kind.');
		}
		if (kind === 'container' && reconstructionVersion !== 1) {
			throw new Error('Legacy complete result unexpectedly contained a container entry.');
		}
		if (kind === 'container' && !Array.isArray(entry.value) && (entry.value === null || typeof entry.value !== 'object')) {
			throw new Error('Complete result container entry did not contain an object or array.');
		}
		if (
			kind === 'container' &&
			((Array.isArray(entry.value) && entry.value.length !== 0) ||
				(!Array.isArray(entry.value) && Object.keys(entry.value as Record<string, unknown>).length !== 0))
		) {
			throw new Error('Complete result container entry was not empty.');
		}
		assertCompatiblePath(path, assignedPaths, fragments.keys());
		root = setAtPointer(root, path, entry.value);
		assignedPaths.set(path, kind);
	}

	if (fragments.size > 0) {
		throw new Error('Complete result ended before all string fragments arrived.');
	}
	if (root === undefined) throw new Error('Complete result did not contain any values.');
	return root;
}
