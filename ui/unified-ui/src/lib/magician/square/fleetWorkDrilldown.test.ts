import { describe, expect, it } from 'vitest';
import type { CitizenCurrentWorkVM } from './engine/types';
import { taskToQuest, type Quest } from './fleetQuests';
import { buildCitizenWorkDrilldown, buildGuildWorkDrilldown } from './fleetWorkDrilldown';

function quest(value: Record<string, unknown>): Quest {
	const mapped = taskToQuest({
		created_at: '2026-07-10T10:00:00Z',
		updated_at: '2026-07-10T11:00:00Z',
		...value
	});
	if (!mapped) throw new Error('fixture did not map');
	return mapped;
}

function currentWork(questId: string, overrides: Partial<CitizenCurrentWorkVM> = {}): CitizenCurrentWorkVM {
	return {
		questId,
		title: `Work ${questId}`,
		status: 'running',
		executionId: `exec-${questId}`,
		currentStep: 'Initial step',
		currentSubstep: null,
		isBlocked: false,
		updatedAt: '2026-07-10T11:00:00Z',
		...overrides
	};
}

describe('fleet work drill-down', () => {
	it('joins current work to canonical task context and surfaces the pending blocker', () => {
		const active = quest({
			id: 'task-1',
			title: 'Launch campaign',
			description: 'Publish the reviewed launch across every approved channel.',
			status: 'paused',
			agent_id: 'cmo',
			current_step_title: 'Review assets',
			current_substep_title: 'Confirm final logo',
			pending_questions: [{ id: 'q-1', prompt: 'Which logo should ship?' }],
			completion_artifact_names: ['launch-plan.md']
		});

		const model = buildCitizenWorkDrilldown(
			{ id: 'cmo', currentWork: [currentWork('task-1')] },
			[active]
		);

		expect(model.focus).toMatchObject({
			taskId: 'task-1',
			objective: 'Publish the reviewed launch across every approved channel.',
			currentStep: 'Review assets · Confirm final logo',
			blocker: 'Which logo should ship?',
			artifacts: ['launch-plan.md']
		});
	});

	it('keeps a bounded current-work list and carries the latest real outcome', () => {
		const quests = [
			quest({ id: 'active-1', title: 'One', status: 'running', agent_id: 'cto' }),
			quest({ id: 'active-2', title: 'Two', status: 'planning', agent_id: 'cto' }),
			quest({ id: 'active-3', title: 'Three', status: 'waiting', agent_id: 'cto' }),
			quest({ id: 'active-4', title: 'Four', status: 'paused', agent_id: 'cto' }),
			quest({
				id: 'done',
				title: 'Shipped foundation',
				status: 'completed',
				agent_id: 'cto',
				completion_summary: 'Foundation shipped successfully.',
				completion_artifact_names: ['report.md', 'build.zip', 'notes.txt', 'trace.json', 'extra.log'],
				updated_at: '2026-07-10T12:00:00Z'
			})
		];

		const model = buildCitizenWorkDrilldown({ id: 'cto', currentWork: [] }, quests);

		expect(model.activeCount).toBe(4);
		expect(model.additionalCurrent).toHaveLength(2);
		expect(model.latestOutcome?.outcome).toBe('Foundation shipped successfully.');
		expect(model.latestOutcome?.artifacts).toHaveLength(4);
	});

	it('limits program work to assigned crew members', () => {
		const model = buildGuildWorkDrilldown(['cto', 'engineer'], [
			quest({ id: 'inside', title: 'Inside', status: 'running', agent_id: 'engineer' }),
			quest({ id: 'outside', title: 'Outside', status: 'running', agent_id: 'cmo' }),
			quest({ id: 'done', title: 'Delivered', status: 'completed', agent_id: 'cto' })
		]);

		expect(model.focus?.taskId).toBe('inside');
		expect(model.activeCount).toBe(1);
		expect(model.latestOutcome?.taskId).toBe('done');
	});
});
