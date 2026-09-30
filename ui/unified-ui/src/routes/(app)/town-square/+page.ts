/**
 * `/town-square` is retired as a standalone page: the social feed now lives
 * as the Social tab on `/square` (compact, single column). Hard 301 keeps
 * bookmarks, the TopBar muscle memory of yore, and any in-flight links
 * landing on the right surface.
 */
import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

export const load: PageLoad = () => {
	throw redirect(301, '/square?tab=social');
};
