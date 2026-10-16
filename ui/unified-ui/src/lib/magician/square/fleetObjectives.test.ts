import { describe, expect, it } from 'vitest';
import { buildQuestDependencyGraph, chooseFleetObjective } from './fleetObjectives';
import { taskToQuest, type Quest } from './fleetQuests';

function quest(value: Record<string, unknown>): Quest {
	const mapped = taskToQuest({
		created_at: '2026-07-10T10:00:00Z',
		updated_at: '2026-07-10T11:00:00Z',
		...value
	});
	if (!mapped) throw new Error('fixture did not map');
	return mapped;
}

describe('fleet objectives', () => {
	it('prioritizes an operator decision over ordinary active work', () => {
		const active = quest({ id: 'active', title: 'Build', status: 'running' });
		const decision = quest({
			id: 'decision',
			title: 'Release',
			status: 'paused',
			pending_questions: [{ id: 'q1', prompt: 'Ship this release?' }]
		});

		expect(chooseFleetObjective([active, decision])).toMatchObject({
			title: 'Release',
			detail: 'Ship this release?',
			actionLabel: 'Review decision',
			urgency: 'critical'
		});
	});

	it('builds prerequisite and dependent links without inventing missing tasks', () => {
		const foundation = quest({ id: 'foundation', title: 'Foundation', status: 'completed' });
		const launch = quest({
			id: 'launch',
			title: 'Launch',
			status: 'pending',
			depends_on: ['foundation', 'outside-scope']
		});
		const graph = buildQuestDependencyGraph([foundation, launch]);

		expect(graph.find((node) => node.quest.id === 'launch')?.prerequisites.map((q) => q.id)).toEqual([
			'foundation'
		]);
		expect(graph.find((node) => node.quest.id === 'foundation')?.dependents.map((q) => q.id)).toEqual([
			'launch'
		]);
	});
});
