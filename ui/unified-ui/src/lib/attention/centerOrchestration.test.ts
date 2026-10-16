import { describe, expect, it } from 'vitest';

import type { FeedItem } from '$lib/feed/types';
import type { AttentionStoreState } from '$lib/stores/attentionStore';
import {
	attentionFeedCanLoadMore,
	attentionFeedLaneNeedsAdvance,
	attentionFrontierSignature,
	attentionItemLaunchFailure,
	attentionKnownTotal,
	attentionSourceFrontiers,
	compactAttentionFeedback,
	planAttentionNextPage
} from './centerOrchestration';

function row(id: string): FeedItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'task',
		title: id,
		status: 'needs_action',
		created_at: 1,
		updated_at: 1,
		actions: [],
		metadata: {}
	};
}

function state(overrides: Partial<AttentionStoreState> = {}): AttentionStoreState {
	const page = { total: 0, limit: 25, cursor: null, next_cursor: null, has_more: false };
	return {
		isLoading: false,
		error: null,
		expanded: false,
		lastLoadedAt: null,
		counts: {
			requests: 0,
			approvals: 0,
			escalations: 0,
			needs_action: 0,
			failed: 0,
			running: 0
		},
		totals: { requests: 0, approvals: 0, escalations: 0, failed: 0, running: 0 },
		limit: 25,
		pages: {
			requests: { ...page },
			approvals: { ...page },
			escalations: { ...page },
			failed: { ...page },
			running: { ...page }
		},
		requests: [],
		approvals: [],
		escalations: [],
		failed: [],
		running: [],
		...overrides
	};
}

describe('Attention center feedback and item lookup state', () => {
	it('drops empty feedback and assigns error severity from operational language', () => {
		expect(
			compactAttentionFeedback(
				null,
				'Page loaded.',
				'Attention feed unavailable',
				'Cursor request failed',
				'Unknown error'
			)
		).toEqual([
			{ kind: 'notice', message: 'Page loaded.' },
			{ kind: 'error', message: 'Attention feed unavailable' },
			{ kind: 'error', message: 'Cursor request failed' },
			{ kind: 'error', message: 'Unknown error' }
		]);
	});

	it.each([
		[null, 'not-found'],
		['feed unavailable', 'load-error']
	] as const)('projects item lookup failure from %s', (feedError, expected) => {
		expect(attentionItemLaunchFailure(feedError)).toBe(expected);
	});
});

describe('Attention center source availability', () => {
	it('loads only lanes owned by the active category', () => {
		const current = state({
			pages: {
				...state().pages,
				requests: { ...state().pages.requests, has_more: true },
				approvals: { ...state().pages.approvals, has_more: true }
			}
		});
		expect(attentionFeedCanLoadMore(current, 'all', 200)).toBe(true);
		expect(attentionFeedCanLoadMore(current, 'requests', 200)).toBe(true);
		expect(attentionFeedCanLoadMore(current, 'approvals', 200)).toBe(true);
		expect(attentionFeedCanLoadMore(current, 'escalations', 200)).toBe(false);
	});

	it('stops feed loading at the compact buffer cap', () => {
		const requests = Array.from({ length: 2 }, (_, index) => row(`request-${index}`));
		const current = state({
			requests,
			pages: {
				...state().pages,
				requests: { ...state().pages.requests, has_more: true }
			}
		});
		expect(attentionFeedCanLoadMore(current, 'requests', 2)).toBe(false);
		expect(attentionFeedCanLoadMore(current, 'requests', 3)).toBe(true);
	});

	it('builds category-specific source frontiers over the feed lanes only', () => {
		const current = state({
			requests: [row('request')],
			approvals: [row('approval-1'), row('approval-2')],
			pages: {
				...state().pages,
				requests: { ...state().pages.requests, has_more: true },
				approvals: { ...state().pages.approvals, has_more: false }
			}
		});
		expect(attentionSourceFrontiers(current, 'all', 200)).toEqual([
			{ loadedCount: 1, hasMore: true, bufferLimit: 200 },
			{ loadedCount: 2, hasMore: false, bufferLimit: 200 },
			{ loadedCount: 0, hasMore: false, bufferLimit: 200 },
			{ loadedCount: 0, hasMore: false, bufferLimit: 200 }
		]);
		expect(attentionSourceFrontiers(current, 'approvals', 10)).toEqual([
			{ loadedCount: 2, hasMore: false, bufferLimit: 10 }
		]);
	});

	it('detects when any category lane still needs proof advancement', () => {
		const current = state({
			requests: [row('request')],
			pages: {
				...state().pages,
				requests: { ...state().pages.requests, has_more: true }
			}
		});
		expect(attentionFeedLaneNeedsAdvance(current, 2, 'requests', 200)).toBe(true);
		expect(attentionFeedLaneNeedsAdvance(current, 1, 'requests', 200)).toBe(false);
	});

	it('changes frontier signatures only for sources owned by the category', () => {
		const current = state({ requests: [row('request')], approvals: [row('approval')] });
		expect(attentionFrontierSignature(current, 'all')).toBe('1:false:1:false:0:false:0:false');
		expect(attentionFrontierSignature(current, 'requests')).toBe('1:false');
	});
});

describe('Attention center totals and next-page decisions', () => {
	it.each([
		[3, 2, 1, 0, 3],
		[2, 5, 3, 2, 5],
		[12, 5, 3, 2, 12]
	] as const)('chooses the strongest known total', (combined, attentionRows, needsAction, failed, expected) => {
		expect(attentionKnownTotal(combined, attentionRows, needsAction, failed)).toBe(expected);
	});

	it('advances when the target page has a globally safe first row', () => {
		expect(planAttentionNextPage(1, 6, 7, false, true)).toEqual({
			pageIndex: 1,
			notice: null
		});
	});

	it('routes capped and exhausted states to distinct notices', () => {
		expect(planAttentionNextPage(2, 6, 12, true, true)).toEqual({
			pageIndex: null,
			notice: 'More items are available in full Attention.'
		});
		expect(planAttentionNextPage(2, 6, 12, false, false)).toEqual({
			pageIndex: null,
			notice: 'You have reached the end of the inbox.'
		});
	});

	it('keeps the page stable while an uncapped source can still advance', () => {
		expect(planAttentionNextPage(2, 6, 12, false, true)).toEqual({
			pageIndex: null,
			notice: null
		});
	});
});
