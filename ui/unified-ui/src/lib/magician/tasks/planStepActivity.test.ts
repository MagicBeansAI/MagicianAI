import { describe, expect, it } from 'vitest';

import type { ExecutionPanelState } from '$lib/types/executionPanel';

import {
	groupActivityByPlanStep,
	OFF_PLAN_EXPLANATION,
	offPlanLabel,
	stepActivityCount
} from './planStepActivity';
import { deriveTimeline, type TimelineEntry } from './taskTimeline';

/**
 * The same cases as `PlanStepActivityTests.swift` (iOS) and
 * `PlanStepActivityTest.kt` (Android).
 */
function row(id: string, at: number, planStepId: string | null, offPlan = false): TimelineEntry {
	return {
		id,
		at,
		kind: 'tool',
		status: 'done',
		title: id,
		body: null,
		detail: null,
		latencyMs: null,
		model: null,
		costUsd: null,
		tokens: null,
		screenshot: false,
		executionId: 'exec-1',
		agentId: null,
		planStepId,
		offPlan
	};
}

describe('groupActivityByPlanStep', () => {
	it('leaves an unattributed run unchanged', () => {
		const activity = groupActivityByPlanStep([row('a', 1, null), row('b', 2, null)]);
		expect(activity.attributed).toBe(false);
		expect(activity.byStep.size).toBe(0);
		expect(activity.offPlan).toEqual([]);
		expect(groupActivityByPlanStep(null).attributed).toBe(false);
	});

	it('groups rows under their step and the rest outside the plan, oldest first', () => {
		const activity = groupActivityByPlanStep([
			row('after', 9, null, true),
			row('s2-call', 7, 's2'),
			row('s1-second', 3, 's1'),
			row('before', 1, null, true),
			row('s1-first', 2, 's1'),
			row('between', 5, null, true)
		]);
		expect(activity.attributed).toBe(true);
		expect(activity.byStep.get('s1')?.map((entry) => entry.id)).toEqual(['s1-first', 's1-second']);
		expect(activity.byStep.get('s2')?.map((entry) => entry.id)).toEqual(['s2-call']);
		expect(activity.offPlan.map((entry) => entry.id)).toEqual(['before', 'between', 'after']);
	});

	it('ignores rows that carry no attribution in an attributed run', () => {
		const activity = groupActivityByPlanStep([row('lifecycle', 1, null), row('s1', 2, 's1')]);
		expect(activity.attributed).toBe(true);
		expect(activity.offPlan).toEqual([]);
		expect(activity.byStep.get('s1')?.length).toBe(1);
	});

	it('counts a step with no rows as 0, and nothing when unattributed', () => {
		const attributed = groupActivityByPlanStep([row('s1', 2, 's1')]);
		expect(stepActivityCount(attributed, 's1')).toBe(1);
		expect(stepActivityCount(attributed, 's3')).toBe(0);
		expect(stepActivityCount(groupActivityByPlanStep([]), 's1')).toBeNull();
	});

	it('labels the off-plan group with its count and explanation', () => {
		expect(offPlanLabel(3)).toBe('Outside the plan · 3');
		expect(OFF_PLAN_EXPLANATION).toBe('Work the agent did between or beyond the planned steps');
	});
});

describe('deriveTimeline attribution fields', () => {
	it('carries plan_step_id and off_plan from the activity log', () => {
		const state = {
			overview: { execution_id: 'exec-1' },
			run: {
				activity_log: [
					{
						id: 'log-1',
						status: 'info',
						title: 'tool succeeded',
						created_at: 10,
						metadata: { event_type: 'tool.succeeded', execution_id: 'exec-1', plan_step_id: 's1' }
					},
					{
						id: 'log-2',
						status: 'info',
						title: 'tool succeeded',
						created_at: 20,
						metadata: { event_type: 'tool.succeeded', execution_id: 'exec-1', off_plan: true }
					}
				]
			}
		} as unknown as ExecutionPanelState;
		const timeline = deriveTimeline(state, 'exec-1') ?? [];
		const byId = new Map(timeline.map((entry) => [entry.id, entry]));
		expect(byId.get('log-1')?.planStepId).toBe('s1');
		expect(byId.get('log-1')?.offPlan).toBe(false);
		expect(byId.get('log-2')?.planStepId).toBeNull();
		expect(byId.get('log-2')?.offPlan).toBe(true);
	});
});
