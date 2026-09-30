import { redirect } from '@sveltejs/kit';
import { INTERNAL_TASKS_ROUTE } from '$lib/magician/tasks/taskRoutes';

// Internal tasks now share the canonical Tasks workspace.
export function load() {
	throw redirect(308, INTERNAL_TASKS_ROUTE);
}
