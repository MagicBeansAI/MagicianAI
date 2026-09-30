import { redirect } from '@sveltejs/kit';
import { legacyInternalTasksRedirect } from '$lib/magician/tasks/taskRoutes';

export function load({ url }: { url: URL }): never {
	throw redirect(308, legacyInternalTasksRedirect(url.searchParams));
}
