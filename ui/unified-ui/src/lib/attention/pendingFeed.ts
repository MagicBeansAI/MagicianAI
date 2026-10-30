import type { FeedAttentionResponse } from '$lib/feed/types';

const lanes = ['requests', 'approvals', 'escalations'] as const;
type Lane = typeof lanes[number];
type Cursors = Partial<Record<Lane, string | null>>;

/** A composer needs all open asks; failed/running history stays paginated. */
export async function completePendingFeed(
	head: FeedAttentionResponse,
	read: (cursors: Cursors) => Promise<FeedAttentionResponse>
): Promise<FeedAttentionResponse> {
	if (!head.pages) return head;
	const result = { ...head, pages: { ...head.pages } };
	const cursors: Cursors = {};
	const seen = new Set<string>();
	for (const lane of lanes) cursors[lane] = head.pages?.[lane]?.next_cursor || null;
	while (lanes.some((lane) => cursors[lane])) {
		for (const lane of lanes) {
			const cursor = cursors[lane];
			if (!cursor) continue;
			const key = `${lane}\u0000${cursor}`;
			if (seen.has(key)) throw new Error('The server repeated a pending-question cursor. Refresh to retry.');
			seen.add(key);
		}
		const page = await read({ ...cursors });
		for (const lane of lanes) {
			if (!cursors[lane]) continue;
			result[lane] = [...new Map([...result[lane], ...page[lane]].map((item) => [item.id, item])).values()];
			if (page.pages?.[lane]) result.pages[lane] = page.pages[lane];
			cursors[lane] = page.pages?.[lane]?.next_cursor || null;
		}
	}
	return result;
}
