import { describe, expect, it, vi } from 'vitest';

import type { ResurfacingCard, ResurfacingCursor } from './resurfacingQueries';
import {
	WORTH_CARD_LOOKUP_MAX_PAGES,
	WORTH_CARD_LOOKUP_PAGE_SIZE,
	collectWorthCardLookup
} from './worthCardLookup';

function card(id: string): ResurfacingCard {
	return {
		candidate_id: id,
		line: id,
		why_now: '',
		source_kind: 'memory',
		source_ref: id,
		source_title: id,
		summary: id,
		detail_label: id,
		temporal_anchor_at: null,
		brief: null,
		brief_status: 'v2',
		content_revision: null,
		source_updated: false,
		recommended_action: null,
		actions: []
	};
}

function cursor(id: string): ResurfacingCursor {
	return { surfaced_at: 1, score: 1, candidate_id: id };
}

describe('collectWorthCardLookup', () => {
	it('asks for the first 100 cards and stops when the cursor is exhausted', async () => {
		const fetchPage = vi.fn().mockResolvedValue({
			cards: [card('a'), card('b')],
			next_cursor: null
		});
		await expect(collectWorthCardLookup(fetchPage)).resolves.toEqual([card('a'), card('b')]);
		expect(fetchPage).toHaveBeenCalledTimes(1);
		expect(fetchPage).toHaveBeenCalledWith({
			limit: WORTH_CARD_LOOKUP_PAGE_SIZE,
			cursor: null
		});
	});

	it('walks successive cursors so Square can join the whole visible lane', async () => {
		const fetchPage = vi
			.fn()
			.mockResolvedValueOnce({
				cards: [card('page-1')],
				next_cursor: cursor('page-1')
			})
			.mockResolvedValueOnce({
				cards: [card('page-2')],
				next_cursor: null
			});
		await expect(collectWorthCardLookup(fetchPage)).resolves.toEqual([
			card('page-1'),
			card('page-2')
		]);
		expect(fetchPage).toHaveBeenNthCalledWith(2, {
			limit: 100,
			cursor: cursor('page-1')
		});
	});

	it('caps the walk so a never-ending cursor cannot spin', async () => {
		const fetchPage = vi.fn().mockImplementation(
			async ({ cursor: current }: { cursor: ResurfacingCursor | null }) => ({
				cards: [card(current?.candidate_id ?? 'first')],
				next_cursor: cursor(`page-${fetchPage.mock.calls.length}`)
			})
		);
		const cards = await collectWorthCardLookup(fetchPage);
		expect(fetchPage).toHaveBeenCalledTimes(WORTH_CARD_LOOKUP_MAX_PAGES);
		expect(cards).toHaveLength(WORTH_CARD_LOOKUP_MAX_PAGES);
	});
});
