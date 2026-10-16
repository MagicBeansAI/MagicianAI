// Pure planner helpers shared by PlannerDock and ChatPanel
// (ChatPanel still needs taskPendingQuestions for the composer
// plan-reply flow and titleCase for the thread plan sheet).
// Moved verbatim from ChatPanel.svelte during the Stage E decomposition.

import type { Task } from '$lib/stores/taskStore';

export function titleCase(value: string): string {
	return value.replace(/_/g, ' ').replace(/\b\w/g, (match) => match.toUpperCase());
}

export function formatRelative(isoValue: string | undefined): string {
	const timestamp = isoValue ? Date.parse(isoValue) : NaN;
	if (!Number.isFinite(timestamp)) return 'just now';
	const diffMs = Date.now() - timestamp;
	const minutes = Math.round(diffMs / 60_000);
	if (Math.abs(minutes) < 1) return 'just now';
	if (Math.abs(minutes) < 60) return `${Math.abs(minutes)}m ago`;
	const hours = Math.round(minutes / 60);
	if (Math.abs(hours) < 48) return `${Math.abs(hours)}h ago`;
	const days = Math.round(hours / 24);
	return `${Math.abs(days)}d ago`;
}

export function taskPendingQuestions(task: Task | null | undefined): Array<{ id: string; question: string }> {
	if (!task) return [];
	if (task.pendingQuestions && task.pendingQuestions.length > 0) {
		return task.pendingQuestions;
	}
	return task.pendingQuestion ? [task.pendingQuestion] : [];
}

function plannerDockRank(task: Task): number {
	switch (task.planStatus) {
		case 'eliciting':
			return 0;
		case 'draft':
			return 1;
		default:
			return 2;
	}
}

export function plannerDockTasks(tasks: Task[]): Task[] {
	return tasks
		.filter((task) =>
			(task.planStatus === 'eliciting' && taskPendingQuestions(task).length > 0)
			|| task.planStatus === 'draft'
		)
		.sort((left, right) => {
			const rankDelta = plannerDockRank(left) - plannerDockRank(right);
			if (rankDelta !== 0) return rankDelta;
			const leftUpdated = Date.parse(left.updatedAt || left.createdAt || '') || 0;
			const rightUpdated = Date.parse(right.updatedAt || right.createdAt || '') || 0;
			return rightUpdated - leftUpdated;
		});
}

export function planStatusColor(status: Task['planStatus']): 'default' | 'success' | 'warning' | 'error' | 'info' {
	switch (status) {
		case 'approved':
			return 'success';
		case 'planning':
			return 'info';
		case 'eliciting':
		case 'draft':
			return 'warning';
		case 'failed':
		case 'rejected':
			return 'error';
		default:
			return 'default';
	}
}

export function planStatusHeadline(task: Task): string {
	switch (task.planStatus) {
		case 'planning':
			return 'Building plan';
		case 'eliciting':
			return 'Waiting for clarification';
		case 'draft':
			return 'Plan ready for review';
		case 'approved':
			return task.status === 'ready' ? 'Plan approved and ready' : 'Plan approved';
		case 'rejected':
			return 'Plan rejected';
		case 'failed':
			return 'Planning failed';
		default:
			return 'Task plan';
	}
}

export function planStatusSummary(task: Task): string {
	const pendingQuestions = taskPendingQuestions(task);
	switch (task.planStatus) {
		case 'planning':
			return 'The planner is running through the staged V3 flow for this task.';
		case 'eliciting':
			return pendingQuestions.length > 1
				? `The planner is waiting on ${pendingQuestions.length} answers before it can finalize the plan.`
				: pendingQuestions[0]?.question || 'The planner needs one more answer before it can finalize the plan.';
		case 'draft':
			return 'The plan is ready for review, editing, approval, or replanning.';
		case 'approved':
			return task.status === 'ready'
				? 'The approved plan can now be executed.'
				: 'The approved plan is waiting for the task to become ready.';
		case 'rejected':
			return 'This plan was rejected. Start a new planning pass when ready.';
		case 'failed':
			return task.errorMessage || 'The latest planning attempt failed.';
		default:
			return task.description || task.title;
	}
}
