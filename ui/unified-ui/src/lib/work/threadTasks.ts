import { computeFilteredTasks, type Task, type TaskFilter, type TaskLane } from '$lib/stores/taskStore';
import { normalizeThreadId } from '$lib/threads/normalizeThreadId';

export function tasksInThread(tasks: Task[], threadId: string | null): Task[] {
 return threadId ? tasks.filter(task => normalizeThreadId(task.uiThreadId) === normalizeThreadId(threadId)) : tasks;
}
function bucket(task: Task): number {
 if (task.status === 'running' || task.status === 'planning') return 0;
 if (task.status === 'paused' || task.status === 'failed') return 1;
 if (task.status === 'ready') return 2;
 if (task.status === 'completed') return 4;
 return 3;
}
/** Preserve thread execution-first ordering using the shared lane/grace rules. */
export function filteredThreadTasks(tasks: Task[], filter: TaskFilter, search: string, pending: Set<string>): Task[] {
 return computeFilteredTasks(tasks, filter, search, pending).sort((a, b) => bucket(a) - bucket(b) || (Date.parse(b.updatedAt || b.createdAt || '') || 0) - (Date.parse(a.updatedAt || a.createdAt || '') || 0));
}
export function threadTaskCounts(tasks: Task[], pending: Set<string>): Record<TaskLane, number> {
 return Object.fromEntries(['all', 'inbox', 'today', 'overdue', 'running', 'completed'].map(lane => [lane, computeFilteredTasks(tasks, lane as TaskLane, '', pending).length])) as Record<TaskLane, number>;
}
