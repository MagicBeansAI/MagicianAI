import { writable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';

export const MAX_STEER_MESSAGE_BYTES = 4 * 1024;

export type ExecutionControlAction = 'pause' | 'resume' | 'steer' | 'cancel';

export function executionControlTimeoutMs(action: ExecutionControlAction): number {
	return action === 'pause' ? 45_000 : 15_000;
}

export interface ExecutionControlState {
	execution_id: string;
	waiting_state: string;
	paused_from_state?: string | null;
	pause_kind?: string | null;
	active: boolean;
	can_pause: boolean;
	can_resume: boolean;
	can_steer: boolean;
	can_cancel: boolean;
}

interface ApiErrorPayload {
	code?: unknown;
	error?: unknown;
	message?: unknown;
	details?: unknown;
}

export class ExecutionControlApiError extends Error {
	constructor(
		message: string,
		public readonly status: number,
		public readonly code?: string,
		public readonly details?: unknown
	) {
		super(message);
		this.name = 'ExecutionControlApiError';
	}
}

async function readApiError(response: Response, fallback: string): Promise<ExecutionControlApiError> {
	let payload: ApiErrorPayload | null = null;
	try {
		payload = (await response.json()) as ApiErrorPayload;
	} catch {
		// The status and fallback still produce an actionable error.
	}
	const message =
		typeof payload?.error === 'string'
			? payload.error
			: typeof payload?.message === 'string'
				? payload.message
				: fallback;
	return new ExecutionControlApiError(
		message,
		response.status,
		typeof payload?.code === 'string' ? payload.code : undefined,
		payload?.details
	);
}

export async function requireExecutionControlResponse(
	response: Response,
	fallback: string
): Promise<Response> {
	if (!response.ok) throw await readApiError(response, fallback);
	return response;
}

function executionControlUrl(executionId: string, suffix: string): string {
	return `/api/magician/v2/executions/${encodeURIComponent(executionId)}/${suffix}`;
}

export async function getExecutionControlState(executionId: string): Promise<ExecutionControlState> {
	const response = await timedFetch(executionControlUrl(executionId, 'control-state'), {
		timeoutMs: 10_000
	});
	if (!response.ok) {
		throw await readApiError(response, 'Could not load execution controls.');
	}
	return (await response.json()) as ExecutionControlState;
}

export async function applyExecutionControl(
	executionId: string,
	action: ExecutionControlAction,
	message?: string
): Promise<void> {
	const targetExecutionId = executionId.trim();
	await coordinateExecutionControl(targetExecutionId, async () => {
		const response = await timedFetch(executionControlUrl(targetExecutionId, action), {
			method: 'POST',
			timeoutMs: executionControlTimeoutMs(action),
			headers: action === 'steer' ? { 'Content-Type': 'application/json' } : undefined,
			body: action === 'steer' ? JSON.stringify({ message: message ?? '' }) : undefined
		});
		await requireExecutionControlResponse(response, `Could not ${action} the run.`);
	});
}

const busyIds = new Set<string>();
const busyStore = writable<ReadonlySet<string>>(new Set());
const invalidationVersions = new Map<string, number>();
const invalidationStore = writable<ReadonlyMap<string, number>>(new Map());

export const executionControlBusy = { subscribe: busyStore.subscribe };
export const executionControlInvalidations = { subscribe: invalidationStore.subscribe };

export function beginExecutionControl(executionId: string): boolean {
	if (busyIds.has(executionId)) return false;
	busyIds.add(executionId);
	busyStore.set(new Set(busyIds));
	return true;
}

export function endExecutionControl(executionId: string): void {
	if (!busyIds.delete(executionId)) return;
	busyStore.set(new Set(busyIds));
}

export function invalidateExecutionControl(executionId: string): void {
	const nextVersion = (invalidationVersions.get(executionId) ?? 0) + 1;
	invalidationVersions.set(executionId, nextVersion);
	invalidationStore.set(new Map(invalidationVersions));
}

export async function coordinateExecutionControl<T>(
	executionId: string,
	mutation: () => Promise<T>
): Promise<T> {
	const targetExecutionId = executionId.trim();
	if (!targetExecutionId) {
		throw new ExecutionControlApiError('Execution id is required.', 400, 'invalid_execution_id');
	}
	if (!beginExecutionControl(targetExecutionId)) {
		throw new ExecutionControlApiError(
			'Another control action is already in progress for this run.',
			409,
			'execution_control_busy'
		);
	}
	try {
		return await mutation();
	} finally {
		invalidateExecutionControl(targetExecutionId);
		endExecutionControl(targetExecutionId);
	}
}

export function steerMessageByteLength(message: string): number {
	return new TextEncoder().encode(message).byteLength;
}
