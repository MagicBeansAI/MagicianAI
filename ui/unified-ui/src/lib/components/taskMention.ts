import type { Task } from '$lib/stores/taskStore';

type MentionTask = Pick<Task, 'id' | 'title' | 'status'>;

export function buildTaskLookup(tasks: MentionTask[]): Map<string, MentionTask> {
	return new Map(tasks.map((task) => [task.id, task]));
}

export function sanitizeDependsOn(
	dependsOn: string[],
	taskLookup: Map<string, MentionTask>
): string[] {
	return dependsOn.filter((taskId) => taskLookup.has(taskId));
}

export function buildSelectedTaskMap(
	dependsOn: string[],
	taskLookup: Map<string, MentionTask>
): Map<string, MentionTask> {
	return new Map(
		dependsOn
			.map((taskId) => {
				const task = taskLookup.get(taskId);
				return task ? ([taskId, task] as const) : null;
			})
			.filter((entry): entry is readonly [string, MentionTask] => entry !== null)
	);
}
