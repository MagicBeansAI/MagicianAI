// Cursor pagination store for /tasks?type=monitors (Phase 4).
// Pure vitest — the fetcher is injected, no network, no Svelte.
import { describe, expect, it } from 'vitest';
import type { MonitorListItemV1, MonitorListPageV1 } from '../types/monitor';
import type { MonitorStateFilter } from './api';
import {
	createMonitorPager,
	mergeMonitorRows,
	monitorPagerView,
	type MonitorPagerSnapshot
} from './pagination';

function row(taskId: string, state: 'active' | 'paused' = 'active'): MonitorListItemV1 {
	return {
		task_id: taskId,
		title: `Monitor ${taskId}`,
		objective: `Watch ${taskId}`,
		state,
		cadence_summary: 'Cron 0 9 * * 1 (UTC)',
		monitor_revision: 1,
		last_run_status: 'never_ran',
		health: 'ok'
	};
}

/**
 * An envelope from a server that has NOT applied the counting keys — no
 * `total`, no `offset`. Every `page(...)` below is therefore also a regression
 * test for the cursor-only fallback.
 */
function page(
	items: MonitorListItemV1[],
	nextCursor: string | null,
	limit = 2
): MonitorListPageV1 {
	return { items, next_cursor: nextCursor, limit };
}

/** The counting envelope: `total` is the filtered corpus, `offset` is where the cursor landed. */
function countedPage(
	items: MonitorListItemV1[],
	nextCursor: string | null,
	offset: number,
	total: number,
	limit = 2
): MonitorListPageV1 {
	return { items, next_cursor: nextCursor, limit, total, offset };
}

interface Call {
	limit: number;
	cursor: string | null;
	state: MonitorStateFilter | null;
}

/** Scripted fetcher: consumes responses in order and records the calls. */
function scriptedFetcher(responses: Array<MonitorListPageV1 | Error>) {
	const calls: Call[] = [];
	const pending: Array<() => void> = [];
	let paused = false;
	const fetchPage = async (
		limit: number,
		cursor: string | null,
		state: MonitorStateFilter | null
	): Promise<MonitorListPageV1> => {
		calls.push({ limit, cursor, state });
		if (paused) {
			await new Promise<void>((resolve) => pending.push(resolve));
		}
		const next = responses.shift();
		if (!next) throw new Error('scripted fetcher exhausted');
		if (next instanceof Error) throw next;
		return next;
	};
	return {
		calls,
		fetchPage,
		pause() {
			paused = true;
		},
		flush() {
			paused = false;
			while (pending.length > 0) pending.shift()?.();
		}
	};
}

describe('mergeMonitorRows', () => {
	it('appends new rows and drops duplicate task ids across page boundaries', () => {
		const merged = mergeMonitorRows([row('a'), row('b')], [row('b'), row('c')]);
		expect(merged.map((r) => r.task_id)).toEqual(['a', 'b', 'c']);
	});

	it('returns the existing array untouched when nothing new arrives', () => {
		const existing = [row('a')];
		expect(mergeMonitorRows(existing, [row('a')])).toBe(existing);
	});
});

describe('createMonitorPager', () => {
	it('loads page one, then follows the server cursor and accumulates rows', async () => {
		const fetcher = scriptedFetcher([
			page([row('a'), row('b')], 'cur_b'),
			page([row('c')], null)
		]);
		const snapshots: MonitorPagerSnapshot[] = [];
		const pager = createMonitorPager(fetcher.fetchPage, (s) => snapshots.push(s), 2);

		// Pristine snapshot renders skeletons, never an empty-state flash
		// (§9.2 rule 6): initialLoading is true before the first reset.
		expect(pager.snapshot().initialLoading).toBe(true);
		expect(await pager.loadNext()).toBe(false);

		await pager.reset(null);
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['a', 'b']);
		expect(pager.snapshot().hasMore).toBe(true);
		expect(fetcher.calls[0]).toEqual({ limit: 2, cursor: null, state: null });

		const loaded = await pager.loadNext();
		expect(loaded).toBe(true);
		expect(fetcher.calls[1].cursor).toBe('cur_b');
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['a', 'b', 'c']);
		expect(pager.snapshot().hasMore).toBe(false);

		// Exhausted cursor: no further fetches.
		expect(await pager.loadNext()).toBe(false);
		expect(fetcher.calls).toHaveLength(2);

		// Loading flags surfaced for skeleton/spinner states.
		expect(snapshots[0].initialLoading).toBe(true);
		expect(snapshots.at(-1)?.loading).toBe(false);
	});

	it('passes the state filter through and resets rows on filter change', async () => {
		const fetcher = scriptedFetcher([
			page([row('a')], null),
			page([row('p', 'paused')], null)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.reset('paused');
		expect(fetcher.calls[1].state).toBe('paused');
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['p']);
		expect(pager.snapshot().stateFilter).toBe('paused');
	});

	it('coalesces concurrent load-next calls onto one cursor fetch', async () => {
		const fetcher = scriptedFetcher([
			page([row('a')], 'cur_a'),
			page([row('b')], null)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 1);
		await pager.reset(null);

		fetcher.pause();
		const first = pager.loadNext();
		const second = pager.loadNext();
		fetcher.flush();
		await Promise.all([first, second]);

		// One page-one fetch + ONE coalesced next-page fetch.
		expect(fetcher.calls).toHaveLength(2);
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['a', 'b']);
	});

	it('drops late responses from a superseded generation (filter switched mid-flight)', async () => {
		const fetcher = scriptedFetcher([
			page([row('stale')], 'cur_stale'),
			page([row('fresh', 'paused')], null)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 1);

		fetcher.pause();
		const staleLoad = pager.reset(null);
		const freshLoad = pager.reset('paused');
		fetcher.flush();
		await Promise.all([staleLoad, freshLoad]);

		// Only the current generation's rows and cursor survive. The scripted
		// fetcher answers in call order, so the stale (first) response carried
		// row "stale" — it must not appear.
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['fresh']);
		expect(pager.snapshot().stateFilter).toBe('paused');
		expect(pager.snapshot().hasMore).toBe(false);
	});

	it('surfaces fetch errors and recovers on retry (reset again)', async () => {
		const fetcher = scriptedFetcher([
			new Error('backend unavailable'),
			page([row('a')], null)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		expect(pager.snapshot().error).toBe('backend unavailable');
		expect(pager.snapshot().items).toEqual([]);

		await pager.reset(null);
		expect(pager.snapshot().error).toBeNull();
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['a']);
	});
});

describe('monitorPagerView (an absent total is not zero pages)', () => {
	it('returns null when the server sent no total', async () => {
		const fetcher = scriptedFetcher([page([row('a')], 'cur_a')]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);
		await pager.reset(null);

		expect(pager.snapshot().total).toBeNull();
		// Null, not a pageCount — the caller renders the cursor-only control.
		expect(monitorPagerView(pager.snapshot())).toBeNull();
		// And the cursor still says there is more, which is the whole point.
		expect(pager.snapshot().hasMore).toBe(true);
	});

	it('renders one page — never zero — when the server really does report an empty corpus', async () => {
		const fetcher = scriptedFetcher([countedPage([], null, 0, 0)]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);
		await pager.reset(null);

		expect(pager.snapshot().total).toBe(0);
		expect(monitorPagerView(pager.snapshot())).toEqual({
			currentPage: 1,
			pageCount: 1,
			startItem: 0,
			endItem: 0,
			totalItems: 0
		});
	});

	it('keeps a page ahead reachable while the cursor is live, even if total under-reports', async () => {
		const fetcher = scriptedFetcher([countedPage([row('a'), row('b')], 'cur_b', 0, 2)]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);
		await pager.reset(null);

		// total/pageSize says one page; the cursor says otherwise and wins.
		expect(monitorPagerView(pager.snapshot())?.pageCount).toBe(2);
	});

	it('ends the count where the reader stands once the cursor is exhausted', async () => {
		const fetcher = scriptedFetcher([countedPage([row('a')], null, 0, 99)]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);
		await pager.reset(null);

		// A total claiming 50 pages cannot advertise pages the cursor won't hand out.
		expect(monitorPagerView(pager.snapshot())?.pageCount).toBe(1);
	});
});

describe('createMonitorPager — paged mode (total present)', () => {
	it('replaces rows page by page and counts pages from total', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		expect(monitorPagerView(pager.snapshot())).toEqual({
			currentPage: 1,
			pageCount: 3,
			startItem: 1,
			endItem: 2,
			totalItems: 5
		});

		await pager.goToPage(2);
		// Moved by CURSOR, not by an offset parameter.
		expect(fetcher.calls[1]).toEqual({ limit: 2, cursor: 'cur_b', state: null });
		// One page at a time — the rows are replaced, not accumulated.
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['c', 'd']);
		expect(pager.snapshot().page).toBe(2);
		expect(monitorPagerView(pager.snapshot())).toEqual({
			currentPage: 2,
			pageCount: 3,
			startItem: 3,
			endItem: 4,
			totalItems: 5
		});
	});

	it('walks the cursor chain to skip to the last page', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('e')], null, 4, 5)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(3);

		expect(fetcher.calls.map((call) => call.cursor)).toEqual([null, 'cur_b', 'cur_d']);
		expect(pager.snapshot().page).toBe(3);
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['e']);
		expect(monitorPagerView(pager.snapshot())).toEqual({
			currentPage: 3,
			pageCount: 3,
			startItem: 5,
			endItem: 5,
			totalItems: 5
		});
	});

	it('settles on the last page the server offers when the target overshoots', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c')], null, 2, 5)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(9);

		expect(fetcher.calls).toHaveLength(2);
		expect(pager.snapshot().page).toBe(2);
		expect(pager.snapshot().hasMore).toBe(false);
	});

	it('goes back through the cursor that produced the page, in one fetch', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('e')], null, 4, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(3);
		await pager.goToPage(2);

		expect(fetcher.calls).toHaveLength(4);
		expect(fetcher.calls[3].cursor).toBe('cur_b');
		expect(pager.snapshot().page).toBe(2);
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['c', 'd']);
	});

	it('returns to page one with no cursor at all', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('a'), row('b')], 'cur_b', 0, 5)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(2);
		await pager.goToPage(1);

		expect(fetcher.calls[2].cursor).toBeNull();
		expect(pager.snapshot().page).toBe(1);
		// A page move is not a refresh: no full-surface skeleton.
		expect(pager.snapshot().initialLoading).toBe(false);
	});

	it('reloads from page one on a page-size change', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('a'), row('b'), row('c'), row('d')], 'cur_d', 0, 5, 4)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(2);
		await pager.setPageSize(4);

		// Page 3 of 25 is not page 3 of 100 — the only honest landing is one.
		expect(fetcher.calls[2]).toEqual({ limit: 4, cursor: null, state: null });
		expect(pager.snapshot().page).toBe(1);
		expect(pager.snapshot().pageSize).toBe(4);
		expect(monitorPagerView(pager.snapshot())?.pageCount).toBe(2);
	});

	it('ignores page moves when the server sent no total', async () => {
		const fetcher = scriptedFetcher([page([row('a')], 'cur_a')]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(2);

		// There are no page numbers to move between; Load-more is the only path.
		expect(fetcher.calls).toHaveLength(1);
		expect(pager.snapshot().page).toBe(1);
	});
});

/**
 * The cursor pager's half of the shared removal policy
 * (`$lib/shared/components/pageAfterRemoval`). Nothing removes a row from this
 * list inline yet — delete and pause both live in the detail panel — but the
 * pager REPLACES rows now rather than accumulating them, so the first inline
 * action added here would drain a page exactly the way `/attention` did.
 */
describe('createMonitorPager — a row leaves the list', () => {
	it('re-reads the page the reader is on and stays there when it still has rows', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('d'), row('e')], null, 2, 4)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(2);
		await pager.removeRow('c');

		// Back through the cursor that produced page 2, not from page one.
		expect(fetcher.calls[2]).toEqual({ limit: 2, cursor: 'cur_b', state: null });
		expect(pager.snapshot().page).toBe(2);
		// The row pulled forward from page 3 is here: dropping `c` locally would
		// have left a one-row page with a gap where `e` belongs.
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['d', 'e']);
	});

	it('steps back one page when the removal emptied the last one', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('e')], null, 4, 5),
			// Page 3 re-read: `e` is gone and the corpus ends at four.
			countedPage([], null, 4, 4),
			// The step back — four rows now, so page 2 is genuinely the last.
			countedPage([row('c'), row('d')], null, 2, 4)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(3);
		await pager.removeRow('e');

		expect(fetcher.calls.map((call) => call.cursor)).toEqual([
			null,
			'cur_b',
			'cur_d',
			'cur_d',
			'cur_b'
		]);
		expect(pager.snapshot().page).toBe(2);
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['c', 'd']);
		expect(monitorPagerView(pager.snapshot())).toEqual({
			currentPage: 2,
			pageCount: 2,
			startItem: 3,
			endItem: 4,
			totalItems: 4
		});
	});

	it('decrements the total the moment the row goes, before the re-read lands', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('b'), row('c')], 'cur_c', 0, 4)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		fetcher.pause();
		const settled = pager.removeRow('a');

		// The policy's first sentence, and the whole reason it is local: the
		// pager must stop counting a row the reader has been told is gone
		// without waiting a round trip to say so.
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['b']);
		expect(pager.snapshot().total).toBe(4);

		fetcher.flush();
		await settled;
		expect(pager.snapshot().page).toBe(1);
		expect(pager.snapshot().items.map((r) => r.task_id)).toEqual(['b', 'c']);
		expect(pager.snapshot().total).toBe(4);
	});

	it('does not move the reader when the re-read failed rather than came back empty', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			new Error('monitors unreachable')
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(2);
		await pager.removeRow('c');

		// Three fetches, not four: a failure is not an empty page, so there is
		// no step back to make.
		expect(fetcher.calls).toHaveLength(3);
		expect(pager.snapshot().page).toBe(2);
		expect(pager.snapshot().error).toBe('monitors unreachable');
	});
});

describe('createMonitorPager — a row changes without leaving', () => {
	it('re-reads the page the reader is on rather than starting over', async () => {
		const fetcher = scriptedFetcher([
			countedPage([row('a'), row('b')], 'cur_b', 0, 5),
			countedPage([row('c'), row('d')], 'cur_d', 2, 5),
			countedPage([row('c', 'paused'), row('d')], 'cur_d', 2, 5)
		]);
		const pager = createMonitorPager(fetcher.fetchPage, () => {}, 2);

		await pager.reset(null);
		await pager.goToPage(2);
		await pager.reloadCurrentPage();

		expect(fetcher.calls[2]).toEqual({ limit: 2, cursor: 'cur_b', state: null });
		expect(pager.snapshot().page).toBe(2);
		expect(pager.snapshot().items.map((r) => r.state)).toEqual(['paused', 'active']);
		// A re-read is not a refresh: no full-surface skeleton under the reader.
		expect(pager.snapshot().initialLoading).toBe(false);
	});
});
