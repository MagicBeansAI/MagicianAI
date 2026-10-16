import { describe, expect, it } from 'vitest';

import {
	channelFollowUpPagination,
	shouldRenderChannelFollowUps
} from './channelFollowUpVisibility';

describe('message follow-up visibility', () => {
	it('becomes visible when an initially empty async request loads follow-ups', () => {
		const initial = shouldRenderChannelFollowUps({
			loading: false,
			error: null,
			total: 0,
			itemCount: 0
		});
		const loaded = shouldRenderChannelFollowUps({
			loading: false,
			error: null,
			total: 496,
			itemCount: 5
		});

		expect(initial).toBe(false);
		expect(loaded).toBe(true);
	});

	it.each([
		{ loading: true, error: null, total: 0, itemCount: 0 },
		{ loading: false, error: 'Message follow-ups unavailable.', total: 0, itemCount: 0 },
		{ loading: false, error: null, total: 1, itemCount: 0 },
		{ loading: false, error: null, total: 0, itemCount: 1 }
	])('keeps the panel mounted for a renderable state: %o', (state) => {
		expect(shouldRenderChannelFollowUps(state)).toBe(true);
	});

	it('updates pagination when the async total and items arrive', () => {
		expect(channelFollowUpPagination(0, 1, 0, 5)).toEqual({
			pageCount: 1,
			startItem: 0,
			endItem: 0
		});
		expect(channelFollowUpPagination(496, 1, 5, 5)).toEqual({
			pageCount: 100,
			startItem: 1,
			endItem: 5
		});
		expect(channelFollowUpPagination(496, 100, 1, 5)).toEqual({
			pageCount: 100,
			startItem: 496,
			endItem: 496
		});
	});
});
