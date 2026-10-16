import { describe, expect, it } from 'vitest';
import type { Task, TaskPlanStatus } from '$lib/stores/taskStore';
import { plannerDockTasks } from './planHelpers';

function task(id: string, planStatus: TaskPlanStatus, updatedAt: string): Task {
	return {
		id,
		title: id,
		description: '',
		status: planStatus === 'eliciting' || planStatus === 'planning' ? 'planning' : 'pending',
		tags: [],
		source: 'task',
		createdAt: updatedAt,
		updatedAt,
		planStatus
	};
}

describe('plannerDockTasks', () => {
	it('keeps only planner work that requires user attention', () => {
		const tasks = plannerDockTasks([
			task('planning-new', 'planning', '2026-07-11T10:00:00Z'),
			task('draft', 'draft', '2026-07-11T09:00:00Z'),
			{
				...task('question-old', 'eliciting', '2026-07-11T08:00:00Z'),
				pendingQuestions: [{ id: 'question-1', question: 'Which domain?' }]
			},
			task('empty-eliciting', 'eliciting', '2026-07-11T08:30:00Z'),
			task('approved', 'approved', '2026-07-11T11:00:00Z'),
			task('failed', 'failed', '2026-07-11T12:00:00Z')
		]);

		expect(tasks.map((entry) => entry.id)).toEqual([
			'question-old',
			'draft'
		]);
	});

	it('orders tasks in the same planner state newest first', () => {
		const tasks = plannerDockTasks([
			{
				...task('older', 'eliciting', '2026-07-11T08:00:00Z'),
				pendingQuestions: [{ id: 'question-1', question: 'Older?' }]
			},
			{
				...task('newer', 'eliciting', '2026-07-11T09:00:00Z'),
				pendingQuestions: [{ id: 'question-2', question: 'Newer?' }]
			}
		]);

		expect(tasks.map((entry) => entry.id)).toEqual(['newer', 'older']);
	});
});
