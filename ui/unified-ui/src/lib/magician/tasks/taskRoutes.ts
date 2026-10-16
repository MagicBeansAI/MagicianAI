export type TasksRouteView = 'tasks' | 'internal' | 'monitors';

export const TASKS_ROUTE = '/work';
export const INTERNAL_TASKS_ROUTE = '/work?type=internal';
export const MONITORS_TASKS_ROUTE = '/work?type=monitors';

export function resolveTasksRouteView(searchParams: URLSearchParams): TasksRouteView {
	const type = searchParams.get('type');
	if (type === 'internal') return 'internal';
	if (type === 'monitors') return 'monitors';
	return 'tasks';
}

export function internalTaskRoute(taskId?: string): string {
	const params = new URLSearchParams({ type: 'internal' });
	if (taskId?.trim()) params.set('selected', taskId.trim());
	return `${TASKS_ROUTE}?${params.toString()}`;
}

/**
 * Canonical monitor-surface deep link (plan §9.1): the list when called
 * bare, one monitor via `selected`, and an EXACT update via `update` — the
 * target Today `Changed` cards and Tauri notification opens resolve to.
 */
export function monitorsTaskRoute(taskId?: string, updateId?: string): string {
	const params = new URLSearchParams({ type: 'monitors' });
	const trimmedTaskId = taskId?.trim();
	if (trimmedTaskId) {
		params.set('selected', trimmedTaskId);
		if (updateId?.trim()) params.set('update', updateId.trim());
	}
	return `${TASKS_ROUTE}?${params.toString()}`;
}

export function legacyInternalTasksRedirect(searchParams: URLSearchParams): string {
	const params = new URLSearchParams({ type: 'internal' });
	searchParams.forEach((value, key) => {
		if (key !== 'type' && key !== 'view') params.append(key, value);
	});
	return `${TASKS_ROUTE}?${params.toString()}`;
}
