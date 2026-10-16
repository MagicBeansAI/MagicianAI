// The live task card's plan-step poll: the current run's `/execution-panel`,
// read while a planned task runs and its card is mounted.
//
// One shared poll per task (every card naming the task shares it), built on
// `createSharedPoll` (polls only while subscribed, jittered backoff on
// failure, last good value kept). Reads go through the usage module's
// throttle, so they share its cap on concurrent panel reads, and refresh the
// card's usage snapshot as a side effect. The drawer has its own poll for the
// task it has open; this one exists because a chat card has no drawer open.

import type { Readable } from 'svelte/store';

import { createSharedPoll, type SharedPoll } from '$lib/stores/sharedPoll';
import { extractPlanStepsFromExecutionPanel, type PlanStep } from '$lib/stores/taskStore';
import { readTaskRunStateShared } from './taskUsage';

/** Steps change at tool-call pace; a few seconds is live enough. */
const PLAN_STEP_POLL_MS = 5_000;
const PLAN_STEP_POLL_MAX_BACKOFF_MS = 60_000;

const polls = new Map<string, SharedPoll<PlanStep[]>>();

/** The task's current-run plan steps with status; `null` until the first read. */
export function planStepPoll(
	taskId: string,
	principal: string,
	workspace: string
): Readable<PlanStep[] | null> {
	const key = `${principal}\u0000${workspace}\u0000${taskId}`;
	let poll = polls.get(key);
	if (!poll) {
		poll = createSharedPoll<PlanStep[]>({
			fetcher: async () => {
				const result = await readTaskRunStateShared(taskId, principal, workspace);
				if (!result.ok) throw new Error(result.reason);
				return result.state ? extractPlanStepsFromExecutionPanel(result.state) : [];
			},
			idleMs: PLAN_STEP_POLL_MS,
			fastMs: PLAN_STEP_POLL_MS,
			maxBackoffMs: PLAN_STEP_POLL_MAX_BACKOFF_MS
		});
		polls.set(key, poll);
	}
	return poll.value;
}

/** Test seam. */
export function resetPlanStepPolls(): void {
	polls.clear();
}
