import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

export const load: PageLoad = ({ params, url }) => {
	if (params.name?.toLowerCase() === 'vibedev') {
		const query = url.searchParams.toString();
		throw redirect(308, query ? `/vibe?${query}` : '/vibe');
	}
};
