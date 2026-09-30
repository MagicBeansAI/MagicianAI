import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

/**
 * The thread workspace lives at canonical sub-routes
 * `/t/<id>/{chat,tasks,settings}`. Bare `/t/<id>` redirects to the chat tab
 * (the default surface), preserving the query string. A `?selected=<taskId>`
 * deep-link targets a task, so it lands on the Tasks tab (which owns the task
 * execution panel) instead.
 */
export const load: PageLoad = ({ params, url }) => {
	const sub = url.searchParams.has('selected') ? 'tasks' : 'chat';
	const base = `/t/${encodeURIComponent(params.name)}/${sub}`;
	const query = url.searchParams.toString();
	throw redirect(308, query ? `${base}?${query}` : base);
};
