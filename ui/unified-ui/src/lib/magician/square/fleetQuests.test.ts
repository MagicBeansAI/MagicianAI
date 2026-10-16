import { describe, expect, it } from 'vitest';
import { questPhase, taskToQuest } from './fleetQuests';

describe('taskToQuest', () => {
	it('preserves task context and maps pending decisions into awaiting orders', () => {
		const quest = taskToQuest({
			id: 'task-1',
			title: 'Ship the campaign',
			description: 'Deliver a reviewed release',
			status: 'paused',
			agent_id: 'cto',
			priority: 'high',
			due_date: '2026-07-12T12:00:00Z',
			depends_on: ['task-0'],
			active_root_execution_id: 'exec-1',
			current_step_title: 'Review implementation',
			current_substep_title: 'Inspect UI',
			has_plan: true,
			approved_plan_id: 'plan-1',
			plan_status: 'approved',
			pending_questions: [{ id: 'q-1', prompt: 'Approve the release?' }],
			completion_artifact_names: ['release-notes.md'],
			created_at: '2026-07-10T10:00:00Z',
			updated_at: '2026-07-10T11:00:00Z'
		});

		expect(quest).toMatchObject({
			state: 'awaiting_orders',
			priority: 'high',
			dependsOn: ['task-0'],
			currentStep: 'Review implementation',
			currentSubstep: 'Inspect UI',
			planId: 'plan-1',
			artifacts: ['release-notes.md']
		});
		expect(quest?.pendingQuestions).toEqual([
			{ id: 'q-1', prompt: 'Approve the release?' }
		]);
		if (quest) expect(questPhase(quest)).toBe('waiting');
	});

	it('distinguishes synthesis delivery from completed work', () => {
		const delivering = taskToQuest({
			id: 'task-2',
			title: 'Build artifact',
			status: 'running',
			synthesis_pending: true,
			created_at: '2026-07-10T10:00:00Z',
			updated_at: '2026-07-10T11:00:00Z'
		});
		const completed = taskToQuest({
			id: 'task-3',
			title: 'Delivered artifact',
			status: 'completed',
			completion_summary: 'Artifact shipped',
			completion_outcome: 'success',
			created_at: '2026-07-10T10:00:00Z',
			updated_at: '2026-07-10T11:00:00Z'
		});

		expect(delivering?.state).toBe('delivering');
		expect(completed).toMatchObject({
			state: 'succeeded',
			completionSummary: 'Artifact shipped',
			completionOutcome: 'success'
		});
	});
});
