// The live task card's plan-step line and step bar, while a planned task runs.
//
// Source: the current run's `/execution-panel` payload,
// `debug.selected_execution.step_statuses`, parsed by the same
// `extractPlanStepsFromExecutionPanel` the drawer's plan rows come from
// (`taskStore.applyExecutionPanelSteps` → `task.planSteps` → `runStepsOf`).
// Strings and the segment rule mirror iOS (`PlanProgressModel.swift`
// `PlanSteps`, `LiveTaskCardSupport.swift` `LiveTaskStepBar`) and Android
// (`PlanProgress.kt`), so the three cards read the same.

import { extractPlanStepsFromExecutionPanel, type PlanStep } from '$lib/stores/taskStore';

/** A plan step's mark in the step bar. */
export type PlanStepMark = 'done' | 'failed' | 'inProgress' | 'pending' | 'skipped' | 'notReached';

export interface PlanStepRow {
	id: string;
	name: string;
	mark: PlanStepMark;
	/** The agent's reason (a skipped, failed or done-another-way step). */
	note: string | null;
}

/** Beyond this many plan steps the bar is one continuous fill. */
export const PLAN_STEP_BAR_MAX_SEGMENTS = 12;

export type PlanStepBar =
	/** One segment per plan step, in plan order. */
	| { kind: 'segments'; marks: PlanStepMark[] }
	/** Share of steps settled (done or skipped), 0…1. */
	| { kind: 'continuous'; fraction: number };

/**
 * A store step status as a mark. Cancelled (and anything unknown, which the
 * store already reads as pending) reads as pending; pending reads
 * `notReached` once the run ended.
 */
export function planStepMark(status: PlanStep['status'] | undefined, runEnded: boolean): PlanStepMark {
	switch (status) {
		case 'completed':
			return 'done';
		case 'failed':
			return 'failed';
		case 'in_progress':
			return 'inProgress';
		case 'skipped':
			return 'skipped';
		default:
			return runEnded ? 'notReached' : 'pending';
	}
}

/** Store plan steps as rows, in plan order (never re-sorted by completion). */
export function planStepRowsFromSteps(steps: PlanStep[], runEnded: boolean): PlanStepRow[] {
	return steps.map((step, index) => ({
		id: step.id || `step-${index + 1}`,
		name: step.description?.trim() || `Step ${index + 1}`,
		mark: planStepMark(step.status, runEnded),
		note: step.note?.trim() || null
	}));
}

/** An `/execution-panel` payload's `step_statuses` as rows; `[]` when it has none. */
export function planStepRowsFromPanel(executionPanel: unknown, runEnded: boolean): PlanStepRow[] {
	return planStepRowsFromSteps(extractPlanStepsFromExecutionPanel(executionPanel), runEnded);
}

/**
 * `Step 3 of 6 · <name>` while a step runs; `Step 2 of 3 failed · <name>`
 * after a failure; else `4 of 6 done` (`· 1 skipped` when any were).
 */
export function planStepSummary(rows: PlanStepRow[]): string {
	if (rows.length === 0) return '';
	const total = rows.length;
	const running = rows.findIndex((row) => row.mark === 'inProgress');
	if (running >= 0) return `Step ${running + 1} of ${total} · ${rows[running].name}`;
	const failed = rows.findIndex((row) => row.mark === 'failed');
	if (failed >= 0) return `Step ${failed + 1} of ${total} failed · ${rows[failed].name}`;
	const done = rows.filter((row) => row.mark === 'done').length;
	const skipped = rows.filter((row) => row.mark === 'skipped').length;
	return skipped > 0 ? `${done} of ${total} done · ${skipped} skipped` : `${done} of ${total} done`;
}

/** The summary for a screen reader: `Step 3 of 6: <name>`, `4 of 6 done, 1 skipped`. */
export function planStepAccessibleSummary(rows: PlanStepRow[]): string {
	const summary = planStepSummary(rows);
	return summary.startsWith('Step ')
		? summary.replace(' · ', ': ')
		: summary.replace(' · ', ', ');
}

/** Segments up to `PLAN_STEP_BAR_MAX_SEGMENTS` steps, else a continuous fill; `null` with no steps. */
export function planStepBar(rows: PlanStepRow[]): PlanStepBar | null {
	if (rows.length === 0) return null;
	if (rows.length <= PLAN_STEP_BAR_MAX_SEGMENTS) {
		return { kind: 'segments', marks: rows.map((row) => row.mark) };
	}
	const settled = rows.filter((row) => row.mark === 'done' || row.mark === 'skipped').length;
	return { kind: 'continuous', fraction: settled / rows.length };
}
