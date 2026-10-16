import { redirect } from '@sveltejs/kit';
import type { LayoutLoad } from './$types';

export const load: LayoutLoad = ({ params, url }) => {
	if (params.name?.toLowerCase() === 'vibedev') {
		const query = url.searchParams.toString();
		throw redirect(308, query ? `/vibe?${query}` : '/vibe');
	}
};
