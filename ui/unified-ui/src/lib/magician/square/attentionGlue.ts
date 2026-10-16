import type { FeedItem } from '$lib/feed/types';
import type { AttentionStoreState } from '$lib/stores/attentionStore';

/**
 * Glue between the attention funnel (/feed/attention via attentionStore) and
 * the Civilization game: groups needs-you items per citizen so the [!] bubble
 * opens straight onto the actual approvals/requests, actionable in-game.
 * Same id derivations the Today page and the old square feed used.
 */

export type NeedsLane = 'approvals' | 'requests' | 'escalations';

export interface NeedsItem {
	item: FeedItem;
	lane: NeedsLane;
}

export function readMetadataString(item: FeedItem, key: string): string | null {
	const meta = item.metadata;
	if (meta && typeof meta === 'object' && !Array.isArray(meta)) {
		const v = (meta as Record<string, unknown>)[key];
		if (typeof v === 'string' && v.length > 0) return v;
	}
	return null;
}

/** The approval id carried by an approval FeedItem (metadata, or `approval:<id>`). */
export function approvalIdOf(item: FeedItem): string | null {
	return (
		readMetadataString(item, 'approval_id') ||
		(item.id.startsWith('approval:') ? item.id.slice('approval:'.length) : null)
	);
}

/** Group the funnel's needs-you lanes by the agent they belong to. */
export function groupNeedsByAgent(state: AttentionStoreState): Map<string, NeedsItem[]> {
	const out = new Map<string, NeedsItem[]>();
	const add = (items: FeedItem[], lane: NeedsLane) => {
		for (const item of items) {
			const agent = item.agent_id;
			if (!agent) continue;
			const list = out.get(agent);
			const entry = { item, lane };
			if (list) list.push(entry);
			else out.set(agent, [entry]);
		}
	};
	add(state.approvals, 'approvals');
	add(state.requests, 'requests');
	add(state.escalations, 'escalations');
	return out;
}
