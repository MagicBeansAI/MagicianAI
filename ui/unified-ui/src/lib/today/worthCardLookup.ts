/**
 * Worth-a-look rich-card lookup shared by Today and Town Square.
 *
 * The canonical lane needs the legacy card to know which contextual actions
 * a candidate supports. The endpoint clamps `limit` to 100, so a single
 * request cannot cover a larger lane. Both pages page through until the
 * lane is covered, bounded so a cursor that never terminates cannot spin.
 */

import type { ResurfacingCard, ResurfacingCursor } from './resurfacingQueries';

export const WORTH_CARD_LOOKUP_PAGE_SIZE = 100;
export const WORTH_CARD_LOOKUP_MAX_PAGES = 6;

export interface WorthCardLookupPage {
	cards: ResurfacingCard[];
	next_cursor?: ResurfacingCursor | null;
}

export async function collectWorthCardLookup(
	fetchPage: (args: {
		limit: number;
		cursor: ResurfacingCursor | null;
	}) => Promise<WorthCardLookupPage>
): Promise<ResurfacingCard[]> {
	const collected: ResurfacingCard[] = [];
	let cursor: ResurfacingCursor | null = null;
	for (let page = 0; page < WORTH_CARD_LOOKUP_MAX_PAGES; page += 1) {
		const result = await fetchPage({
			limit: WORTH_CARD_LOOKUP_PAGE_SIZE,
			cursor
		});
		const cards = Array.isArray(result.cards) ? result.cards : [];
		collected.push(...cards);
		if (!result.next_cursor) break;
		cursor = result.next_cursor;
	}
	return collected;
}
