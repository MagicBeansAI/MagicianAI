import { describe, expect, it } from 'vitest';
import type { ExecutionPanelState } from '$lib/types/executionPanel';
import { executionProgressAt, latestProgressAt, taskProgressSignalOf } from './taskProgress';

function panel(events: Array<[string, number]>, executionId = 'exec-1'): ExecutionPanelState {
	return {
		overview: { execution_id: executionId, updated_at: 999_999 },
		run: { activity_log: events.map(([type, at]) => ({
			created_at: at, metadata: { event_type: type }
		})) }, debug: {}
	} as unknown as ExecutionPanelState;
}

describe('task progress evidence', () => {
	it('recognises streamed model output while the call is still in progress', () => {
		const state = panel([['llm.requested', 10_000], ['llm.first_token', 400_000], ['reasoning.content', 500_000]]);
		expect(executionProgressAt(state, 'exec-1')).toBe(500_000);
	});

	it('uses work within a long step, including delegated child work', () => {
		const state = panel([['step.started', 10_000], ['tool.succeeded', 400_000], ['llm.requested', 410_000]]);
		state.run.activity_log![1].metadata = { event_type: 'tool.succeeded', execution_id: 'child-exec' };
		expect(executionProgressAt(state, 'exec-1')).toBe(410_000);
	});

	it('ignores heartbeats, metrics, warnings, status polls and unknown event types', () => {
		const state = panel([
			['tool.succeeded', 10_000], ['execution.heartbeat', 500_000],
			['execution.status_changed', 600_000], ['metrics.updated', 700_000],
			['agentic.step_stuck_warning', 800_000], ['unrecognized', 900_000]
		]);
		state.debug.timeline = [{ id: 'heartbeat', title: 'Execution heartbeat', timestamp: 999_999, severity: 'info', message: '' }];
		expect(executionProgressAt(state, 'exec-1')).toBe(10_000);
	});

	it('does not borrow progress from a history-picked execution', () => {
		expect(executionProgressAt(panel([['tool.succeeded', 500_000]], 'old-exec'), 'exec-1')).toBeNull();
	});

	it('uses the recent activity fallback and explicit legacy step titles', () => {
		const state = panel([]);
		state.run.recent_activity = panel([['tool.failed', 200_000]]).run.activity_log!;
		state.debug.timeline = [{ id: 'step', title: 'Step started', timestamp: 300_000, severity: 'info', message: '' }];
		expect(executionProgressAt(state, 'exec-1')).toBe(300_000);
	});

	it('keeps missing and invalid timing unknown', () => {
		expect(latestProgressAt(null, undefined, NaN, Infinity, 0, -1)).toBeNull();
		expect(executionProgressAt(panel([['tool.succeeded', NaN]]), 'exec-1')).toBeNull();
		expect(latestProgressAt(200, 100)).toBe(200);
	});

	it('reads typed and enveloped work events with their recorded root and scope', () => {
		const data = { principal: 'owner', workspace: 'work', execution_id: 'child', timestamp: 200,
			correlation: { task_id: 'task-1', root_execution_id: 'exec-1' } };
		expect(taskProgressSignalOf({ event_type: 'AgenticActionExecuted', data })).toEqual({
			principal: 'owner', workspace: 'work', taskId: 'task-1', executionId: 'exec-1', at: 200
		});
		expect(taskProgressSignalOf({ event_type: 'AgentEvent', data: { event: {
			event_type: 'llm.succeeded', payload: data
		} } })?.at).toBe(200);
		expect(taskProgressSignalOf({ event_type: 'ExecutionStatusChanged', data })).toBeNull();
		expect(taskProgressSignalOf({ event_type: 'LLMResponseReceived', data: { ...data, timestamp: null } })).toBeNull();
		expect(taskProgressSignalOf({ event_type: 'LLMResponseReceived', data: {
			...data, correlation: { ...data.correlation, provider_attempt_count: 0 }
		} })).toBeNull();
	});

	it('reads a panel delta from its work log rather than its publication timestamp', () => {
		expect(taskProgressSignalOf({ event_type: 'ExecutionPanelDelta', data: {
			principal: 'owner', workspace: 'work', task_id: 'task-1', execution_id: 'exec-1',
			timestamp: 999_999, state: panel([['tool.succeeded', 200_000]])
		} })?.at).toBe(200_000);
	});
});
