import { describe, expect, it } from 'vitest';
import {
	INTERNAL_TASKS_ROUTE,
	internalTaskRoute,
	legacyInternalTasksRedirect,
	MONITORS_TASKS_ROUTE,
	monitorsTaskRoute,
	resolveTasksRouteView
} from './taskRoutes';

describe('task routes', () => {
	it('uses the regular task workspace unless the internal/monitors type is explicit', () => {
		expect(resolveTasksRouteView(new URLSearchParams())).toBe('tasks');
		expect(resolveTasksRouteView(new URLSearchParams('type=scheduled'))).toBe('tasks');
		expect(resolveTasksRouteView(new URLSearchParams('type=internal'))).toBe('internal');
		expect(resolveTasksRouteView(new URLSearchParams('type=monitors'))).toBe('monitors');
	});

	it('builds canonical monitor deep links (list, monitor, exact update)', () => {
		expect(monitorsTaskRoute()).toBe(MONITORS_TASKS_ROUTE);
		expect(monitorsTaskRoute('task_monitor_1')).toBe(
			'/tasks?type=monitors&selected=task_monitor_1'
		);
		expect(monitorsTaskRoute('task_monitor_1', 'mu_71d3f6a2c4e89b10')).toBe(
			'/tasks?type=monitors&selected=task_monitor_1&update=mu_71d3f6a2c4e89b10'
		);
		// An update id without its monitor cannot resolve — fall back to the list.
		expect(monitorsTaskRoute(undefined, 'mu_71d3f6a2c4e89b10')).toBe(MONITORS_TASKS_ROUTE);
	});

	it('builds canonical links for internal task selections', () => {
		expect(internalTaskRoute()).toBe(INTERNAL_TASKS_ROUTE);
		expect(internalTaskRoute('task id/7')).toBe(
			'/tasks?type=internal&selected=task+id%2F7'
		);
	});

	it('preserves legacy route parameters while forcing the canonical task type', () => {
		const params = new URLSearchParams('selected=task-7&type=tasks&status=failed');
		expect(legacyInternalTasksRedirect(params)).toBe(
			'/tasks?type=internal&selected=task-7&status=failed'
		);
	});
});
