import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/magician/tasks/taskAttention', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/magician/tasks/taskAttention')>()),
	readTaskRunState: vi.fn()
}));
vi.mock('./taskCallHistory', () => ({ readTaskCallHistory: vi.fn() }));

import {
	recordTaskLlmCall, recordTaskUsageEvent, recordTaskUsageSnapshot,
	resetTaskUsage, taskUsage, usageSnapshotFrom, loadTaskUsageSnapshot
} from './taskUsage';
import { readTaskRunState } from '$lib/magician/tasks/taskAttention';
import { readTaskCallHistory } from './taskCallHistory';
import type { ExecutionPanelState } from '$lib/types/executionPanel';

beforeEach(() => {
	resetTaskUsage();
	vi.mocked(readTaskRunState).mockReset();
	vi.mocked(readTaskCallHistory).mockReset();
});

function panel(callIds: string[], executionId = 'exec-1', updatedAt = 1): ExecutionPanelState {
	return {
		overview: { task_id: 'task-1', execution_id: executionId, updated_at: updatedAt },
		run: {
			activity_log: callIds.flatMap((id, index) => ['requested', 'succeeded'].map((phase) => ({
				id: `${id}-${phase}`, created_at: index * 10, status: phase === 'requested' ? 'running' : 'done',
				metadata: {
					event_type: `llm.${phase}`, llm_call_id: id, execution_id: executionId,
					...(phase === 'succeeded' ? { input_tokens: 1000, output_tokens: 200, cost_usd: 0.01 } : {})
				}
			})))
		},
		output: {}, debug: {}
	} as unknown as ExecutionPanelState;
}

describe('taskUsage', () => {
	it('hydrates both sources once per phase when a task card subscribes mid-run', async () => {
		vi.mocked(readTaskRunState).mockResolvedValue({ ok: true, state: panel(['call-a']) });
		vi.mocked(readTaskCallHistory).mockResolvedValue(new Map([
			['call-a', { tokens: 1200, costUsd: 0.01, executionId: 'exec-1' }],
			['judge', { tokens: 550, costUsd: 0.002, executionId: 'exec-1' }]
		]));
		await loadTaskUsageSnapshot('task-1', 'anonymous', 'default', 'turn-live');
		await loadTaskUsageSnapshot('task-1', 'anonymous', 'default', 'turn-live');
		expect(readTaskRunState).toHaveBeenCalledTimes(1);
		expect(readTaskCallHistory).toHaveBeenCalledOnce();
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 2, tokens: 1750 });
		recordTaskLlmCall('task-1', 'new-call', 300, null, 'exec-1');
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 3, tokens: 2050 });
	});

	it('retains auxiliary ledger calls across subsequent activity-panel updates', () => {
		recordTaskUsageSnapshot('task-1', panel(['call-a']), new Map([
			['call-a', { tokens: 1200, costUsd: 0.01, executionId: 'exec-1' }],
			['judge', { tokens: 550, costUsd: 0.002, executionId: 'exec-1' }],
			['unknown', { tokens: null, costUsd: null, executionId: 'exec-1' }]
		]));
		recordTaskUsageSnapshot('task-1', panel(['call-a']));
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 3, tokens: 1750, costUsd: 0.012 });
	});

	it('includes a delegated child live call under its root execution', () => {
		recordTaskUsageSnapshot('task-1', panel(['call-a']));
		recordTaskUsageEvent({ event_type: 'LLMResponseReceived', data: {
			task_id: 'task-1', execution_id: 'exec-child', input_tokens: 500, output_tokens: 50,
			correlation: { llm_call_id: 'child-call', root_execution_id: 'exec-1' }
		} });
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 2, tokens: 1750 });
	});

	it('extends a late subscription baseline without replacing it or double counting replay', () => {
		recordTaskUsageSnapshot('task-1', panel(['call-a', 'call-b']));
		recordTaskLlmCall('task-1', 'call-b', 1200, 0.01, 'exec-1');
		recordTaskLlmCall('task-1', 'call-c', 700, 0.02, 'exec-1');
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 3, tokens: 3100, costUsd: 0.04 });
		// A later full read now covers the same live call. Its authoritative
		// measurements replace the live estimate; it still counts exactly once.
		recordTaskUsageSnapshot('task-1', panel(['call-a', 'call-b', 'call-c']));
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 3, tokens: 3600, costUsd: 0.03 });
	});

	it('keeps recorded progress across resumes and older, shorter replays', () => {
		recordTaskUsageSnapshot('task-1', panel(['call-a', 'call-b']));
		recordTaskUsageSnapshot('task-1', panel(['call-b']));
		expect(get(taskUsage).get('task-1')).toMatchObject({ activityCount: 2, calls: 2, tokens: 2400 });
		recordTaskUsageSnapshot('task-1', panel(['call-b', 'call-c']));
		expect(get(taskUsage).get('task-1')).toMatchObject({ activityCount: 3, calls: 3, tokens: 3600 });
	});

	it('starts fresh for a genuinely new execution and ignores late old-execution reads', () => {
		recordTaskLlmCall('task-1', 'old-call', 800, 0.02, 'exec-1');
		recordTaskUsageSnapshot('task-1', panel(['call-a', 'call-b'], 'exec-1', 1));
		recordTaskUsageSnapshot('task-1', panel(['call-c'], 'exec-2', 2));
		recordTaskUsageSnapshot('task-1', panel(['call-a'], 'exec-1', 1));
		expect(get(taskUsage).get('task-1')).toMatchObject({
			executionId: 'exec-2', calls: 1, tokens: 1200, activityCount: 1
		});
	});

	it('merges typed and enveloped responses and enriches missing measurements', () => {
		recordTaskUsageEvent({ event_type: 'LLMResponseReceived', data: {
			correlation: { task_id: 'task-1', execution_id: 'exec-1', llm_call_id: 'call-a' },
			input_tokens: 1000, output_tokens: 200
		} });
		recordTaskUsageEvent({ event_type: 'AgentEvent', data: { event: {
			event_type: 'llm.succeeded', payload: {
				task_id: 'task-1', execution_id: 'exec-1', llm_call_id: 'call-a', cost_usd: 0.01
			}
		} } });
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 1, tokens: 1200, costUsd: 0.01 });
	});

	it('does not turn unreported usage placeholders into a zero-token call', () => {
		recordTaskUsageEvent({ event_type: 'LLMResponseReceived', data: {
			correlation: { task_id: 'task-1', llm_call_id: 'judge', usage_availability: { tokens: false, cost: false } },
			input_tokens: 0, output_tokens: 0, cost: 0
		} });
		recordTaskUsageEvent({ event_type: 'LLMResponseReceived', data: {
			task_id: 'task-1', llm_call_id: 'judge-2', usage_reported: false,
			input_tokens: 0, output_tokens: 0, cost: 0
		} });
		expect(get(taskUsage).has('task-1')).toBe(false);
		// An explicitly measured zero remains valid, and a priced call with
		// no token count remains distinguishable from that measured zero.
		recordTaskLlmCall('task-1', 'priced', null, 0.01);
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 1, tokensReported: false });
		recordTaskLlmCall('task-1', 'measured-zero', 0, 0);
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 2, tokens: 0, tokensReported: true });
	});

	it('refreshes usage and full progress from live execution-panel deltas', () => {
		recordTaskUsageEvent({ event_type: 'ExecutionPanelDelta', data: {
			task_id: 'task-1', state: panel(['call-a', 'call-b'])
		} });
		expect(get(taskUsage).get('task-1')).toMatchObject({ calls: 2, tokens: 2400, activityCount: 2 });
	});

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
