import { describe, expect, it } from 'vitest';

import type { PlanStep } from '$lib/stores/taskStore';
import {
	PLAN_STEP_BAR_MAX_SEGMENTS,
	planStepAccessibleSummary,
	planStepBar,
	planStepMark,
	planStepRowsFromPanel,
	planStepRowsFromSteps,
	planStepSummary,
	type PlanStepRow
} from './planStepProgress';

function rows(marks: PlanStepRow['mark'][]): PlanStepRow[] {
	return marks.map((mark, index) => ({
		id: `s${index + 1}`,
		name: `Step name ${index + 1}`,
		mark,
		note: null
	}));
}

function step(status: PlanStep['status'], description = 'Do it', extra: Partial<PlanStep> = {}): PlanStep {
	return { id: description, description, status, ...extra };
}

describe('planStepMark', () => {
	it('maps the store statuses like iOS and Android', () => {
		expect(planStepMark('completed', false)).toBe('done');
		expect(planStepMark('failed', false)).toBe('failed');
		expect(planStepMark('in_progress', false)).toBe('inProgress');
		expect(planStepMark('skipped', false)).toBe('skipped');
		expect(planStepMark('pending', false)).toBe('pending');
		expect(planStepMark('cancelled', false)).toBe('pending');
		expect(planStepMark(undefined, false)).toBe('pending');
	});

	it('reads a pending step as not reached once the run ended', () => {
		expect(planStepMark('pending', true)).toBe('notReached');
		expect(planStepMark('cancelled', true)).toBe('notReached');
		expect(planStepMark('completed', true)).toBe('done');
	});
});

describe('planStepRowsFromPanel', () => {
	it('reads debug.selected_execution.step_statuses in plan order, with notes', () => {
		const panel = {
			debug: {
				selected_execution: {
					step_statuses: [
						{ number: 1, name: 'Gather sources', status: 'completed', step_id: 'a' },
						{ number: 2, name: 'Draft', status: 'skipped', step_id: 'b', note: ' Not needed ' },
						{ number: 3, name: 'Review', status: 'in_progress', step_id: 'c' },
						{ number: 4, name: '', status: 'pending' }
					]
				}
			}
		};
		expect(planStepRowsFromPanel(panel, false)).toEqual([
			{ id: 'a', name: 'Gather sources', mark: 'done', note: null },
			{ id: 'b', name: 'Draft', mark: 'skipped', note: 'Not needed' },
			{ id: 'c', name: 'Review', mark: 'inProgress', note: null },
			{ id: 'step-4', name: 'Step 4', mark: 'pending', note: null }
		]);
	});

	it('is empty when the payload has no step statuses', () => {
		expect(planStepRowsFromPanel(null, false)).toEqual([]);
		expect(planStepRowsFromPanel({ debug: { selected_execution: { step_statuses: [] } } }, false)).toEqual([]);
	});

	it('names an unnamed store step by its position', () => {
		expect(planStepRowsFromSteps([step('pending', '  ')], false)[0].name).toBe('Step 1');
	});
});

describe('planStepSummary', () => {
	it('names the running step', () => {
		expect(planStepSummary(rows(['done', 'done', 'inProgress', 'pending', 'pending', 'pending']))).toBe(
			'Step 3 of 6 · Step name 3'
		);
	});

	it('names the failed step when none runs', () => {
		expect(planStepSummary(rows(['done', 'failed', 'pending']))).toBe('Step 2 of 3 failed · Step name 2');
	});

	it('prefers the running step over a failed one', () => {
		expect(planStepSummary(rows(['failed', 'inProgress']))).toBe('Step 2 of 2 · Step name 2');
	});

	it('counts done steps, and skipped ones when any', () => {
		expect(planStepSummary(rows(['done', 'done', 'done', 'done', 'done', 'done']))).toBe('6 of 6 done');
		expect(planStepSummary(rows(['done', 'skipped', 'done']))).toBe('2 of 3 done · 1 skipped');
		expect(planStepSummary(rows(['done', 'pending', 'pending']))).toBe('1 of 3 done');
	});

	it('is empty with no steps', () => {
		expect(planStepSummary([])).toBe('');
	});
});

describe('planStepAccessibleSummary', () => {
	it('reads "Step 3 of 6: <name>"', () => {
		expect(planStepAccessibleSummary(rows(['done', 'done', 'inProgress', 'pending', 'pending', 'pending']))).toBe(
			'Step 3 of 6: Step name 3'
		);
		expect(planStepAccessibleSummary(rows(['done', 'failed', 'pending']))).toBe('Step 2 of 3 failed: Step name 2');
		expect(planStepAccessibleSummary(rows(['done', 'skipped', 'done']))).toBe('2 of 3 done, 1 skipped');
	});

	it('keeps a separator inside the step name', () => {
		const named = rows(['inProgress']);
		named[0].name = 'Fetch · parse';
		expect(planStepAccessibleSummary(named)).toBe('Step 1 of 1: Fetch · parse');
	});
});

describe('planStepBar', () => {
	it('is null with no steps', () => {
		expect(planStepBar([])).toBeNull();
	});

	it('has one segment per step up to 12', () => {
		expect(PLAN_STEP_BAR_MAX_SEGMENTS).toBe(12);
		expect(planStepBar(rows(['done', 'failed', 'inProgress', 'skipped', 'pending']))).toEqual({
			kind: 'segments',
			marks: ['done', 'failed', 'inProgress', 'skipped', 'pending']
		});
		const twelve = planStepBar(rows(Array(12).fill('pending')));
		expect(twelve).toEqual({ kind: 'segments', marks: Array(12).fill('pending') });
	});

	it('is one continuous fill past 12, counting done and skipped as settled', () => {
		const marks: PlanStepRow['mark'][] = [
			...Array(5).fill('done'),
			...Array(2).fill('skipped'),
			'failed',
			'inProgress',
			...Array(7).fill('pending')
		];
		expect(planStepBar(rows(marks))).toEqual({ kind: 'continuous', fraction: 7 / 16 });
	});
});
