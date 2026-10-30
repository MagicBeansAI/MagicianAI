/**
 * Pure SQL builders + parsers for the /llm account-wide spend breakdown
 * (Today / Last 7 days / Last 30 days, each by operation and by model).
 *
 * The six queries run through the analytics `llm_calls` query endpoint, which
 * materializes a DE-DUPLICATED `llm_calls` view (one row per call) — so
 * `SUM(cost_usd)` here never double-counts. Never query `read_parquet` directly.
 *
 * Day-boundary contract mirrors `pulseQueries`: "today" is the LOCAL calendar
 * day, inlined as epoch-ms numeric literals (SQL does no timezone math). The
 * rolling 7d/30d windows are literal epoch-ms lower bounds too, anchored on the
 * injected `anchorMs` (`$lib/llm/queryWindow`) — the server prunes only from a
 * literal bound. This module has no `Date.now()`; callers inject the clock via
 * `localDayBoundaries` and the anchor.
 *
 * Chunk-summary parity: `COALESCE(provider_attempt_count, 1) <> 0` excludes
 * `logical_chunk_summary` bookkeeping aggregates (their real chunk calls are
 * counted separately); harness estimates remain included, matching the Today's Pulse band so the Today total here
 * equals the pulse band's `spendToday`.
 */

import type { LocalDayBoundaries } from '$lib/today/pulseQueries';
import { LLM_SHORT_WINDOW_LANE, rollingWindowPredicate } from './queryWindow';

export type SpendWindow = 'today' | '7d' | '30d';
export type SpendDimension = 'operation' | 'model';

export interface SpendQuerySpec {
	window: SpendWindow;
	dimension: SpendDimension;
	sql: string;
	/** Batch lane: `today` runs in the short-window lane, 7d/30d in the default one. */
	lane?: string;
}

export interface SpendRow {
	label: string;
	spend: number;
	calls: number;
}

/** Excludes chunk-summary bookkeeping rows (see module doc). */
const REAL_CALL = "(COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')";

function windowPredicate(window: SpendWindow, b: LocalDayBoundaries, anchorMs: number): string {
	if (window === 'today') {
		return `timestamp_ms >= ${b.todayStartMs} AND timestamp_ms < ${b.tomorrowStartMs}`;
	}
	return rollingWindowPredicate(window === '7d' ? 7 : 30, anchorMs);
}

/**
 * Bucket expression for a dimension.
 *
 * These deliberately do NOT filter `IS NOT NULL`. The window total is summed
 * client-side from these rows, so anything the GROUP BY drops silently
 * disappears from the headline `$` figure — a paid call with no recorded
 * operation used to make the Spend total read LOWER than the same window in
 * "Today vs yesterday" with no way to see why. Bucketing the unattributed rows
 * under an explicit label keeps the total complete and makes the gap visible
 * instead of invisible.
 *
 * `model` also collapses empty strings, matching `model_key` in
 * `buildTodayVsYesterdaySql` so the two sections label a missing model the
 * same way.
 */
function dimensionExpr(dimension: SpendDimension): string {
	return dimension === 'model'
		? `COALESCE(NULLIF(model, ''), 'unknown')`
		: `COALESCE(NULLIF(operation, ''), 'unattributed')`;
}

function breakdownSql(
	window: SpendWindow,
	dimension: SpendDimension,
	b: LocalDayBoundaries,
	anchorMs: number
): string {
	const bucket = dimensionExpr(dimension);
	return (
		`SELECT ${bucket} AS label, SUM(cost_usd) AS spend, COUNT(*) AS calls ` +
		`FROM llm_calls ` +
		`WHERE ${windowPredicate(window, b, anchorMs)} AND ${REAL_CALL} ` +
		`GROUP BY ${bucket} HAVING SUM(cost_usd) > 0 ORDER BY spend DESC`
	);
}

/**
 * The six spend-breakdown queries: {today, 7d, 30d} x {operation, model}. The
 * caller wires each through `setupLiveDataSource({ kind: 'llm_calls_sql', lane })`;
 * same-kind, same-lane sources auto-batch into one `/query_batch` request, so
 * the two `today` queries ride the short-window batch and the four 7d/30d
 * queries the default one. `anchorMs` ends the rolling 7d/30d windows.
 */
export function buildSpendBreakdownQueries(
	b: LocalDayBoundaries,
	anchorMs: number
): SpendQuerySpec[] {
	const windows: SpendWindow[] = ['today', '7d', '30d'];
	const dimensions: SpendDimension[] = ['operation', 'model'];
	const specs: SpendQuerySpec[] = [];
	for (const window of windows) {
		for (const dimension of dimensions) {
			specs.push({
				window,
				dimension,
				sql: breakdownSql(window, dimension, b, anchorMs),
				lane: window === 'today' ? LLM_SHORT_WINDOW_LANE : undefined
			});
		}
	}
	return specs;
}

/** Numeric coercion resilient to JSON numbers, stringified BIGINTs, and NULLs. */
function toNumber(value: unknown): number {
	if (typeof value === 'number') return Number.isFinite(value) ? value : 0;
	if (typeof value === 'string') {
		const n = Number(value);
		return Number.isFinite(n) ? n : 0;
	}
	return 0;
}

/**
 * Parse endpoint records (`{ label, spend, calls }`) into typed rows, dropping
 * blank labels and sorting by spend descending (defensive — the SQL already
 * orders, but a parser should not depend on the transport preserving it).
 */
export function parseSpendRows(records: Array<Record<string, unknown>>): SpendRow[] {
	const rows: SpendRow[] = [];
	for (const record of records) {
		const label = String(record.label ?? '').trim();
		if (!label) continue;
		rows.push({ label, spend: toNumber(record.spend), calls: toNumber(record.calls) });
	}
	rows.sort((a, b) => b.spend - a.spend);
	return rows;
}

/** Total spend + total paid calls across a window's rows. */
export function summarizeSpend(rows: SpendRow[]): { totalSpend: number; totalCalls: number } {
	return rows.reduce(
		(acc, row) => ({
			totalSpend: acc.totalSpend + row.spend,
			totalCalls: acc.totalCalls + row.calls
		}),
		{ totalSpend: 0, totalCalls: 0 }
	);
}

/**
 * Keep the top `topN` rows and fold the remainder into a "+K more ($Y)" bucket.
 */
export function rollupSpendRows(
	rows: SpendRow[],
	topN: number
): { shown: SpendRow[]; moreCount: number; moreSpend: number } {
	const shown = rows.slice(0, topN);
	const rest = rows.slice(topN);
	return {
		shown,
		moreCount: rest.length,
		moreSpend: rest.reduce((sum, row) => sum + row.spend, 0)
	};
}
