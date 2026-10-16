import { describe, expect, it, vi } from 'vitest';

import {
	attentionAggregateBufferCapped,
	attentionFrontiersBlockedByCap,
	attentionFrontiersProven,
	attentionGloballyProvenRowCount,
	attentionJumpLandingIndex,
	attentionPageRows,
	attentionPagerView,
	attentionSourceFrontierProven,
	clampAttentionPageIndex,
	createBalancedLifecycle,
	createGenerationGuard,
	createSerializedCursorLoader,
	mergeBoundedRows,
	planAttentionReviewHistory
} from './pagination';
import { ATTENTION_HISTORY_STATE_KEY } from './centerState';

describe('Attention center pagination', () => {
	it('returns finite six-row windows', () => {
		const rows = Array.from({ length: 14 }, (_, index) => index);
		expect(attentionPageRows(rows, 0)).toEqual([0, 1, 2, 3, 4, 5]);
		expect(attentionPageRows(rows, 1)).toEqual([6, 7, 8, 9, 10, 11]);
		expect(attentionPageRows(rows, 2)).toEqual([12, 13]);
	});

	it('derives ServerPager props from the server total while the cursor is live', () => {
		expect(
			attentionPagerView({
				pageIndex: 1,
				pageSize: 20,
				visibleRowCount: 20,
				provenRowCount: 40,
				categoryTotal: 137,
				hasServerMore: true
			})
		).toEqual({
			currentPage: 2,
			pageCount: 7,
			startItem: 21,
			endItem: 40,
			totalItems: 137
		});
	});

	it('falls back to loaded rows once the cursor is exhausted', () => {
		// The server total over-counts after client-side dismissals — the
		// loaded rows are the only truth left.
		expect(
			attentionPagerView({
				pageIndex: 0,
				pageSize: 20,
				visibleRowCount: 13,
				provenRowCount: 13,
				categoryTotal: 137,
				hasServerMore: false
			})
		).toEqual({
			currentPage: 1,
			pageCount: 1,
			startItem: 1,
			endItem: 13,
			totalItems: 13
		});
	});

	it('keeps one page reachable ahead when the total under-reports a live cursor', () => {
		const view = attentionPagerView({
			pageIndex: 2,
			pageSize: 6,
			visibleRowCount: 6,
			provenRowCount: 18,
			categoryTotal: 0,
			hasServerMore: true
		});

		expect(view.pageCount).toBe(4);
		expect(view.currentPage).toBe(3);
	});

	it('never advertises pages a buffer cap makes unreachable', () => {
		const view = attentionPagerView({
			pageIndex: 3,
			pageSize: 6,
			visibleRowCount: 6,
			provenRowCount: 24,
			categoryTotal: 500,
			hasServerMore: true,
			maxReachablePage: 4
		});

		expect(view.pageCount).toBe(4);
		expect(view.currentPage).toBe(4);
	});

	it('reports an empty range for a page that rendered no rows', () => {
		const view = attentionPagerView({
			pageIndex: 0,
			pageSize: 20,
			visibleRowCount: 0,
			provenRowCount: 0,
			categoryTotal: 0,
			hasServerMore: false
		});

		expect(view).toEqual({
			currentPage: 1,
			pageCount: 1,
			startItem: 0,
			endItem: 0,
			totalItems: 0
		});
	});

	it('lands an overshooting jump on the furthest proven page', () => {
		expect(attentionJumpLandingIndex(0, 6, 50, 20)).toBe(2);
		expect(attentionJumpLandingIndex(0, 1, 50, 20)).toBe(1);
	});

	it('never drags a forward jump backwards when rows shrank mid-flight', () => {
		expect(attentionJumpLandingIndex(3, 6, 12, 20)).toBe(3);
	});

	it('clamps a page after resolutions shrink the combined row count', () => {
		expect(clampAttentionPageIndex(2, 13)).toBe(2);
		expect(clampAttentionPageIndex(2, 12)).toBe(1);
		expect(clampAttentionPageIndex(4, 0)).toBe(0);
	});

	it('requires every non-exhausted source frontier to prove the global window', () => {
		const feed = { loadedCount: 25, hasMore: true, bufferLimit: 200 };
		const channel = { loadedCount: 6, hasMore: true, bufferLimit: 200 };

		expect(attentionFrontiersProven([feed, channel], 12)).toBe(false);
		expect(
			attentionFrontiersProven([feed, { ...channel, loadedCount: 12 }], 12)
		).toBe(true);
		expect(
			attentionSourceFrontierProven({ ...channel, hasMore: false }, 12)
		).toBe(true);
	});

	it('does not prove rows beyond a non-exhausted source buffer cap', () => {
		const capped = { loadedCount: 200, hasMore: true, bufferLimit: 200 };

		expect(attentionSourceFrontierProven(capped, 200)).toBe(true);
		expect(
			attentionSourceFrontierProven(capped, 201)
		).toBe(false);
		expect(attentionFrontiersBlockedByCap([capped], 201)).toBe(true);
		expect(attentionGloballyProvenRowCount([capped], 260)).toBe(200);
	});

	it('allows all locally available rows after every source is exhausted', () => {
		const exhausted = { loadedCount: 3, hasMore: false, bufferLimit: 200 };

		expect(attentionSourceFrontierProven(exhausted, 240)).toBe(true);
		expect(attentionFrontiersBlockedByCap([exhausted], 240)).toBe(false);
		expect(attentionGloballyProvenRowCount([exhausted], 17)).toBe(17);
	});

	it('reports an aggregate cap even when every child lane is exhausted', () => {
		expect(attentionAggregateBufferCapped(240, 200, 200)).toBe(true);
		expect(attentionAggregateBufferCapped(200, 200, 200)).toBe(false);
		expect(attentionAggregateBufferCapped(240, 199, 200)).toBe(false);
	});

	it('dedupes cursor pages and enforces a finite local buffer', () => {
		const merged = mergeBoundedRows(
			[{ id: 'a' }, { id: 'b' }],
			[{ id: 'b' }, { id: 'c' }, { id: 'd' }],
			(row) => row.id,
			3
		);
		expect(merged).toEqual([{ id: 'a' }, { id: 'b' }, { id: 'c' }]);
	});

	it('coalesces concurrent cursor loads and stops at the backend end', async () => {
		let hasMore = true;
		let finish!: () => void;
		const pending = new Promise<void>((resolve) => {
			finish = resolve;
		});
		const loadPage = vi.fn(async () => {
			await pending;
			hasMore = false;
		});
		const loader = createSerializedCursorLoader(() => hasMore, loadPage);

		const first = loader.loadNext();
		const raced = loader.loadNext();
		expect(loader.isLoading()).toBe(true);
		expect(loadPage).toHaveBeenCalledTimes(1);
		finish();
		expect(await first).toBe(true);
		expect(await raced).toBe(true);
		expect(loader.isLoading()).toBe(false);
		expect(await loader.loadNext()).toBe(false);
		expect(loadPage).toHaveBeenCalledTimes(1);
	});

	it('does not let an old generation clear the current cursor load', async () => {
		const generations = createGenerationGuard();
		const finishes: Array<() => void> = [];
		const loadPage = vi.fn(
			() => new Promise<void>((resolve) => finishes.push(resolve))
		);
		const loader = createSerializedCursorLoader(
			() => true,
			loadPage,
			() => generations.current()
		);

		const oldLoad = loader.loadNext();
		generations.advance();
		const currentLoad = loader.loadNext();
		expect(loadPage).toHaveBeenCalledTimes(2);
		finishes[0]();
		await oldLoad;
		expect(loader.isLoading()).toBe(true);
		finishes[1]();
		await currentLoad;
		expect(loader.isLoading()).toBe(false);
	});
});

describe('Attention center lifecycle', () => {
	it('balances start and stop across repeated open/close and destroy calls', () => {
		const start = vi.fn();
		const stop = vi.fn();
		const lifecycle = createBalancedLifecycle(start, stop);

		lifecycle.setActive(true);
		lifecycle.setActive(true);
		lifecycle.setActive(false);
		lifecycle.setActive(false);
		lifecycle.setActive(true);
		lifecycle.destroy();
		lifecycle.destroy();

		expect(start).toHaveBeenCalledTimes(2);
		expect(stop).toHaveBeenCalledTimes(2);
	});

	it('invalidates close/reopen and A to B to A work even when scope text repeats', () => {
		const generations = createGenerationGuard();
		const firstA = generations.advance();
		generations.advance(); // close or scope B
		const secondA = generations.advance();

		expect(generations.isCurrent(firstA)).toBe(false);
		expect(generations.isCurrent(secondA)).toBe(true);
	});
});

describe('Attention review history', () => {
	it('consumes an owned attention item entry before review navigation', () => {
		const url = new URL(
			'https://magician.test/square?cycle=7&attention=1&attention_item=review-1#citizen-4'
		);
		const plan = planAttentionReviewHistory(url, {
			[ATTENTION_HISTORY_STATE_KEY]: {
				version: 1,
				kind: 'item',
				itemParent: 'center'
			}
		});

		expect(plan).toEqual({ kind: 'back' });
	});

	it('replaces an unowned attention deep link with its originating page URL', () => {
		const url = new URL(
			'https://magician.test/today?tab=needs_you&attention=1&attention_item=review-1#queue'
		);
		const plan = planAttentionReviewHistory(url, { unrelated: 'kept' });

		expect(plan.kind).toBe('replace');
		if (plan.kind !== 'replace') throw new Error('expected replacement plan');
		expect(plan.url.href).toBe('https://magician.test/today?tab=needs_you#queue');
		expect(plan.state).toEqual({ unrelated: 'kept' });
	});
});
