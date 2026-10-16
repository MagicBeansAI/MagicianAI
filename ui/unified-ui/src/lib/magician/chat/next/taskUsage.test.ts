import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';

import { recordTaskLlmCall, resetTaskUsage, taskUsage, usageSnapshotFrom } from './taskUsage';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

beforeEach(resetTaskUsage);

describe('taskUsage', () => {
	it('sums calls, tokens and cost per task', () => {
		recordTaskLlmCall('task-1', 'call-a', 1200, 0.01);
		recordTaskLlmCall('task-1', 'call-b', 800, null);
		recordTaskLlmCall('task-2', 'call-c', 50, 0.002);

		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 2, tokens: 2000, costUsd: 0.01 });
		expect(get(taskUsage).get('task-2')).toMatchObject({ calls: 1, tokens: 50, costUsd: 0.002 });
	});

	it('counts a replayed call once', () => {
		recordTaskLlmCall('task-1', 'call-a', 1200, 0.01);
		recordTaskLlmCall('task-1', 'call-a', 1200, 0.01);
		expect(get(taskUsage).get('task-1')?.calls).toBe(1);
	});

	it('reports no cost until a call is priced', () => {
		recordTaskLlmCall('task-1', 'call-a', 100, null);
		expect(get(taskUsage).get('task-1')?.costUsd).toBeNull();
	});

	it('reads planning usage from the planning timeline of a stored payload', () => {
		const state = {
			default_tab: 'plan',
			overview: { task_id: 'task-1', status: 'ready' },
			run: {},
			output: {},
			debug: {
				timeline: [],
				planning_timeline: [
					{ id: 'a', timestamp: 1, severity: 'info', title: 'Progress', message: 'Outlined', cost_usd: 0.02, tokens: 9000 },
					{ id: 'b', timestamp: 2, severity: 'info', title: 'Progress', message: 'Outlining the plan' },
					{ id: 'c', timestamp: 3, severity: 'info', title: 'Progress', message: 'Worked out the steps', cost_usd: 0.03, tokens: 27000 }
				],
				observations: [],
				shell_entries: [],
				history_count: 0,
				tags: []
			}
		} as unknown as ExecutionPanelState;
		const snapshot = usageSnapshotFrom(state);
		expect(snapshot.run).toBeNull();
		expect(snapshot.planning).toMatchObject({ calls: 2, tokens: 36000 });
		expect(snapshot.planning?.costUsd).toBeCloseTo(0.05, 10);
	});

	it('counts every call a backfilled planning summary stands for', () => {
		const state = {
			default_tab: 'plan',
			overview: { task_id: 'task-1', status: 'ready' },
			run: {},
			output: {},
			debug: {
				timeline: [],
				planning_timeline: [
					{ id: 'b', timestamp: 1, severity: 'info', title: 'Progress', message: 'Planning model usage', cost_usd: 0.27, tokens: 96000, llm_calls: 40 }
				],
				observations: [],
				shell_entries: [],
				history_count: 0,
				tags: []
			}
		} as unknown as ExecutionPanelState;
		expect(usageSnapshotFrom(state).planning).toMatchObject({ calls: 40, tokens: 96000, costUsd: 0.27 });
	});
});
