import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { fetchPageMock } = vi.hoisted(() => ({ fetchPageMock: vi.fn() }));

vi.mock('$lib/attention/attentionDelivery', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/attention/attentionDelivery')>();
	return { ...actual, fetchAttentionDeliveryPage: fetchPageMock };
});

import {
	deliveryFollowUpItem,
	deliveryPage,
	deliveryProjection,
	deliveryRoot
} from '$lib/attention/attentionDelivery.testFixtures';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { attentionDeliveryStore } from './attentionDeliveryStore';

const DEFAULT_SCOPE_KEY = ['anonymous', 'default'].join('\u0000');

beforeEach(() => {
	fetchPageMock.mockReset();
	scopeIdentityStore.reset();
	attentionDeliveryStore.clear();
});

afterEach(() => {
	attentionDeliveryStore.clear();
	scopeIdentityStore.reset();
});

describe('attentionDeliveryStore', () => {
	it('appends only the next contiguous page in server position order', async () => {
		const projection = deliveryProjection([
			deliveryFollowUpItem(3),
			deliveryFollowUpItem(1),
			deliveryFollowUpItem(2)
		]);
		const root = deliveryRoot(projection);
		const first = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(0, 2),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: 'opaque-next'
		});
		const second = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(2),
			pageIndex: 1,
			pageStart: 2,
			pageSize: 2,
			cursor: 'opaque-next',
			nextCursor: null
		});
		fetchPageMock
			.mockResolvedValueOnce({ kind: 'page', response: first })
			.mockResolvedValueOnce({ kind: 'page', response: second });

		await attentionDeliveryStore.ensure('follow_up', DEFAULT_SCOPE_KEY, projection, 2);
		await attentionDeliveryStore.loadMore('follow_up');

		const lane = get(attentionDeliveryStore).follow_up;
		expect(lane.items.map((item) => item.candidate_id)).toEqual([
			'follow_up:ann-3',
			'follow_up:ann-1',
			'follow_up:ann-2'
		]);
		expect(lane.pages.map((page) => page.page.page_index)).toEqual([0, 1]);
		expect(fetchPageMock.mock.calls[1]?.[0]).toMatchObject({ cursor: 'opaque-next' });
	});

	it('resets and quietly requests a fresh root when a later page is not contiguous', async () => {
		const projection = deliveryProjection([
			deliveryFollowUpItem(1),
			deliveryFollowUpItem(2),
			deliveryFollowUpItem(3)
		]);
		const root = deliveryRoot(projection);
		const first = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(0, 1),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 1,
			cursor: null,
			nextCursor: 'opaque-next'
		});
		const drifted = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(2),
			pageIndex: 1,
			pageStart: 2,
			pageSize: 1,
			cursor: 'opaque-next',
			nextCursor: null
		});
		fetchPageMock
			.mockResolvedValueOnce({ kind: 'page', response: first })
			.mockResolvedValueOnce({ kind: 'page', response: drifted })
			.mockResolvedValueOnce({ kind: 'page', response: first });

		await attentionDeliveryStore.ensure('follow_up', DEFAULT_SCOPE_KEY, projection, 1);
		await attentionDeliveryStore.loadMore('follow_up');
		await vi.waitFor(() => expect(fetchPageMock).toHaveBeenCalledTimes(3));

		const lane = get(attentionDeliveryStore).follow_up;
		expect(lane.pages).toHaveLength(1);
		expect(lane.items.map((item) => item.candidate_id)).toEqual(['follow_up:ann-1']);
		expect(lane.root?.decision_id).toBe(root.decision_id);
	});

	it('handles a typed cursor refresh-required without presenting the stale append', async () => {
		const projection = deliveryProjection();
		const root = deliveryRoot(projection);
		const first = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(0, 1),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 1,
			cursor: null,
			nextCursor: 'opaque-next'
		});
		fetchPageMock
			.mockResolvedValueOnce({ kind: 'page', response: first })
			.mockResolvedValueOnce({
				kind: 'refresh_required',
				refresh: {
					schema_version: 1,
					status: 'refresh_required',
					error: 'attention_delivery_refresh_required',
					reason: 'revision_drift',
					lane: 'follow_up',
					refresh_href: '/api/magician/v2/channel-assist/attention-learning/canonical-deliveries/follow_up'
				}
			})
			.mockResolvedValueOnce({ kind: 'page', response: first });

		await attentionDeliveryStore.ensure('follow_up', DEFAULT_SCOPE_KEY, projection, 1);
		await attentionDeliveryStore.loadMore('follow_up');
		await vi.waitFor(() => expect(fetchPageMock).toHaveBeenCalledTimes(3));

		expect(get(attentionDeliveryStore).follow_up.items).toHaveLength(1);
		expect(get(attentionDeliveryStore).follow_up.pages).toHaveLength(1);
	});

	it('discards a late first page after the principal/workspace changes', async () => {
		const projection = deliveryProjection();
		const root = deliveryRoot(projection);
		const first = deliveryPage({
			root,
			items: projection.lanes.follow_up.slice(0, 1),
			pageIndex: 0,
			pageStart: 0,
			pageSize: 1,
			cursor: null,
			nextCursor: 'opaque-next'
		});
		let resolveFetch!: (result: { kind: 'page'; response: typeof first }) => void;
		fetchPageMock.mockReturnValueOnce(new Promise((resolve) => { resolveFetch = resolve; }));

		const pending = attentionDeliveryStore.ensure('follow_up', DEFAULT_SCOPE_KEY, projection, 1);
		scopeIdentityStore.observe('other-principal', 'other-workspace');
		resolveFetch({ kind: 'page', response: first });
		await pending;

		const lane = get(attentionDeliveryStore).follow_up;
		expect(lane.root).toBeNull();
		expect(lane.items).toEqual([]);
		expect(lane.isLoading).toBe(false);
		expect(lane.fallbackReason).toBe('scope_mismatch');
	});

	it('keeps the visible page while a same-scope refresh is in flight', async () => {
		const firstProjection = deliveryProjection([deliveryFollowUpItem(1), deliveryFollowUpItem(2)]);
		const firstRoot = deliveryRoot(firstProjection);
		const firstPage = deliveryPage({
			root: firstRoot,
			items: firstProjection.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		const nextProjection = deliveryProjection([deliveryFollowUpItem(2)]);
		nextProjection.projection_id = 'projection-delivery-2';
		nextProjection.universe_digest = 'universe-digest-2';
		const nextRoot = deliveryRoot(nextProjection);
		const nextPage = deliveryPage({
			root: nextRoot,
			items: nextProjection.lanes.follow_up,
			pageIndex: 0,
			pageStart: 0,
			pageSize: 2,
			cursor: null,
			nextCursor: null
		});
		let resolveRefresh!: (result: { kind: 'page'; response: typeof nextPage }) => void;
		fetchPageMock
			.mockResolvedValueOnce({ kind: 'page', response: firstPage })
			.mockReturnValueOnce(new Promise((resolve) => { resolveRefresh = resolve; }));

		await attentionDeliveryStore.ensure('follow_up', DEFAULT_SCOPE_KEY, firstProjection, 2);
		const pending = attentionDeliveryStore.refresh('follow_up', DEFAULT_SCOPE_KEY, nextProjection, 2);
		expect(get(attentionDeliveryStore).follow_up.isLoading).toBe(true);
		expect(get(attentionDeliveryStore).follow_up.items.map((item) => item.candidate_id)).toEqual([
			'follow_up:ann-1',
			'follow_up:ann-2'
		]);

		resolveRefresh({ kind: 'page', response: nextPage });
		await pending;
		expect(get(attentionDeliveryStore).follow_up.items.map((item) => item.candidate_id)).toEqual([
			'follow_up:ann-2'
		]);
	});
});
