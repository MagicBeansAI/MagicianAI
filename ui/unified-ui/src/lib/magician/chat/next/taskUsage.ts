// Model usage per task, for the chat's live task card: calls, tokens (in + out)
// and cost, for the run (with its delegated children) plus planning.
//
// Two sources, so the total survives a reload and covers planning:
// - live: the task-scoped activity stream (`ChatTurnProgress` with a `taskId`)
//   records each finished call (`recordTaskLlmCall`), counted once by its id;
// - snapshot: `/execution-panel` (`loadTaskUsageSnapshot`), the run's priced
//   calls (the same `deriveTimeline` → `runCost` the task panel shows) and the
//   planning activity's priced calls (`debug.planning_timeline`).
// The run part is the live total when there is one, else the snapshot's;
// planning always comes from the snapshot (planning calls are not on the
// task's event stream).

import { derived, writable, type Readable } from 'svelte/store';

import { readTaskRunState, type TaskRunStateResult } from '$lib/magician/tasks/taskAttention';
import { deriveTimeline, executionIdOf, runCost } from '$lib/magician/tasks/taskTimeline';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

export interface TaskUsage {
	calls: number;
	tokens: number;
	/** `null` until a call reports a price. */
	costUsd: number | null;
}

/** The card's figure: run + planning, and the two parts for its tooltip. */
export interface TaskUsageTotal extends TaskUsage {
	run: TaskUsage | null;
	planning: TaskUsage | null;
}

interface Snapshot {
	run: TaskUsage | null;
	planning: TaskUsage | null;
}

const seen = new Set<string>();
const live = writable<Map<string, TaskUsage>>(new Map());
const snapshots = writable<Map<string, Snapshot>>(new Map());
const loaded = new Set<string>();

/** Record one finished model call of a task's run. Idempotent per `callKey`. */
export function recordTaskLlmCall(
	taskId: string,
	callKey: string,
	tokens: number | null,
	costUsd: number | null
): void {
	const key = `${taskId}::${callKey}`;
	if (seen.has(key)) return;
	seen.add(key);
	live.update((map) => {
		const next = new Map(map);
		next.set(taskId, add(next.get(taskId) ?? null, { calls: 1, tokens: tokens ?? 0, costUsd }));
		return next;
	});
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
		readTasks.add(taskId);
		const result = await readTaskRunState(taskId, principal, workspace);
		if (!result.ok || result.state === null) return;
		const snapshot = usageSnapshotFrom(result.state);
		snapshots.update((map) => new Map(map).set(taskId, snapshot));
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
		readTasks.add(taskId);
		result = await readTaskRunState(taskId, principal, workspace);
		if (result.ok && result.state !== null) {
			const snapshot = usageSnapshotFrom(result.state);
			snapshots.update((map) => new Map(map).set(taskId, snapshot));
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
	const cost = executionId ? runCost(deriveTimeline(state, executionId)) : null;
	const run =
		cost && (cost.usage !== null || cost.costUsd !== null)
			? {
					calls: cost.usage?.calls ?? 0,
					tokens: (cost.usage?.input ?? 0) + (cost.usage?.output ?? 0),
					costUsd: cost.costUsd
				}
			: null;
	let planning: TaskUsage | null = null;
	for (const entry of state.debug?.planning_timeline ?? []) {
		const tokens = typeof entry.tokens === 'number' ? entry.tokens : null;
		const priced = typeof entry.cost_usd === 'number' ? entry.cost_usd : null;
		if (tokens === null && priced === null) continue;
		// A backfilled summary row stands for many calls (`llm_calls`).
		const calls = typeof entry.llm_calls === 'number' && entry.llm_calls > 0 ? entry.llm_calls : 1;
		planning = add(planning, { calls, tokens: tokens ?? 0, costUsd: priced });
	}
	return { run, planning };
}

export const taskUsage: Readable<Map<string, TaskUsageTotal>> = derived(
	[live, snapshots],
	([$live, $snapshots]) => {
		const ids = new Set([...$live.keys(), ...$snapshots.keys()]);
		const totals = new Map<string, TaskUsageTotal>();
		for (const id of ids) {
			const liveRun = $live.get(id) ?? null;
			const snapshot = $snapshots.get(id);
			const run = liveRun && liveRun.calls > 0 ? liveRun : (snapshot?.run ?? null);
			const planning = snapshot?.planning ?? null;
			const total = add(run, planning ?? { calls: 0, tokens: 0, costUsd: null });
			if (total.calls === 0 && total.costUsd === null) continue;
			totals.set(id, { ...total, run, planning });
		}
		return totals;
	}
);

function add(a: TaskUsage | null, b: TaskUsage): TaskUsage {
	const base = a ?? { calls: 0, tokens: 0, costUsd: null };
	return {
		calls: base.calls + b.calls,
		tokens: base.tokens + b.tokens,
		costUsd:
			base.costUsd === null && b.costUsd === null ? null : (base.costUsd ?? 0) + (b.costUsd ?? 0)
	};
}

/** Test seam. */
export function resetTaskUsage(): void {
	seen.clear();
	loaded.clear();
	inFlight.clear();
	readTasks.clear();
	live.set(new Map());
	snapshots.set(new Map());
}
