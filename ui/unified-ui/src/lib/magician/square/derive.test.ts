import { describe, expect, it } from 'vitest';
import type { AgentSummary } from '$lib/stores/agentStore';
import type { CitizenCurrentWorkVM } from './engine/types';
import { citizenVibeOf, displayNameOf, vibeOf } from './derive';
import { selectHandoffTrees } from './fleetHandoffs';

function agent(overrides: Partial<AgentSummary> = {}): AgentSummary {
	return {
		agent_id: 'agent-1',
		status: 'idle',
		updated_at: 1,
		...overrides
	};
}

function work(status: string, overrides: Partial<CitizenCurrentWorkVM> = {}): CitizenCurrentWorkVM {
	return {
		questId: 'task-1',
		title: 'Task',
		status,
		executionId: null,
		currentStep: null,
		currentSubstep: null,
		isBlocked: false,
		updatedAt: '2026-07-11T00:00:00Z',
		...overrides
	};
}

describe('vibeOf', () => {
	it('keeps a paused execution paused even while its execution id is retained', () => {
		expect(
			vibeOf(
				agent({
					status: 'paused',
					current_execution_id: 'execution-1'
				})
			)
		).toBe('paused');
	});

	it('does not interpret inactive-like status text as active work', () => {
		expect(vibeOf(agent({ status: 'inactive' as AgentSummary['status'] }))).toBe('idle');
	});
});

describe('citizenVibeOf', () => {
	it('does not treat ready or terminal task records as live work', () => {
		expect(citizenVibeOf(agent(), [work('ready'), work('completed')], false)).toBe('idle');
	});

	it('uses the global Attention signal before task execution state', () => {
		expect(citizenVibeOf(agent(), [work('ready', { executionId: 'exec-1' })], true)).toBe('needs');
	});

	it('treats blocked planning and running work distinctly', () => {
		expect(citizenVibeOf(agent(), [work('planning', { isBlocked: true })], false)).toBe('paused');
		expect(citizenVibeOf(agent(), [work('running', { executionId: 'exec-1' })], false)).toBe('working');
	});
});

describe('displayNameOf', () => {
	it('uses the explicit agent name and never persona prompt text', () => {
		expect(
			displayNameOf(
				agent({
					name: 'Nova',
					persona: 'You are a detailed system prompt that must never become a display name.'
				})
			)
		).toBe('Nova');
	});

	it('falls back to the stable agent id when no explicit name exists', () => {
		expect(displayNameOf(agent({ agent_id: 'web-researcher', name: '   ', persona: 'You are...' }))).toBe(
			'web-researcher'
		);
	});
});

describe('selectHandoffTrees', () => {
	it('selects only live task trees and never falls back to historical roots', () => {
		expect(
			selectHandoffTrees([
				{ id: 'task-running', status: 'running', active_root_execution_id: 'exec-active' },
				{ id: 'task-paused', status: 'paused', latest_root_execution_id: 'exec-latest' },
				{ id: 'task-no-tree', status: 'planning' },
				{ id: 'task-complete', status: 'completed', active_root_execution_id: 'exec-old' }
			])
		).toEqual([{ taskId: 'task-running', rootExecutionId: 'exec-active' }]);
	});
});
