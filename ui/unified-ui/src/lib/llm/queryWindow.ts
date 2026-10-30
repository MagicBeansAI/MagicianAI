/**
 * Literal epoch-ms time bounds and batch lanes for `llm_calls` analytics SQL.
 *
 * The analytics server narrows the `llm_calls` materialization (and its
 * partition lookback) from each query's lower bound
 * (`llm_calls_timestamp_lower_bound_ms` in `legacy_llm_compat.rs`). It reads a
 * relative bound such as `epoch_ms(now() - INTERVAL 7 DAYS)` only with two
 * days of time-zone slack, so rolling windows are computed in the browser and
 * inlined as exact numbers: `timestamp_ms > 1730000000000`.
 *
 * Anchors are rounded UP to the minute. Live widgets re-wire when their SQL
 * text changes (`liveDataKey` keys on kind + lane + sql), so rebuilding a query
 * in the same minute yields the identical string and reuses the wired
 * controller instead of refetching. Rounding up rather than down keeps the
 * window at most N days, so the server's partition lookback
 * (`ceil((now - bound) / day)`) stays N days instead of N + 1; the window has
 * no upper bound, so nothing newer than the anchor is lost.
 */

/** Granularity of query anchors; one string per minute, not per millisecond. */
export const QUERY_ANCHOR_STEP_MS = 60_000;

export const DAY_MS = 86_400_000;

/**
 * Batch lane for windows of one day or less (today, today-vs-yesterday, the
 * 24h widgets). Kept apart from the 7d/30d lane so a small window renders in
 * seconds instead of waiting behind the wide batch.
 */
export const LLM_SHORT_WINDOW_LANE = 'short-window';

/** `nowMs` rounded up to the minute. */
export function queryAnchorMs(nowMs: number = Date.now()): number {
	return Math.ceil(nowMs / QUERY_ANCHOR_STEP_MS) * QUERY_ANCHOR_STEP_MS;
}

/** Epoch-ms start of the rolling `days` window ending at `anchorMs`. */
export function rollingWindowStartMs(days: number, anchorMs: number): number {
	return queryAnchorMs(anchorMs) - Math.floor(days * DAY_MS);
}

/**
 * Literal replacement for
 * `timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL {days} DAYS)`.
 */
export function rollingWindowPredicate(days: number, anchorMs: number): string {
	return `timestamp_ms > ${rollingWindowStartMs(days, anchorMs)}`;
}

/**
 * The batch lane for a rolling window of `days`: the short-window lane for
 * windows of at most one day, the default lane otherwise (and for lifetime,
 * `days <= 0`).
 */
export function laneForWindowDays(days: number): string | undefined {
	return days > 0 && days <= 1 ? LLM_SHORT_WINDOW_LANE : undefined;
}
