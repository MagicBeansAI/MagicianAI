import {
	ATTENTION_HISTORY_STATE_KEY,
	readAttentionHistoryMarker,
	updateAttentionCenterUrl
} from './centerState';

export const ATTENTION_CENTER_PAGE_SIZE = 6;

export function clampAttentionPageIndex(
	pageIndex: number,
	rowCount: number,
	pageSize = ATTENTION_CENTER_PAGE_SIZE
): number {
	const safeSize = Math.max(1, Math.floor(pageSize));
	const safeRows = Math.max(0, Math.floor(rowCount));
	const maxPage = Math.max(0, Math.ceil(safeRows / safeSize) - 1);
	return Math.min(Math.max(0, Math.floor(pageIndex)), maxPage);
}

export interface AttentionSourceFrontier {
	loadedCount: number;
	hasMore: boolean;
	bufferLimit: number;
}

/** A source can prove a global top-N window only after loading N rows or exhausting its cursor. */
export function attentionSourceFrontierProven(
	frontier: AttentionSourceFrontier,
	requiredRows: number
): boolean {
	const limit = Math.max(0, Math.floor(frontier.bufferLimit));
	const loaded = Math.min(limit, Math.max(0, Math.floor(frontier.loadedCount)));
	const target = Math.max(0, Math.floor(requiredRows));
	return !frontier.hasMore || loaded >= target;
}

export function attentionFrontiersProven(
	frontiers: AttentionSourceFrontier[],
	requiredRows: number
): boolean {
	return frontiers.every((frontier) => attentionSourceFrontierProven(frontier, requiredRows));
}

/** True when a source cannot prove the requested window without exceeding its local cap. */
export function attentionFrontiersBlockedByCap(
	frontiers: AttentionSourceFrontier[],
	requiredRows: number
): boolean {
	const target = Math.max(0, Math.floor(requiredRows));
	return frontiers.some((frontier) => {
		const limit = Math.max(0, Math.floor(frontier.bufferLimit));
		const loaded = Math.min(limit, Math.max(0, Math.floor(frontier.loadedCount)));
		return frontier.hasMore && loaded >= limit && loaded < target;
	});
}

/** Number of merged rows whose ordering is proven by every source frontier. */
export function attentionGloballyProvenRowCount(
	frontiers: AttentionSourceFrontier[],
	availableRows: number
): number {
	let proven = Math.max(0, Math.floor(availableRows));
	for (const frontier of frontiers) {
		if (!frontier.hasMore) continue;
		const limit = Math.max(0, Math.floor(frontier.bufferLimit));
		const loaded = Math.min(limit, Math.max(0, Math.floor(frontier.loadedCount)));
		proven = Math.min(proven, loaded);
	}
	return proven;
}

/** True when a merged source was truncated even though its child cursors are exhausted. */
export function attentionAggregateBufferCapped(
	totalLoadedRows: number,
	bufferedRows: number,
	bufferLimit: number
): boolean {
	const total = Math.max(0, Math.floor(totalLoadedRows));
	const buffered = Math.max(0, Math.floor(bufferedRows));
	const limit = Math.max(0, Math.floor(bufferLimit));
	return total > buffered && buffered >= limit;
}

/** ServerPager props for one attention tab. */
export interface AttentionPagerView {
	/** 1-based, clamped into `[1, pageCount]`. */
	currentPage: number;
	pageCount: number;
	startItem: number;
	endItem: number;
	totalItems: number;
}

export interface AttentionPagerViewInput {
	/** 0-based page the surface is currently rendering. */
	pageIndex: number;
	pageSize: number;
	/** Rows actually rendered on the current page (drives `start`-`end`). */
	visibleRowCount: number;
	/** Rows whose global ordering every source frontier has proven. */
	provenRowCount: number;
	/** Server-reported total for the active tab; 0 when unknown. */
	categoryTotal: number;
	/** More cursor pages are still fetchable for this tab. */
	hasServerMore: boolean;
	/** 1-based hard ceiling from a local buffer cap; omit when unbounded. */
	maxReachablePage?: number;
}

/**
 * Offset-shaped `ServerPager` props derived from cursor-frontier state.
 *
 * The attention feed is cursor-paged across several merged sources, but every
 * tab has a true server-side total, so "Page 2 of 7 · 21-40 of 137" is honest
 * without an offset endpoint. Two asymmetries matter:
 *
 * - While the cursor is live the server total is the denominator (loaded rows
 *   are only a prefix). Once it is exhausted the loaded rows are the truth —
 *   totals over-count after client-side dismissals.
 * - An under-reported total must never disable Next while the server still has
 *   rows, so a live cursor always keeps at least one page ahead reachable.
 */
export function attentionPagerView(input: AttentionPagerViewInput): AttentionPagerView {
	const pageSize = Math.max(1, Math.floor(input.pageSize));
	const pageIndex = Math.max(0, Math.floor(input.pageIndex));
	const proven = Math.max(0, Math.floor(input.provenRowCount));
	const visible = Math.max(0, Math.floor(input.visibleRowCount));
	const categoryTotal = Math.max(0, Math.floor(input.categoryTotal));

	const totalItems = input.hasServerMore ? Math.max(categoryTotal, proven) : proven;
	let pageCount = Math.max(1, Math.ceil(totalItems / pageSize));
	if (input.hasServerMore) pageCount = Math.max(pageCount, pageIndex + 2);

	// A buffer cap can make far pages unreachable — never advertise them, but
	// never clamp below the page already on screen either.
	const ceiling = Math.floor(input.maxReachablePage ?? Number.MAX_SAFE_INTEGER);
	pageCount = Math.max(1, Math.min(pageCount, Math.max(ceiling, pageIndex + 1)));

	const startItem = visible === 0 ? 0 : pageIndex * pageSize + 1;
	const endItem = visible === 0 ? 0 : pageIndex * pageSize + visible;
	return {
		currentPage: Math.min(Math.max(1, pageIndex + 1), pageCount),
		pageCount,
		startItem,
		endItem,
		totalItems: Math.max(totalItems, endItem)
	};
}

/**
 * Where a First/Last/arbitrary jump settles once its cursor loads have run.
 *
 * A jump that overshoots what the frontiers can prove lands on the furthest
 * fully-backed page rather than staying put — but a forward jump never drags
 * the user backwards when the row set shrank mid-flight.
 */
export function attentionJumpLandingIndex(
	currentIndex: number,
	targetIndex: number,
	provenRowCount: number,
	pageSize: number
): number {
	const size = Math.max(1, Math.floor(pageSize));
	const current = Math.max(0, Math.floor(currentIndex));
	const target = Math.max(0, Math.floor(targetIndex));
	const proven = Math.max(0, Math.floor(provenRowCount));
	const maxIndex = Math.max(0, Math.ceil(proven / size) - 1);
	return Math.min(target, Math.max(maxIndex, current));
}

export function attentionPageRows<T>(
	rows: T[],
	pageIndex: number,
	pageSize = ATTENTION_CENTER_PAGE_SIZE
): T[] {
	const safePage = Math.max(0, Math.floor(pageIndex));
	const safeSize = Math.max(1, Math.floor(pageSize));
	const start = safePage * safeSize;
	return rows.slice(start, start + safeSize);
}

export function mergeBoundedRows<T>(
	existing: T[],
	incoming: T[],
	key: (row: T) => string,
	limit: number
): T[] {
	const seen = new Set<string>();
	const safeLimit = Math.max(0, Math.floor(limit));
	return [...existing, ...incoming]
		.filter((row) => {
			const rowKey = key(row);
			if (seen.has(rowKey)) return false;
			seen.add(rowKey);
			return true;
		})
		.slice(0, safeLimit);
}

export interface SerializedCursorLoader {
	loadNext(): Promise<boolean>;
	isLoading(): boolean;
}

/** Coalesces concurrent Next/hydration requests onto one cursor fetch. */
export function createSerializedCursorLoader(
	hasMore: () => boolean,
	loadPage: () => Promise<void>,
	generation: () => unknown = () => 0
): SerializedCursorLoader {
	let inFlight: { generation: unknown; promise: Promise<boolean> } | null = null;
	return {
		loadNext(): Promise<boolean> {
			const requestedGeneration = generation();
			const activeRequest = inFlight;
			if (activeRequest && activeRequest.generation === requestedGeneration) {
				return activeRequest.promise;
			}
			if (!hasMore()) return Promise.resolve(false);
			const request = {
				generation: requestedGeneration,
				promise: Promise.resolve(false)
			};
			request.promise = loadPage()
				.then(() => true)
				.finally(() => {
					if (inFlight === request) inFlight = null;
				});
			inFlight = request;
			return request.promise;
		},
		isLoading(): boolean {
			return inFlight !== null;
		}
	};
}

export interface GenerationGuard {
	advance(): number;
	current(): number;
	isCurrent(generation: number): boolean;
}

/** Monotonic token guard for async work that cannot be cancelled. */
export function createGenerationGuard(): GenerationGuard {
	let generation = 0;
	return {
		advance(): number {
			generation += 1;
			return generation;
		},
		current(): number {
			return generation;
		},
		isCurrent(candidate: number): boolean {
			return candidate === generation;
		}
	};
}

export type AttentionReviewHistoryPlan =
	| { kind: 'back' }
	| { kind: 'replace'; url: URL; state: Record<string, unknown> };

/** Consume an owned item entry, or sanitize an unowned deep link in place. */
export function planAttentionReviewHistory(
	url: URL,
	state: unknown
): AttentionReviewHistoryPlan {
	if (readAttentionHistoryMarker(state)?.kind === 'item') return { kind: 'back' };
	const nextState =
		state && typeof state === 'object' ? { ...(state as Record<string, unknown>) } : {};
	delete nextState[ATTENTION_HISTORY_STATE_KEY];
	return {
		kind: 'replace',
		url: updateAttentionCenterUrl(url, { open: false }),
		state: nextState
	};
}

export interface BalancedLifecycle {
	setActive(active: boolean): void;
	destroy(): void;
	isActive(): boolean;
}

/** Idempotent lifecycle guard used to balance attentionStore start/stop calls. */
export function createBalancedLifecycle(start: () => void, stop: () => void): BalancedLifecycle {
	let active = false;
	return {
		setActive(next: boolean): void {
			if (next === active) return;
			active = next;
			if (active) start();
			else stop();
		},
		destroy(): void {
			if (!active) return;
			active = false;
			stop();
		},
		isActive(): boolean {
			return active;
		}
	};
}
