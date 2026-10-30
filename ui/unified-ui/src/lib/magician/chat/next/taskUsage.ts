// Model usage per task, for the chat's live task card: calls, tokens (in + out)
// and cost, for the run (with its delegated children) plus planning.
//
// Two sources, so the total survives a reload and covers planning:
// - live: task-attributed typed/enveloped responses and execution-panel
//   deltas from the shared chat event hub, counted once by model-call id;
// - snapshot: `/execution-panel` (`loadTaskUsageSnapshot`), the run's priced
//   calls from the same `deriveTimeline` the task panel shows, and the
//   planning activity's priced calls (`debug.planning_timeline`).
// Stored and live calls merge by call id: attaching mid-run must neither
// replace the stored baseline nor count replayed calls twice. Live panel
// deltas refresh the same history without another polling connection.

import { derived, writable, type Readable } from 'svelte/store';

import { readTaskRunState, type TaskRunStateResult } from '$lib/magician/tasks/taskAttention';
import { deriveTimeline, executionIdOf } from '$lib/magician/tasks/taskTimeline';
import type { ExecutionPanelState } from '$lib/types/executionPanel';
import { readTaskCallHistory, type CallUsage } from './taskCallHistory';

export interface TaskUsage {
	calls: number;
	tokens: number;
	tokensReported: boolean;
	/** `null` until a call reports a price. */
	costUsd: number | null;
}

/** The card's figure: run + planning, and the two parts for its tooltip. */
export interface TaskUsageTotal extends TaskUsage {
	run: TaskUsage | null;
	planning: TaskUsage | null;
	executionId: string | null;
	/** Full recorded run history, independent of the chat event tail. */
	activityCount: number | null;
}

interface Snapshot {
	run: TaskUsage | null;
	planning: TaskUsage | null;
	executionId: string | null;
	calls: Map<string, CallUsage>;
	activityIds: Set<string> | null;
	updatedAt: number;
}

const live = writable<Map<string, Map<string, CallUsage>>>(new Map());
const snapshots = writable<Map<string, Snapshot>>(new Map());
const loaded = new Set<string>();

/** Record a measured, finished model call, enriching replayed placeholders. */
export function recordTaskLlmCall(
	taskId: string,
	callKey: string,
	tokens: number | null,
	costUsd: number | null,
	executionId: string | null = null
): void {
	// Some control/judge events explicitly report no provider usage. Their
	// zero-valued placeholders are not measured calls.
	if (tokens === null && costUsd === null) return;
	live.update((map) => {
		const calls = new Map(map.get(taskId));
		const previous = calls.get(callKey);
		const next = {
			tokens: tokens ?? previous?.tokens ?? null,
			costUsd: costUsd ?? previous?.costUsd ?? null,
			executionId: executionId ?? previous?.executionId ?? null
		};
		if (previous && previous.tokens === next.tokens && previous.costUsd === next.costUsd &&
			previous.executionId === next.executionId) return map;
		calls.set(callKey, next);
		return new Map(map).set(taskId, calls);
	});
}

/** Apply a full panel read or the identical state carried by a live delta. */
export function recordTaskUsageSnapshot(
	taskId: string, state: ExecutionPanelState, callHistory?: Map<string, CallUsage> | null
): void {
	const incoming = usageSnapshotFrom(state);
	if (callHistory) {
		incoming.calls = mergeCalls(incoming.calls, callHistory);
		incoming.run = sumCalls(incoming.calls);
	}
	snapshots.update((map) => {
		const previous = map.get(taskId);
		if (previous && incoming.updatedAt < previous.updatedAt &&
			incoming.executionId !== previous.executionId) return map;
		if (previous?.executionId && previous.executionId === incoming.executionId) {
			incoming.calls = mergeCalls(previous.calls, incoming.calls);
			incoming.run = sumCalls(incoming.calls);
			if (previous.activityIds) {
				incoming.activityIds = new Set([...previous.activityIds, ...(incoming.activityIds ?? [])]);
			}
			incoming.updatedAt = Math.max(previous.updatedAt, incoming.updatedAt);
		}
		return new Map(map).set(taskId, incoming);
	});
}

/** The REST replay and live hub both carry these transport envelopes. */
export function recordTaskUsageEvent(event: Record<string, unknown>, fallbackTaskId?: string): void {
	const data = objectOrNull(event.data);
	if (!data) return;
	if (event.event_type === 'ExecutionPanelDelta') {
		const state = objectOrNull(data.state);
		if (typeof data.task_id === 'string' && state?.overview && state?.run) {
			recordTaskUsageSnapshot(data.task_id, data.state as ExecutionPanelState);
		}
		return;
	}
	const inner = event.event_type === 'AgentEvent' ? objectOrNull(data.event) : null;
	const eventType = inner?.event_type ?? event.event_type;
	if (eventType !== 'llm.succeeded' && eventType !== 'LLMResponseReceived') return;
	const payload = objectOrNull(inner?.payload) ?? data;
	const correlation = objectOrNull(payload.correlation) ?? {};
	const taskId = payload.task_id ?? correlation.task_id ?? fallbackTaskId;
	const callId = payload.llm_call_id ?? correlation.llm_call_id;
	if (typeof taskId !== 'string' || typeof callId !== 'string') return;
	const availability = objectOrNull(payload.usage_availability ?? correlation.usage_availability);
	const reported = payload.usage_reported !== false;
	const input = reported && availability?.tokens !== false ? numberOrNull(payload.input_tokens) : null;
	const output = reported && availability?.tokens !== false ? numberOrNull(payload.output_tokens) : null;
	const rootExecutionId = payload.root_execution_id ?? correlation.root_execution_id ?? payload.execution_id ?? correlation.execution_id;
	recordTaskLlmCall(taskId, callId,
		input === null && output === null ? null : (input ?? 0) + (output ?? 0),
		reported && availability?.cost !== false
			? numberOrNull(payload.cost_usd) ?? numberOrNull(payload.cost) : null,
		typeof rootExecutionId === 'string' ? rootExecutionId : null);
}

/**
 * Load a task's stored usage once per `phase` (the card passes its status
 * bucket, so a task is re-read when planning finishes and when the run ends,
 * not on every render).
 *
 * `/execution-panel` is a heavy read and a chat page can name dozens of tasks
 * (every card, every turn that started one), so reads are shared and
 * throttled: one read per task in flight, at most `MAX_CONCURRENT_READS` at
 * once (the browser allows ~6 connections per host; a burst of panel reads
 * starved every other call, the startup check included). A phase whose read
 * would add nothing new (`turn*` after any read of that task) is skipped.
 */
export async function loadTaskUsageSnapshot(
	taskId: string,
	principal: string,
	workspace: string,
	phase: string
): Promise<void> {
	const key = `${taskId}::${phase}`;
	if (loaded.has(key)) return;
	if (phase.startsWith('turn') && readTasks.has(taskId)) return;
	loaded.add(key);
	const pending = inFlight.get(taskId);
	if (pending) return pending;
	const read = throttled(async () => {
		const result = await readTaskRunState(taskId, principal, workspace);
		if (!result.ok || result.state === null) {
			loaded.delete(key);
			return;
		}
		readTasks.add(taskId);
		recordTaskUsageSnapshot(taskId, result.state);
		const executionId = executionIdOf(result.state);
		const history = executionId ? await readTaskCallHistory(executionId, principal, workspace) : null;
		if (history) recordTaskUsageSnapshot(taskId, result.state, history);
	}).finally(() => inFlight.delete(taskId));
	inFlight.set(taskId, read);
	return read;
}

/**
 * One `/execution-panel` read of a task's current run, through the same
 * throttle as the usage snapshots (the live card's plan-step poll reads here,
 * so the card's panel reads never exceed `MAX_CONCURRENT_READS` together). A
 * good payload also refreshes the task's usage snapshot.
 */
export async function readTaskRunStateShared(
	taskId: string,
	principal: string,
	workspace: string
): Promise<TaskRunStateResult> {
	let result: TaskRunStateResult = { ok: true, state: null };
	await throttled(async () => {
		result = await readTaskRunState(taskId, principal, workspace);
		if (result.ok && result.state !== null) {
			recordTaskUsageSnapshot(taskId, result.state);
		}
	});
	return result;
}

const MAX_CONCURRENT_READS = 2;
const inFlight = new Map<string, Promise<void>>();
const readTasks = new Set<string>();
let active = 0;
const waiting: Array<() => void> = [];

async function throttled(work: () => Promise<void>): Promise<void> {
	if (active >= MAX_CONCURRENT_READS) {
		await new Promise<void>((resolve) => waiting.push(resolve));
	}
	active += 1;
	try {
		await work();
	} finally {
		active -= 1;
		waiting.shift()?.();
	}
}

/** The usage an `/execution-panel` payload states: its run and its planning. */
export function usageSnapshotFrom(state: ExecutionPanelState): Snapshot {
	const executionId = executionIdOf(state);
	const timeline = executionId ? deriveTimeline(state, executionId) : null;
	const calls = new Map<string, CallUsage>();
	for (const entry of timeline ?? []) {
		if (entry.kind !== 'llm') continue;
		const input = entry.tokens?.input ?? null;
		const output = entry.tokens?.output ?? null;
		const tokens = input === null && output === null ? null : (input ?? 0) + (output ?? 0);
		if (tokens === null && entry.costUsd === null) continue;
		calls.set(entry.id.replace(/^llm-call:/, ''), { tokens, costUsd: entry.costUsd, executionId });
	}
	const run = sumCalls(calls);
	let planning: TaskUsage | null = null;
	for (const entry of state.debug?.planning_timeline ?? []) {
		const tokens = typeof entry.tokens === 'number' ? entry.tokens : null;
		const priced = typeof entry.cost_usd === 'number' ? entry.cost_usd : null;
		if (tokens === null && priced === null) continue;
		// A backfilled summary row stands for many calls (`llm_calls`).
		const calls = typeof entry.llm_calls === 'number' && entry.llm_calls > 0 ? entry.llm_calls : 1;
		planning = add(planning, { calls, tokens: tokens ?? 0, tokensReported: tokens !== null, costUsd: priced });
	}
	return { run, planning, executionId, calls,
		activityIds: timeline ? new Set(timeline.map((entry) => entry.id)) : null,
		updatedAt: numberOrNull(state.overview?.updated_at) ?? 0 };
}

export const taskUsage: Readable<Map<string, TaskUsageTotal>> = derived(
	[live, snapshots],
	([$live, $snapshots]) => {
		const ids = new Set([...$live.keys(), ...$snapshots.keys()]);
		const totals = new Map<string, TaskUsageTotal>();
		for (const id of ids) {
			const snapshot = $snapshots.get(id);
			const liveCalls = new Map([...($live.get(id) ?? [])].filter(([, call]) =>
				!snapshot?.executionId || !call.executionId || call.executionId === snapshot.executionId));
			const run = sumCalls(mergeCalls(liveCalls, snapshot?.calls ?? new Map()));
			const planning = snapshot?.planning ?? null;
			const total = add(run, planning ?? { calls: 0, tokens: 0, tokensReported: false, costUsd: null });
			totals.set(id, { ...total, run, planning,
				executionId: snapshot?.executionId ?? null,
				activityCount: snapshot?.activityIds?.size ?? null });
		}
		return totals;
	}
);

function add(a: TaskUsage | null, b: TaskUsage): TaskUsage {
	const base = a ?? { calls: 0, tokens: 0, tokensReported: false, costUsd: null };
	return {
		calls: base.calls + b.calls,
		tokens: base.tokens + b.tokens,
		tokensReported: base.tokensReported || b.tokensReported,
		costUsd:
			base.costUsd === null && b.costUsd === null ? null : (base.costUsd ?? 0) + (b.costUsd ?? 0)
	};
}

function mergeCalls(base: Map<string, CallUsage>, next: Map<string, CallUsage>): Map<string, CallUsage> {
	const calls = new Map(base);
	for (const [id, call] of next) {
		const previous = calls.get(id);
		calls.set(id, { ...call, tokens: call.tokens ?? previous?.tokens ?? null,
			costUsd: call.costUsd ?? previous?.costUsd ?? null });
	}
	return calls;
}

function sumCalls(calls: Map<string, CallUsage>): TaskUsage | null {
	let total: TaskUsage | null = null;
	for (const call of calls.values()) {
		total = add(total, { calls: 1, tokens: call.tokens ?? 0,
			tokensReported: call.tokens !== null, costUsd: call.costUsd });
	}
	return total;
}

function numberOrNull(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function objectOrNull(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? value as Record<string, unknown> : null;
}

/** Test seam. */
export function resetTaskUsage(): void {
	loaded.clear();
	inFlight.clear();
	readTasks.clear();
	live.set(new Map());
	snapshots.set(new Map());
}
