/**
 * Monitors list pagination — CURSOR movement under the shared `ServerPager`.
 *
 * The envelope of `GET /monitors` is `{items, next_cursor, limit, total,
 * offset}`. The cursor is still the only way to MOVE (there is no `offset`
 * request parameter, and the cursor is what keeps paging stable while monitors
 * fire on schedules and agents create tasks under a reader sitting on page 2).
 * What `total` adds is the two things a cursor cannot say: how many pages
 * there are, and therefore which one is last. So:
 *
 * - **next/prev/first/last all move by cursor.** Forward is `next_cursor`;
 *   backward re-uses the cursor that produced that page, kept in a stack;
 *   "last" walks the chain forward until the server stops handing out cursors.
 * - **`total` is read for the page count and skip-to-last, and nothing else.**
 *   Reachability is decided by the cursor, never by arithmetic over `total`.
 *
 * **When `total` is absent** — an older server, or a response predating the
 * envelope change — the surface keeps its previous behaviour exactly:
 * accumulate-and-Load-more, no page count, no skip-to-last. Absence means the
 * server never applied the feature, NOT that there are zero pages, so it must
 * never reach `ServerPager` as `pageCount: 0`; `monitorPagerView` returns
 * `null` rather than a number for precisely that reason.
 *
 * Built on the attention center's cursor idioms (`$lib/attention/pagination`):
 * a serialized cursor loader (concurrent forward calls coalesce onto one
 * in-flight fetch) and a generation guard (a filter switch, a page-size change
 * or a refresh invalidates late responses from the previous generation instead
 * of interleaving rows).
 *
 * Framework-free on purpose: the workspace component subscribes via
 * `onChange` snapshots; vitest drives it with an injected `fetchPage`.
 */

import {
	createGenerationGuard,
	createSerializedCursorLoader
} from '$lib/attention/pagination';
import { pageAfterRemoval } from '$lib/shared/components/pageAfterRemoval';
import type { MonitorListItemV1, MonitorListPageV1 } from '$lib/types/monitor';
import type { MonitorStateFilter } from './api';

/**
 * The same page-size vocabulary the two other tabs on this route offer, so
 * switching tabs does not switch vocabularies. First entry is the default.
 */
export const MONITORS_PAGE_SIZES = [25, 50, 100, 250];
export const MONITORS_PAGE_SIZE = MONITORS_PAGE_SIZES[0];

export interface MonitorPagerSnapshot {
	items: MonitorListItemV1[];
	/** Opaque server cursor for the next page; null on the last page. */
	nextCursor: string | null;
	hasMore: boolean;
	/** True during the FIRST page of a generation (full-surface skeleton). */
	initialLoading: boolean;
	/** True while any page fetch is in flight (Load-more spinner). */
	loading: boolean;
	error: string | null;
	stateFilter: MonitorStateFilter | null;
	/** Rows requested per page; one of `MONITORS_PAGE_SIZES`. */
	pageSize: number;
	/** 1-based page the reader is on. Always 1 while `total` is absent. */
	page: number;
	/**
	 * The envelope's `total`, or **null when the server did not send one**.
	 * Null is "no page count, cursor paging only" — never zero.
	 */
	total: number | null;
	/**
	 * Where the cursor resolved to, as the server reported it; null when the
	 * server did not send one (then the page index carries the position).
	 */
	offset: number | null;
}

export type MonitorPageFetcher = (
	limit: number,
	cursor: string | null,
	state: MonitorStateFilter | null
) => Promise<MonitorListPageV1>;

export interface MonitorPager {
	snapshot(): MonitorPagerSnapshot;
	/** Clear + load page one for `filter` (also the refresh path). */
	reset(filter: MonitorStateFilter | null): Promise<void>;
	/**
	 * Advance one cursor page. With `total` present that REPLACES the rows and
	 * moves to the next page; without it the rows accumulate and the page stays
	 * at one. Resolves false when the cursor is exhausted.
	 */
	loadNext(): Promise<boolean>;
	/**
	 * Move to a 1-based page. Backward jumps re-use the stored cursor for that
	 * page; forward jumps walk the cursor chain and settle on the furthest page
	 * the server actually hands out. No-op without a server `total` — there are
	 * no page numbers to move between.
	 */
	goToPage(page: number): Promise<void>;
	/** Change the page size and reload from page one. */
	setPageSize(size: number): Promise<void>;
	/**
	 * Re-read the page the reader is on, leaving their position alone.
	 *
	 * For a mutation that CHANGES a row rather than removing one — a pause, a
	 * resume, a saved edit. `reset` was doing this job and it lands on page one,
	 * so pausing a monitor from page three used to hand the reader page one back.
	 */
	reloadCurrentPage(): Promise<void>;
	/**
	 * Reconcile the list after `taskId` was removed from it: drop the row and
	 * its share of the total right now, then apply the shared removal policy
	 * (`$lib/shared/components/pageAfterRemoval`) — re-read the page the reader
	 * is on, and step back one page if it comes back empty.
	 *
	 * There is no inline row action on this list yet; delete and pause both live
	 * in the detail panel. It is wired anyway, because since this pager started
	 * REPLACING rows rather than accumulating them, the first row action added
	 * here would make it drain exactly the way `/attention` did.
	 */
	removeRow(taskId: string): Promise<void>;
}

/** The five numbers `ServerPager` renders. */
export interface MonitorPagerView {
	currentPage: number;
	pageCount: number;
	startItem: number;
	endItem: number;
	totalItems: number;
}

/** Append a page, deduping by task_id (a row that moved between pages —
 *  e.g. its updated_at changed mid-pagination — must not render twice). */
export function mergeMonitorRows(
	existing: MonitorListItemV1[],
	incoming: MonitorListItemV1[]
): MonitorListItemV1[] {
	const seen = new Set(existing.map((row) => row.task_id));
	const appended = incoming.filter((row) => {
		if (seen.has(row.task_id)) return false;
		seen.add(row.task_id);
		return true;
	});
	return appended.length === 0 ? existing : [...existing, ...appended];
}

/** A non-negative integer, or null for anything the server did not send. */
function readCount(raw: unknown): number | null {
	if (typeof raw !== 'number' || !Number.isFinite(raw) || raw < 0) return null;
	return Math.floor(raw);
}

/**
 * `ServerPager` props for a monitors snapshot, or **null when the server sent
 * no `total`** — the caller must then render the cursor-only control, because
 * there is no honest page count to show. Returning null (rather than a
 * pageCount of 0, or of 1) is what keeps "the server never applied the
 * feature" from being rendered as "there is nothing here".
 *
 * Two asymmetries, both resolved in the cursor's favour:
 *
 * - A live cursor always keeps one more page reachable, even if `total`
 *   under-reports. Next must never be disabled while the server still has rows.
 * - An exhausted cursor ends the count where the reader stands, even if
 *   `total` claims more. Nothing further is reachable, so nothing further is
 *   advertised.
 */
export function monitorPagerView(snapshot: MonitorPagerSnapshot): MonitorPagerView | null {
	if (snapshot.total === null) return null;

	const pageSize = Math.max(1, Math.floor(snapshot.pageSize));
	const pageIndex = Math.max(0, Math.floor(snapshot.page) - 1);
	const base = snapshot.offset === null ? pageIndex * pageSize : snapshot.offset;
	const startItem = snapshot.items.length === 0 ? 0 : base + 1;
	const endItem = snapshot.items.length === 0 ? 0 : base + snapshot.items.length;

	const pageCount = snapshot.hasMore
		? Math.max(Math.ceil(snapshot.total / pageSize), pageIndex + 2)
		: Math.max(1, pageIndex + 1);

	return {
		currentPage: Math.min(pageIndex + 1, pageCount),
		pageCount,
		startItem,
		endItem,
		totalItems: Math.max(snapshot.total, endItem)
	};
}

export function createMonitorPager(
	fetchPage: MonitorPageFetcher,
	onChange: (snapshot: MonitorPagerSnapshot) => void,
	initialPageSize = MONITORS_PAGE_SIZE
): MonitorPager {
	let items: MonitorListItemV1[] = [];
	let nextCursor: string | null = null;
	let exhausted = false;
	// Starts true so the surface renders skeletons (never an empty-state
	// flash) between mount and the first `reset` (§9.2 rule 6).
	let initialLoading = true;
	let loading = false;
	let error: string | null = null;
	let stateFilter: MonitorStateFilter | null = null;
	let pageSize = Math.max(1, Math.floor(initialPageSize));
	let pageIndex = 0;
	let total: number | null = null;
	let offset: number | null = null;
	/**
	 * `cursors[i]` is the cursor that fetches 0-based page `i`; `cursors[0]` is
	 * null (page one asks for no cursor). Recorded as the reader moves forward,
	 * which is what makes Prev and First single fetches rather than a re-walk.
	 */
	let cursors: Array<string | null> = [null];

	const generation = createGenerationGuard();

	function snapshot(): MonitorPagerSnapshot {
		return {
			items,
			nextCursor,
			hasMore: !exhausted,
			initialLoading,
			loading,
			error,
			stateFilter,
			pageSize,
			page: pageIndex + 1,
			total,
			offset
		};
	}

	function emit(): void {
		onChange(snapshot());
	}

	/**
	 * Fetch one page and land on `targetIndex`.
	 *
	 * `targetIndex === 0` restarts: rows, cursor stack and `total` all come
	 * from this response. Beyond that the mode set by page one decides what
	 * happens to the rows — replaced when the server sends a `total` (the
	 * reader is on ONE page), appended when it does not (the old
	 * accumulate-and-Load-more surface, unchanged).
	 */
	async function load(
		cursor: string | null,
		targetIndex: number,
		requestGeneration: number
	): Promise<void> {
		loading = true;
		error = null;
		emit();
		try {
			const page = await fetchPage(pageSize, cursor, stateFilter);
			if (!generation.isCurrent(requestGeneration)) return;
			const reportedTotal = readCount(page.total);
			if (targetIndex === 0) {
				items = page.items;
				cursors = [null];
				pageIndex = 0;
				total = reportedTotal;
			} else if (total === null) {
				// Cursor-only server: accumulate, stay on page one.
				items = mergeMonitorRows(items, page.items);
			} else {
				items = page.items;
				pageIndex = targetIndex;
				cursors[targetIndex] = cursor;
				// A fresher total from a later page is still the truth.
				if (reportedTotal !== null) total = reportedTotal;
			}
			offset = readCount(page.offset);
			nextCursor = page.next_cursor;
			exhausted = page.next_cursor === null;
			cursors[pageIndex + 1] = page.next_cursor;
		} catch (err) {
			if (!generation.isCurrent(requestGeneration)) return;
			error = err instanceof Error ? err.message : String(err);
		} finally {
			if (generation.isCurrent(requestGeneration)) {
				loading = false;
				initialLoading = false;
				emit();
			}
		}
	}

	const loader = createSerializedCursorLoader(
		() => !exhausted,
		() => load(nextCursor, pageIndex + 1, generation.current()),
		() => generation.current()
	);

	/**
	 * Restart at page one. `clearRows` is the difference between a reload the
	 * reader asked to WAIT for (a filter switch, a refresh, a page-size change
	 * — full-surface skeleton) and a page move (rows stay on screen until the
	 * new page lands, the way the shared pager behaves on the task tabs).
	 */
	async function loadFirstPage(clearRows: boolean): Promise<void> {
		const requestGeneration = generation.advance();
		if (clearRows) {
			// Only the waiting path tears down state up front. A page move that
			// fails must leave the reader where they were, and `load` rewrites
			// all of this on success anyway.
			items = [];
			nextCursor = null;
			exhausted = false;
			offset = null;
			cursors = [null];
			pageIndex = 0;
			initialLoading = true;
		}
		await load(null, 0, requestGeneration);
	}

	async function loadNext(): Promise<boolean> {
		if (exhausted || initialLoading) return false;
		return loader.loadNext();
	}

	/**
	 * Re-read one 0-based page and resolve to the rows it came back with.
	 *
	 * A page the reader has never walked through has no stored cursor, and the
	 * only honest re-read is then page one — the same landing `goToPage` gives a
	 * forward jump it cannot back with a cursor.
	 *
	 * A FAILED read reports a non-zero count, because the removal policy reads
	 * zero as "this page ended" and a network error is not evidence of that.
	 */
	async function reloadPage(index: number): Promise<number> {
		const target = Math.max(0, Math.floor(index));
		// `null` is a legal stored cursor (page one), so a page counts as seen by
		// `undefined`, not by falsiness.
		if (target === 0 || cursors[target] === undefined) {
			await loadFirstPage(false);
		} else {
			await load(cursors[target], target, generation.advance());
		}
		return error === null ? items.length : 1;
	}

	return {
		snapshot,
		async reset(filter: MonitorStateFilter | null): Promise<void> {
			stateFilter = filter;
			await loadFirstPage(true);
		},
		loadNext,
		async goToPage(page: number): Promise<void> {
			if (total === null || initialLoading || !Number.isFinite(page)) return;
			const target = Math.max(0, Math.floor(page) - 1);
			if (target === pageIndex) return;
			if (target === 0) {
				await loadFirstPage(false);
				return;
			}
			// `null` is a legal stored cursor (page one), so a page counts as
			// seen by `undefined`, not by falsiness.
			const known = cursors[target];
			if (known !== undefined) {
				await load(known, target, generation.advance());
				return;
			}
			// Forward into pages the reader has not passed through: walk the
			// chain, and settle on the last page the server actually offers if
			// the target is past the end.
			generation.advance();
			while (pageIndex < target && !exhausted) {
				if (!(await loadNext())) break;
				if (error !== null) break;
			}
		},
		async setPageSize(size: number): Promise<void> {
			if (!Number.isFinite(size) || size <= 0) return;
			const next = Math.floor(size);
			if (next === pageSize) return;
			pageSize = next;
			// Page 3 of 25 is not page 3 of 100 — the only honest landing is one.
			await loadFirstPage(true);
		},
		async reloadCurrentPage(): Promise<void> {
			if (initialLoading) return;
			await reloadPage(pageIndex);
		},
		async removeRow(taskId: string): Promise<void> {
			const id = taskId.trim();
			if (id) {
				// The policy's first sentence: the row and its share of the total
				// go now, so the pager stops counting something the reader has
				// already been told is gone.
				const remaining = items.filter((row) => row.task_id !== id);
				if (remaining.length !== items.length) {
					items = remaining;
					if (total !== null) total = Math.max(0, total - 1);
					emit();
				}
			}
			await pageAfterRemoval(pageIndex + 1, (page) => reloadPage(page - 1));
		}
	};
}
