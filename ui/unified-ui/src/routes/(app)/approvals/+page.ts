/**
 * `/approvals` is retired post Phase G (magician v0.6.567+,
 * unified-ui v0.0.331+). The unified `/attention` page is the
 * canonical operator surface for every HITL source — approval,
 * clarification, plan_approval, agentic, user_request, escalation,
 * bot_auth. Hard 301 to keep bookmarks and external links pointing
 * at the right URL; system isn't in production so muscle-memory cost
 * is zero.
 */
import { redirect } from '@sveltejs/kit';
import type { PageLoad } from './$types';

export const load: PageLoad = ({ url }) => {
	const params = new URLSearchParams(url.searchParams);
	if (!params.get('attention_item')) {
		const legacyItemId = params.get('approval_id') || params.get('correlation_id');
		if (legacyItemId?.trim()) params.set('attention_item', legacyItemId.trim());
	}
	if (params.get('attention_item')) params.set('attention', '1');
	const query = params.toString();
	throw redirect(301, query ? `/attention?${query}` : '/attention');
};
