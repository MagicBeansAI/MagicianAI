/**
 * Pure SQL builders + parsers for the Today's Pulse analytics band.
 *
 * Consumers: `todayPulseStore` (fires the three queries on a shared 60s
 * poll) and the /llm "Today vs yesterday" section (reuses the day-boundary
 * helper — never duplicate calendar-day math elsewhere).
 *
 * Endpoints (all POST `{ sql }`, all return `{ columns, rows }` — the same
 * shape `useLiveDataSource` consumes):
 * - `buildLlmPulseSql`    → /api/magician/v2/analytics/llm_calls/query
 * - `buildCodingPulseSql` → /api/magician/v2/analytics/query
 * - `buildMemoryPulseSql` → /api/magician/v2/analytics/memory_events/query
 *
 * Task counts are NOT an analytics query: `computeTaskCounts` derives them
 * in TS from the already-loaded task list (see its doc comment).
 *
 * Day-boundary contract: "today" and "yesterday" are LOCAL calendar days
 * (not rolling 24h). Boundaries are computed in TS from the injected `now`
 * and inlined as epoch-ms numeric literals — the SQL never does timezone
 * math, so DuckDB's UTC clock can't skew the windows. No `Date.now()`
 * anywhere in this module; callers inject the clock.
 *
 * Injection safety: never interpolate caller strings into these builders —
 * epoch-ms numbers only.
 *
 * Signal choices (verified against the Rust emitters + live event data,
 * 2026-07-05):
 * - Coding runs   → durable `coding.started` events with
 *   `source = 'coding_engine'` (one per run, emitted by the run_coding_task
 *   handler's analytics mirror on EVERY run start, regardless of outcome).
 *   Low-volume, so the events table's 24h retention sweep comfortably holds
 *   every row a today-count needs.
 * - Tasks completed → `computeTaskCounts` over the task list the UI already
 *   holds (taskStore), NOT an analytics query. Log-derived counts were
 *   rejected: they overcount execution runs (delegated/internal included),
 *   land only in the default-scope DB, and are string-fragile.
 * - Memories learned → `memory_events` rows with the learning-bridge
 *   promotion kinds (`learning_memory_candidate_promoted` /
 *   `…_review_promoted`) — each marks an actual write into a user memory
 *   tier. Eval cases → `event_kind = 'eval_case'` rows from the
 *   MemoryEvalRunner (`eval_pass` is NULL on runner errors, counted as
 *   not-passed).
 */

export interface PulseQueryResponse {
	columns: string[];
	rows: unknown[][];
}

export interface LlmPulse {
	spendToday: number;
	spendYesterday: number;
	callsToday: number;
	callsYesterday: number;
	/** 24 buckets indexed by local hour-of-day (0-23); unfilled hours are 0. */
	hourlySpend: number[];
	/** 24 call-count buckets indexed by local hour-of-day (0-23); unfilled hours are 0. */
	hourlyCalls?: number[];
	/** Top provider/model pair by call count, not spend, so zero-cost local calls count. */
	topProvider: { name: string; model: string; sharePct: number } | null;
}

export interface CodingPulse {
	codingRunsToday: number;
}

export interface TaskCounts {
	completedToday: number;
	completedYesterday: number;
}

export interface MemoryPulse {
	memoriesToday: number;
	evals: { casesToday: number; passesToday: number };
}

/** Assembled shape the band consumes — see `assemblePulseSnapshot`. */
export interface PulseSnapshot extends CodingPulse, MemoryPulse {
	llm: LlmPulse;
	tasks: TaskCounts;
}

export interface LocalDayBoundaries {
	yesterdayStartMs: number;
	todayStartMs: number;
	tomorrowStartMs: number;
}

/**
 * Local-midnight epoch-ms boundaries around the injected `now`. Built from
 * local calendar components (not `start + 86400000`) so DST-shifted days
 * keep correct boundaries.
 */
export function localDayBoundaries(now: Date): LocalDayBoundaries {
	const y = now.getFullYear();
	const m = now.getMonth();
	const d = now.getDate();
	return {
		yesterdayStartMs: new Date(y, m, d - 1).getTime(),
		todayStartMs: new Date(y, m, d).getTime(),
		tomorrowStartMs: new Date(y, m, d + 1).getTime()
	};
}

/**
 * One UNION ALL query over `llm_calls` with a `section` discriminator:
 * today's hourly spend buckets, today totals, yesterday totals, and today's
 * per-provider/model call counts. Hour buckets are hours-since-LOCAL-midnight
 * (half-hour UTC offsets like IST break UTC-aligned `timestamp_ms / 3600000`
 * bucketing).
 */
export function buildLlmPulseSql(now: Date): string {
	const { yesterdayStartMs, todayStartMs, tomorrowStartMs } = localDayBoundaries(now);
	const today = `timestamp_ms >= ${todayStartMs} AND timestamp_ms < ${tomorrowStartMs}`;
	const yesterday = `timestamp_ms >= ${yesterdayStartMs} AND timestamp_ms < ${todayStartMs}`;
	// A `logical_chunk_summary` (provider_attempt_count = 0) is a bookkeeping
	// aggregate of a chunked operation, not an actual model call — its real
	// provider/model calls are recorded separately. Exclude these from the
	// LLM-call COUNT/spend so those metrics reflect real model calls only.
	// Include harness aggregates: their physical attempts are not separately
	// recorded. NULL count also retains legacy rows predating the field.
	const realCall = `(COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')`;
	return (
		`SELECT 'today_hour' AS section, ` +
		`CAST(CAST(FLOOR((timestamp_ms - ${todayStartMs}) / 3600000.0) AS INTEGER) AS VARCHAR) AS k, ` +
		`NULL::VARCHAR AS model, COALESCE(SUM(cost_usd), 0) AS v1, COUNT(*) AS v2 ` +
		`FROM llm_calls WHERE ${today} AND ${realCall} GROUP BY 2 ` +
		`UNION ALL ` +
		`SELECT 'today_total', 'all', NULL::VARCHAR, COALESCE(SUM(cost_usd), 0), COUNT(*) ` +
		`FROM llm_calls WHERE ${today} AND ${realCall} ` +
		`UNION ALL ` +
		`SELECT 'yesterday_total', 'all', NULL::VARCHAR, COALESCE(SUM(cost_usd), 0), COUNT(*) ` +
		`FROM llm_calls WHERE ${yesterday} AND ${realCall} ` +
		`UNION ALL ` +
		// "Top model" is a model ranking, so its provider breakdown must use
		// the same physical-call population as the totals and require an actual
		// provider/model identity. Logical summary rows commonly carry only an
		// operation (for example `channel_classify`); treating that operation as
		// a model lets bookkeeping volume overwhelm the real model ranking.
		`SELECT 'today_provider', provider, model, COALESCE(SUM(cost_usd), 0), COUNT(*) ` +
		`FROM llm_calls WHERE ${today} AND ${realCall} ` +
		`AND NULLIF(TRIM(provider), '') IS NOT NULL AND NULLIF(TRIM(model), '') IS NOT NULL ` +
		`GROUP BY 2, 3`
	);
}

/**
 * Coding runs today from the durable `events` table (TIMESTAMPTZ column,
 * hence `epoch_ms(timestamp)` against the inlined boundaries). Keeps the
 * `section`/`n` shape of the other pulse queries so the parser stays
 * column-name based.
 */
export function buildCodingPulseSql(now: Date): string {
	const { todayStartMs, tomorrowStartMs } = localDayBoundaries(now);
	const today = `epoch_ms(timestamp) >= ${todayStartMs} AND epoch_ms(timestamp) < ${tomorrowStartMs}`;
	return (
		`SELECT 'coding_runs_today' AS section, COUNT(*) AS n ` +
		`FROM events WHERE event_type = 'coding.started' ` +
		`AND source = 'coding_engine' AND ${today}`
	);
}

/**
 * Memories learned today (learning-bridge promotions into user memory) +
 * eval cases today (count and passes) from `memory_events` (BIGINT
 * `timestamp_ms`, Parquet-backed — no retention decay here).
 */
export function buildMemoryPulseSql(now: Date): string {
	const { todayStartMs, tomorrowStartMs } = localDayBoundaries(now);
	const today = `timestamp_ms >= ${todayStartMs} AND timestamp_ms < ${tomorrowStartMs}`;
	return (
		`SELECT 'memories_today' AS section, CAST(COUNT(*) AS DOUBLE) AS n, CAST(0 AS DOUBLE) AS passes ` +
		`FROM memory_events WHERE ${today} ` +
		`AND event_kind IN ('learning_memory_candidate_promoted', 'learning_memory_candidate_review_promoted') ` +
		`UNION ALL ` +
		`SELECT 'evals_today', CAST(COUNT(*) AS DOUBLE), CAST(COALESCE(SUM(CASE WHEN eval_pass THEN 1 ELSE 0 END), 0) AS DOUBLE) ` +
		`FROM memory_events WHERE ${today} AND event_kind = 'eval_case'`
	);
}

/** Defensive numeric coercion for endpoint cell values (numbers arrive as
 * JSON numbers, but stringified BIGINTs and NULL aggregates must not NaN). */
export function toNumber(value: unknown): number {
	if (typeof value === 'number') return Number.isFinite(value) ? value : 0;
	if (typeof value === 'string') {
		const n = Number(value);
		return Number.isFinite(n) ? n : 0;
	}
	if (typeof value === 'boolean') return value ? 1 : 0;
	if (typeof value === 'object' && value !== null) {
		const obj = value as Record<string, unknown>;
		if ('lower' in obj && 'upper' in obj) {
			const lower = Number(obj.lower);
			const upper = Number(obj.upper);
			if (upper === 0) return Number.isFinite(lower) ? lower : 0;
			const n = upper * Math.pow(2, 64) + lower;
			return Number.isFinite(n) ? n : 0;
		}
	}
	return 0;
}

/** Column-name-based cell lookup — resilient to column reordering. */
function columnIndex(res: PulseQueryResponse, name: string): number {
	return res.columns.indexOf(name);
}

export function parseLlmPulse(res: PulseQueryResponse): LlmPulse {
	const out: LlmPulse = {
		spendToday: 0,
		spendYesterday: 0,
		callsToday: 0,
		callsYesterday: 0,
		hourlySpend: new Array(24).fill(0),
		hourlyCalls: new Array(24).fill(0),
		topProvider: null
	};
	const sectionIdx = columnIndex(res, 'section');
	const kIdx = columnIndex(res, 'k');
	const modelIdx = columnIndex(res, 'model');
	const v1Idx = columnIndex(res, 'v1');
	const v2Idx = columnIndex(res, 'v2');
	if (sectionIdx < 0 || kIdx < 0 || v1Idx < 0 || v2Idx < 0) return out;

	const providers: Array<{ name: string; model: string; calls: number }> = [];
	for (const row of res.rows) {
		const section = String(row[sectionIdx] ?? '');
		const v1 = toNumber(row[v1Idx]);
		const v2 = toNumber(row[v2Idx]);
		if (section === 'today_hour') {
			const hour = toNumber(row[kIdx]);
			if (Number.isInteger(hour) && hour >= 0 && hour < 24) {
				out.hourlySpend[hour] += v1;
				if (out.hourlyCalls) {
					out.hourlyCalls[hour] += v2;
				}
			}
		} else if (section === 'today_total') {
			out.spendToday = v1;
			out.callsToday = v2;
		} else if (section === 'yesterday_total') {
			out.spendYesterday = v1;
			out.callsYesterday = v2;
		} else if (section === 'today_provider') {
			const name = String(row[kIdx] ?? '').trim() || 'unknown';
			const model =
				modelIdx >= 0 ? String(row[modelIdx] ?? '').trim() || 'unknown' : 'unknown';
			// Older/cached responses may still contain the former synthetic
			// `unknown` + operation fallback row. It is not a model and must never
			// win the "Top model" chip even when its bookkeeping count is largest.
			if (v2 > 0 && name !== 'unknown' && model !== 'unknown') {
				providers.push({ name, model, calls: v2 });
			}
		}
	}

	const providerTotal = providers.reduce((sum, p) => sum + p.calls, 0);
	if (providerTotal > 0) {
		const top = providers.reduce((a, b) => (b.calls > a.calls ? b : a));
		out.topProvider = {
			name: top.name,
			model: top.model,
			sharePct: (top.calls * 100) / providerTotal
		};
	}
	return out;
}

export function parseCodingPulse(res: PulseQueryResponse): CodingPulse {
	const out: CodingPulse = { codingRunsToday: 0 };
	const sectionIdx = columnIndex(res, 'section');
	const nIdx = columnIndex(res, 'n');
	if (sectionIdx < 0 || nIdx < 0) return out;
	for (const row of res.rows) {
		if (String(row[sectionIdx] ?? '') === 'coding_runs_today') {
			out.codingRunsToday = toNumber(row[nIdx]);
		}
	}
	return out;
}

/**
 * Minimal structural pick of what taskStore's `Task` carries — status is a
 * string union there and `updatedAt` an ISO string, but this pure module
 * must not import the store, and other callers may hold epoch-ms numbers.
 */
export interface TaskCountsInput {
	status: string;
	updatedAt?: number | string | null;
}

/**
 * Tasks completed today/yesterday from the task list the UI already holds.
 * A task counts when `status === 'completed'` and its `updatedAt` falls in
 * the local today/yesterday window (same `localDayBoundaries` contract as
 * the SQL builders). `updatedAt` is accepted as epoch-ms number or ISO
 * string; missing/unparseable timestamps are excluded.
 *
 * Caveat: `updatedAt` approximates completion time — any later touch of a
 * completed task (retitle, tag edit) re-dates it into that day's count.
 */
export function computeTaskCounts(
	tasks: ReadonlyArray<TaskCountsInput>,
	now: Date
): TaskCounts {
	const { yesterdayStartMs, todayStartMs, tomorrowStartMs } = localDayBoundaries(now);
	const out: TaskCounts = { completedToday: 0, completedYesterday: 0 };
	for (const task of tasks) {
		if (task.status !== 'completed') continue;
		const ms = timestampToMs(task.updatedAt);
		if (ms === null) continue;
		if (ms >= todayStartMs && ms < tomorrowStartMs) out.completedToday += 1;
		else if (ms >= yesterdayStartMs && ms < todayStartMs) out.completedYesterday += 1;
	}
	return out;
}

function timestampToMs(value: number | string | null | undefined): number | null {
	if (typeof value === 'number') return Number.isFinite(value) ? value : null;
	if (typeof value === 'string') {
		const ms = Date.parse(value);
		return Number.isFinite(ms) ? ms : null;
	}
	return null;
}

/**
 * Assemble the band's snapshot from the three parsed query slices plus the
 * task counts, which are computed from the task list (`computeTaskCounts`)
 * rather than parsed from SQL rows — the store wires that source.
 */
export function assemblePulseSnapshot(
	llm: LlmPulse,
	coding: CodingPulse,
	memory: MemoryPulse,
	tasks: TaskCounts
): PulseSnapshot {
	return { llm, tasks, ...coding, ...memory };
}

export function parseMemoryPulse(res: PulseQueryResponse): MemoryPulse {
	const out: MemoryPulse = {
		memoriesToday: 0,
		evals: { casesToday: 0, passesToday: 0 }
	};
	const sectionIdx = columnIndex(res, 'section');
	const nIdx = columnIndex(res, 'n');
	const passesIdx = columnIndex(res, 'passes');
	if (sectionIdx < 0 || nIdx < 0) return out;
	for (const row of res.rows) {
		const section = String(row[sectionIdx] ?? '');
		const n = toNumber(row[nIdx]);
		if (section === 'memories_today') {
			out.memoriesToday = n;
		} else if (section === 'evals_today') {
			out.evals.casesToday = n;
			out.evals.passesToday = passesIdx >= 0 ? toNumber(row[passesIdx]) : 0;
		}
	}
	return out;
}

/**
 * Sub-cent currency drift is noise: it renders as no delta here, and
 * `pulseFormat`'s `spendTone` shares this constant so a delta too small to
 * render is also too small to tint. Single source — never inline 0.005.
 */
export const CURRENCY_DELTA_NOISE_USD = 0.005;

/**
 * Delta chip text. `count` deltas render as rounded percentages (`+18%`),
 * `currency` deltas as absolute dollars (`−$1.26` — U+2212 minus, house
 * typography). Zero-yesterday-with-activity reads "new today" (never ∞%);
 * noise deltas (equal counts, sub-cent currency drift) render empty so the
 * chip stays quiet.
 */
export function formatDelta(
	today: number,
	yesterday: number,
	kind: 'currency' | 'count'
): string {
	if (today === 0 && yesterday === 0) return '';
	if (yesterday === 0 && today > 0) return 'new today';
	const diff = today - yesterday;
	if (kind === 'currency') {
		if (Math.abs(diff) < CURRENCY_DELTA_NOISE_USD) return '';
		const sign = diff > 0 ? '+' : '−';
		return `${sign}$${Math.abs(diff).toFixed(2)}`;
	}
	const pct = Math.round((diff / yesterday) * 100);
	if (pct === 0) return '';
	const sign = pct > 0 ? '+' : '−';
	return `${sign}${Math.abs(pct)}%`;
}

/**
 * True when every metric is zero — the band hides entirely (no dead chrome
 * on the calmest page). Nonzero hourly buckets imply nonzero spendToday, and
 * a topProvider implies nonzero callsToday, so the flat fields cover everything.
 */
export function pulseIsEmpty(s: PulseSnapshot): boolean {
	return (
		s.llm.spendToday === 0 &&
		s.llm.spendYesterday === 0 &&
		s.llm.callsToday === 0 &&
		s.llm.callsYesterday === 0 &&
		s.tasks.completedToday === 0 &&
		s.tasks.completedYesterday === 0 &&
		s.codingRunsToday === 0 &&
		s.memoriesToday === 0 &&
		s.evals.casesToday === 0 &&
		s.evals.passesToday === 0
	);
}
