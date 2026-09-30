import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

export const load: PageLoad = ({ url }) => {
	const query = url.searchParams.toString();
	throw redirect(307, query ? `/today?${query}` : '/today');
};
