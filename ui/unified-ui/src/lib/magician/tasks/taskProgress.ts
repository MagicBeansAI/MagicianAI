import type { ExecutionPanelState } from '$lib/types/executionPanel';
import { executionIdOf } from './taskTimeline';

// Work starts, output and outcomes count. Polls, heartbeats, status reassertions,
// metrics and warnings alone must never keep a wedged run looking healthy.
const PROGRESS_EVENTS = new Set([
	'llm.requested', 'llm.first_token', 'llm.succeeded', 'llm.failed',
	'reasoning.start', 'reasoning.content', 'reasoning.end',
	'tool.started', 'tool.requested', 'tool.succeeded', 'tool.failed',
	'tool.call.started', 'tool.call.finished', 'tool.call.failed',
	'step.started', 'step.completed', 'step.failed', 'step.skipped', 'step.delegated',
	'child.spawned', 'child.completed', 'child.failed', 'child.output_available',
	'sub_goal.requested', 'sub_goal.outcome',
	'execution.started', 'execution.resumed',
	'LLMRequestSent', 'LLMResponseReceived', 'AgenticActionStarted', 'AgenticActionExecuted'
]);

export function isProgressEvent(type: string, metadata?: Record<string, unknown> | null): boolean {
	if (!PROGRESS_EVENTS.has(type)) return false;
	const settledModelCall = type === 'llm.succeeded' || type === 'llm.failed' || type === 'LLMResponseReceived';
	const attempts = metadata?.provider_attempt_count ?? record(metadata?.correlation)?.provider_attempt_count;
	return !(settledModelCall && attempts === 0 && metadata?.response_kind !== 'harness_aggregate');
}

/** Keep absence unknown and combine only real, positive progress instants. */
export function latestProgressAt(...instants: Array<number | null | undefined>): number | null {
	let latest: number | null = null;
	for (const at of instants) {
		if (typeof at === 'number' && Number.isFinite(at) && at > 0) latest = Math.max(latest ?? at, at);
	}
	return latest;
}

/** Progress of the current root and its children, never a history-picked run. */
export function executionProgressAt(state: ExecutionPanelState | null, executionId: string | null): number | null {
	if (!state || !executionId || executionIdOf(state) !== executionId) return null;
	let latest: number | null = null;
	for (const item of [...(state.run?.activity_log ?? []), ...(state.run?.recent_activity ?? [])]) {
		const metadata = record(item?.metadata);
		if (typeof metadata?.event_type === 'string' && isProgressEvent(metadata.event_type, metadata)) {
			latest = latestProgressAt(latest, item.created_at);
		}
	}
	// Older panels carry humanized event names in debug.timeline, without
	// metadata. Recognise the same explicit event vocabulary there.
	for (const entry of state.debug?.timeline ?? []) {
		const type = entry.title?.toLowerCase().replace(/[ _]+/g, '.');
		if (type && isProgressEvent(type)) latest = latestProgressAt(latest, entry.timestamp);
	}
	return latest;
}

export interface TaskProgressSignal {
	principal: string;
	workspace: string;
	taskId: string | null;
	executionId: string;
	at: number;
}

/** The shared socket's scoped work events; no client-clock fallback. */
export function taskProgressSignalOf(event: { event_type: string; data: unknown }): TaskProgressSignal | null {
	const data = record(event.data);
	const inner = event.event_type === 'AgentEvent' ? record(data?.event) : null;
	const payload = record(inner?.payload) ?? data;
	if (!payload) return null;
	const correlation = record(payload.correlation);
	const principal = payload.principal ?? inner?.principal ?? data?.principal;
	const workspace = payload.workspace ?? inner?.workspace ?? data?.workspace;
	const taskId = payload.task_id ?? correlation?.task_id;
	const executionId = payload.root_execution_id ?? correlation?.root_execution_id
		?? payload.execution_id ?? correlation?.execution_id;
	const state = event.event_type === 'ExecutionPanelDelta' ? payload.state as ExecutionPanelState : null;
	const at = state ? executionProgressAt(state, typeof executionId === 'string' ? executionId : null)
		: isProgressEvent(String(inner?.event_type ?? event.event_type), payload)
			? latestProgressAt(payload.timestamp_ms as number, payload.timestamp as number) : null;
	if (typeof principal !== 'string' || typeof workspace !== 'string' || typeof executionId !== 'string' || at === null) return null;
	return { principal, workspace, taskId: typeof taskId === 'string' ? taskId : null, executionId, at };
}

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;
}
