/**
 * Off-plan attribution on the Run act: the run's activity rows grouped under
 * the approved-plan step the run was on when each was recorded, and the rows
 * recorded outside every step — before the first, between steps, after the
 * last, or after a skipped step.
 *
 * The server decides the attribution (`plan_step_id` / `off_plan` on each
 * activity row, from the run's durable `step.started` and step-outcome
 * events); this only groups what it said. A run without an approved plan, or
 * one recorded before step starts were durable, carries neither field on any
 * row, and `attributed` is then `false`: the act renders exactly as before.
 *
 * Pure, and mirrored by `PlanStepActivity.swift` (iOS) and `PlanStepActivity.kt`
 * (Android) with the same test cases.
 * See `docs/archive/plans/2026-10-01-plan-step-progress.md`.
 */

import type { TimelineEntry } from './taskTimeline';

/** The explanation the "Outside the plan" group carries, on every surface. */
export const OFF_PLAN_EXPLANATION = 'Work the agent did between or beyond the planned steps';

export interface PlanStepActivity {
	/** Whether any row carries an attribution; `false` leaves the act unchanged. */
	attributed: boolean;
	/** Each plan step's rows, oldest first, keyed by plan step id. */
	byStep: ReadonlyMap<string, readonly TimelineEntry[]>;
	/** The rows outside every plan step, oldest first. */
	offPlan: readonly TimelineEntry[];
}

const EMPTY: PlanStepActivity = { attributed: false, byStep: new Map(), offPlan: [] };

/**
 * Group `entries` by their plan step. Rows with no attribution (lifecycle
 * rows, shell and observation rows) belong to neither group; they stay in the
 * flat "All activity" timeline, which always shows every row.
 */
export function groupActivityByPlanStep(
	entries: readonly TimelineEntry[] | null | undefined
): PlanStepActivity {
	if (!entries || entries.length === 0) return EMPTY;
	const byStep = new Map<string, TimelineEntry[]>();
	const offPlan: TimelineEntry[] = [];
	let attributed = false;
	const ordered = [...entries].sort((left, right) => left.at - right.at);
	for (const entry of ordered) {
		const stepId = entry.planStepId?.trim();
		if (stepId) {
			attributed = true;
			const rows = byStep.get(stepId);
			if (rows) rows.push(entry);
			else byStep.set(stepId, [entry]);
		} else if (entry.offPlan === true) {
			attributed = true;
			offPlan.push(entry);
		}
	}
	return attributed ? { attributed, byStep, offPlan } : EMPTY;
}

/** How many rows a plan step has, or `null` when nothing is attributed. */
export function stepActivityCount(
	activity: PlanStepActivity,
	stepId: string | null | undefined
): number | null {
	if (!activity.attributed || !stepId) return null;
	return activity.byStep.get(stepId)?.length ?? 0;
}

/** The group header: `Outside the plan · 3`. */
export function offPlanLabel(count: number): string {
	return `Outside the plan · ${count}`;
}
