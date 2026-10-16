import { describe, expect, it } from 'vitest';

import {
	assemblePulseSnapshot,
	buildCodingPulseSql,
	buildLlmPulseSql,
	buildMemoryPulseSql,
	computeTaskCounts,
	formatDelta,
	localDayBoundaries,
	parseCodingPulse,
	parseLlmPulse,
	parseMemoryPulse,
	pulseIsEmpty,
	type PulseQueryResponse,
	type PulseSnapshot
} from './pulseQueries';

// All dates are local-time constructions so the expectations are
// timezone-agnostic: the builder and the test derive boundaries from the
// same local calendar day, whatever TZ vitest runs in.

const MID_DAY = new Date(2026, 6, 4, 15, 30); // 2026-07-04 15:30 local
const TODAY_START = new Date(2026, 6, 4).getTime();
const YESTERDAY_START = new Date(2026, 6, 3).getTime();
const TOMORROW_START = new Date(2026, 6, 5).getTime();

function emptyResponse(): PulseQueryResponse {
	return { columns: [], rows: [] };
}

function zeroSnapshot(): PulseSnapshot {
	return assemblePulseSnapshot(
		parseLlmPulse(emptyResponse()),
		parseCodingPulse(emptyResponse()),
		parseMemoryPulse(emptyResponse()),
		computeTaskCounts([], MID_DAY)
	);
}

describe('localDayBoundaries', () => {
	it('anchors to local midnight of the given day, yesterday, and tomorrow', () => {
		const b = localDayBoundaries(MID_DAY);
		expect(b.todayStartMs).toBe(TODAY_START);
		expect(b.yesterdayStartMs).toBe(YESTERDAY_START);
		expect(b.tomorrowStartMs).toBe(TOMORROW_START);
	});

	it('keeps a midnight-adjacent now inside the just-started day', () => {
		// 00:00:30 — thirty seconds into July 4th must NOT bleed into July 3rd.
		const b = localDayBoundaries(new Date(2026, 6, 4, 0, 0, 30));
		expect(b.todayStartMs).toBe(TODAY_START);
		expect(b.yesterdayStartMs).toBe(YESTERDAY_START);
	});

	it('treats an exact-midnight now as the start of that day', () => {
		const b = localDayBoundaries(new Date(2026, 6, 4, 0, 0, 0));
		expect(b.todayStartMs).toBe(TODAY_START);
	});

	it('follows local calendar days across DST transitions (no fixed 24h math)', () => {
		// 2026-03-08 is the US spring-forward date; in DST zones the local day
		// is 23h long. Boundaries must equal local-midnight constructions, not
		// todayStart + 86400000.
		const b = localDayBoundaries(new Date(2026, 2, 8, 15, 0));
		expect(b.todayStartMs).toBe(new Date(2026, 2, 8).getTime());
		expect(b.yesterdayStartMs).toBe(new Date(2026, 2, 7).getTime());
		expect(b.tomorrowStartMs).toBe(new Date(2026, 2, 9).getTime());
	});
});

describe('buildLlmPulseSql', () => {
	const sql = buildLlmPulseSql(MID_DAY);

	it('inlines local-day epoch-ms boundaries as numeric literals (no SQL time math)', () => {
		expect(sql).toContain(`timestamp_ms >= ${TODAY_START}`);
		expect(sql).toContain(`timestamp_ms < ${TOMORROW_START}`);
		expect(sql).toContain(`timestamp_ms >= ${YESTERDAY_START}`);
		expect(sql).toContain(`timestamp_ms < ${TODAY_START}`);
		expect(sql).not.toMatch(/now\(\)|current_date|current_timestamp|interval/i);
	});

	it('returns one UNION ALL query with all four sections', () => {
		expect(sql).toContain(`'today_hour'`);
		expect(sql).toContain(`'today_total'`);
		expect(sql).toContain(`'yesterday_total'`);
		expect(sql).toContain(`'today_provider'`);
		expect(sql.match(/UNION ALL/g)).toHaveLength(3);
	});

	it('uses the verified llm_calls columns', () => {
		expect(sql).toContain('cost_usd');
		expect(sql).toContain('timestamp_ms');
		expect(sql).toContain('provider');
		expect(sql).toContain('model');
		expect(sql).toContain('FROM llm_calls');
	});

	it('excludes non-model summary rows from totals and the top-model population', () => {
		// `logical_chunk_summary` rows (provider_attempt_count = 0) are bookkeeping
		// aggregates, not model calls — keep them out of all four query sections.
		expect(sql.match(/COALESCE\(provider_attempt_count, 1\) <> 0/g)).toHaveLength(4);
		expect(sql.match(/OR response_kind = 'harness_aggregate'/g)).toHaveLength(4);
		// A top-model candidate must carry the actual physical provider/model.
		expect(sql).toContain(`NULLIF(TRIM(provider), '') IS NOT NULL`);
		expect(sql).toContain(`NULLIF(TRIM(model), '') IS NOT NULL`);
		expect(sql).not.toContain(`NULLIF(operation, '')`);
	});

	it('buckets hours relative to LOCAL midnight, not UTC-aligned hours', () => {
		// Half-hour offsets (IST is UTC+5:30) make UTC-hour bucketing wrong;
		// the hour index must be derived from (timestamp_ms - todayStartMs).
		expect(sql).toContain(`timestamp_ms - ${TODAY_START}`);
		expect(sql).toContain('3600000');
	});

	it('shifts every boundary at a midnight-adjacent now', () => {
		const midnight = buildLlmPulseSql(new Date(2026, 6, 4, 0, 0, 30));
		expect(midnight).toContain(`timestamp_ms >= ${TODAY_START}`);
		expect(midnight).toContain(`timestamp_ms >= ${YESTERDAY_START}`);
	});
});

describe('buildCodingPulseSql', () => {
	const sql = buildCodingPulseSql(MID_DAY);

	it('counts coding runs via the durable coding_engine coding.started event', () => {
		expect(sql).toContain(`event_type = 'coding.started'`);
		expect(sql).toContain(`source = 'coding_engine'`);
		expect(sql).toContain(`'coding_runs_today'`);
	});

	it('serves ONLY coding runs — no log-derived task counting', () => {
		expect(sql).not.toContain(`'log'`);
		expect(sql).not.toContain('Transitioning');
		expect(sql).not.toContain('tasks_completed');
	});

	it('filters the TIMESTAMPTZ column through epoch_ms against inlined today boundaries', () => {
		expect(sql).toContain(`epoch_ms(timestamp) >= ${TODAY_START}`);
		expect(sql).toContain(`epoch_ms(timestamp) < ${TOMORROW_START}`);
		expect(sql).not.toMatch(/now\(\)|current_date|current_timestamp|interval/i);
	});
});

describe('buildMemoryPulseSql', () => {
	const sql = buildMemoryPulseSql(MID_DAY);

	it('counts learned memories via the two learning-bridge promotion kinds', () => {
		expect(sql).toContain(`'learning_memory_candidate_promoted'`);
		expect(sql).toContain(`'learning_memory_candidate_review_promoted'`);
		expect(sql).toContain(`'memories_today'`);
	});

	it('counts eval cases and passes via eval_case rows', () => {
		expect(sql).toContain(`event_kind = 'eval_case'`);
		expect(sql).toContain('eval_pass');
		expect(sql).toContain(`'evals_today'`);
	});

	it('filters timestamp_ms against inlined local-day boundaries', () => {
		expect(sql).toContain(`timestamp_ms >= ${TODAY_START}`);
		expect(sql).toContain(`timestamp_ms < ${TOMORROW_START}`);
		expect(sql).not.toMatch(/now\(\)|current_date|current_timestamp|interval/i);
	});
});

describe('parseLlmPulse', () => {
	const fullResponse: PulseQueryResponse = {
		columns: ['section', 'k', 'model', 'v1', 'v2'],
		rows: [
			['today_hour', '9', null, 0.4, 12],
			['today_hour', '10', null, 0.85, 30],
			['today_total', 'all', null, 1.25, 42],
			['yesterday_total', 'all', null, 2.51, 77],
			['today_provider', 'anthropic', 'claude', 0.75, 20],
			['today_provider', 'openai', 'gpt-5.6-terra', 0.5, 22]
		]
	};

	it('extracts totals, hourly buckets, and the top provider/model by call share', () => {
		const llm = parseLlmPulse(fullResponse);
		expect(llm.spendToday).toBe(1.25);
		expect(llm.callsToday).toBe(42);
		expect(llm.spendYesterday).toBe(2.51);
		expect(llm.callsYesterday).toBe(77);
		expect(llm.hourlySpend).toHaveLength(24);
		expect(llm.hourlySpend[9]).toBe(0.4);
		expect(llm.hourlySpend[10]).toBe(0.85);
		expect(llm.hourlySpend[0]).toBe(0);
		expect(llm.topProvider?.name).toBe('openai');
		expect(llm.topProvider?.model).toBe('gpt-5.6-terra');
		expect(llm.topProvider?.sharePct).toBeCloseTo((22 * 100) / 42);
	});

	it('never presents a high-volume logical operation fallback as the top model', () => {
		const llm = parseLlmPulse({
			columns: ['section', 'k', 'model', 'v1', 'v2'],
			rows: [
				['today_total', 'all', null, 0, 671],
				// Shape emitted by the former query for logical summary rows.
				['today_provider', 'unknown', 'channel_classify', 0, 2060],
				['today_provider', 'ollama', 'gemma4:12b', 0, 671]
			]
		});

		expect(llm.topProvider).toEqual({
			name: 'ollama',
			model: 'gemma4:12b',
			sharePct: 100
		});
	});

	it('returns an all-zero slice for an empty result set', () => {
		const llm = parseLlmPulse(emptyResponse());
		expect(llm.spendToday).toBe(0);
		expect(llm.spendYesterday).toBe(0);
		expect(llm.callsToday).toBe(0);
		expect(llm.callsYesterday).toBe(0);
		expect(llm.hourlySpend).toEqual(new Array(24).fill(0));
		expect(llm.topProvider).toBeNull();
	});

	it('handles partial results (totals only, no provider or hour rows)', () => {
		const llm = parseLlmPulse({
			columns: ['section', 'k', 'v1', 'v2'],
			rows: [['today_total', 'all', 0.1, 3]]
		});
		expect(llm.spendToday).toBe(0.1);
		expect(llm.callsToday).toBe(3);
		expect(llm.spendYesterday).toBe(0);
		expect(llm.topProvider).toBeNull();
		expect(llm.hourlySpend).toEqual(new Array(24).fill(0));
	});

	it('coerces stringified numerics and null aggregates', () => {
		const llm = parseLlmPulse({
			columns: ['section', 'k', 'v1', 'v2'],
			rows: [
				['today_total', 'all', '1.5', '7'],
				['yesterday_total', 'all', null, 0]
			]
		});
		expect(llm.spendToday).toBe(1.5);
		expect(llm.callsToday).toBe(7);
		expect(llm.spendYesterday).toBe(0);
	});

	it('ignores hour buckets outside 0-23', () => {
		const llm = parseLlmPulse({
			columns: ['section', 'k', 'v1', 'v2'],
			rows: [
				['today_hour', '24', 5, 1],
				['today_hour', '-1', 5, 1],
				['today_hour', '23', 0.2, 1]
			]
		});
		expect(llm.hourlySpend[23]).toBe(0.2);
		expect(llm.hourlySpend.reduce((a, b) => a + b, 0)).toBe(0.2);
	});

	it('resolves cells by column NAME, surviving reordered columns', () => {
		// Same data as fullResponse's totals/provider rows, but the endpoint
		// returns the columns in a different order — the parser must keep
		// working because it indexes by name, never by position.
		const llm = parseLlmPulse({
			columns: ['v2', 'section', 'model', 'v1', 'k'],
			rows: [
				[42, 'today_total', null, 1.25, 'all'],
				[77, 'yesterday_total', null, 2.51, 'all'],
				[20, 'today_provider', 'claude', 0.75, 'anthropic'],
				[22, 'today_provider', 'gpt-5.6-terra', 0.5, 'openai'],
				[12, 'today_hour', null, 0.4, '9']
			]
		});
		expect(llm.spendToday).toBe(1.25);
		expect(llm.callsToday).toBe(42);
		expect(llm.spendYesterday).toBe(2.51);
		expect(llm.callsYesterday).toBe(77);
		expect(llm.hourlySpend[9]).toBe(0.4);
		expect(llm.topProvider?.name).toBe('openai');
		expect(llm.topProvider?.model).toBe('gpt-5.6-terra');
		expect(llm.topProvider?.sharePct).toBeCloseTo((22 * 100) / 42);
	});

	it('returns null topProvider when today has zero provider calls', () => {
		const llm = parseLlmPulse({
			columns: ['section', 'k', 'model', 'v1', 'v2'],
			rows: [['today_provider', 'anthropic', 'claude', 0, 0]]
		});
		expect(llm.topProvider).toBeNull();
	});

	it('counts zero-cost local calls in top provider share', () => {
		const llm = parseLlmPulse({
			columns: ['section', 'k', 'model', 'v1', 'v2'],
			rows: [
				['today_total', 'all', null, 2.47, 374],
				['today_provider', 'ollama', 'gemma4:26b-a4b-it-qat', 0, 343],
				['today_provider', 'openai', 'gpt-5.6-terra', 2.47, 31]
			]
		});
		expect(llm.topProvider?.name).toBe('ollama');
		expect(llm.topProvider?.model).toBe('gemma4:26b-a4b-it-qat');
		expect(llm.topProvider?.sharePct).toBeCloseTo((343 * 100) / 374);
	});
});

describe('parseCodingPulse', () => {
	it('extracts the coding-run count by section, coercing stringified BIGINTs', () => {
		const coding = parseCodingPulse({
			columns: ['section', 'n'],
			rows: [['coding_runs_today', '2']]
		});
		expect(coding.codingRunsToday).toBe(2);
	});

	it('zero-fills empty results and ignores unknown sections', () => {
		expect(parseCodingPulse(emptyResponse())).toEqual({ codingRunsToday: 0 });
		const unknown = parseCodingPulse({
			columns: ['section', 'n'],
			rows: [['tasks_completed_today', 4]]
		});
		expect(unknown.codingRunsToday).toBe(0);
	});
});

describe('computeTaskCounts', () => {
	// Local Date constructions keep the expectations timezone-agnostic,
	// matching the boundary-helper contract.
	const isoAt = (y: number, mo: number, d: number, h: number, mi: number) =>
		new Date(y, mo, d, h, mi).toISOString();

	it('buckets completed tasks into today vs yesterday by updatedAt', () => {
		const counts = computeTaskCounts(
			[
				{ status: 'completed', updatedAt: new Date(2026, 6, 4, 9, 0).getTime() },
				{ status: 'completed', updatedAt: new Date(2026, 6, 4, 23, 59).getTime() },
				{ status: 'completed', updatedAt: new Date(2026, 6, 3, 12, 0).getTime() },
				// Two days ago — outside both windows.
				{ status: 'completed', updatedAt: new Date(2026, 6, 2, 12, 0).getTime() }
			],
			MID_DAY
		);
		expect(counts).toEqual({ completedToday: 2, completedYesterday: 1 });
	});

	it('accepts ISO-string updatedAt (what taskStore actually carries)', () => {
		const counts = computeTaskCounts(
			[
				{ status: 'completed', updatedAt: isoAt(2026, 6, 4, 10, 30) },
				{ status: 'completed', updatedAt: isoAt(2026, 6, 3, 22, 0) }
			],
			MID_DAY
		);
		expect(counts).toEqual({ completedToday: 1, completedYesterday: 1 });
	});

	it('excludes tasks with missing or unparseable updatedAt', () => {
		const counts = computeTaskCounts(
			[
				{ status: 'completed' },
				{ status: 'completed', updatedAt: null },
				{ status: 'completed', updatedAt: 'not-a-date' },
				{ status: 'completed', updatedAt: Number.NaN }
			],
			MID_DAY
		);
		expect(counts).toEqual({ completedToday: 0, completedYesterday: 0 });
	});

	it('counts only status === completed, whatever the timestamp', () => {
		const todayMs = new Date(2026, 6, 4, 11, 0).getTime();
		const counts = computeTaskCounts(
			[
				{ status: 'running', updatedAt: todayMs },
				{ status: 'failed', updatedAt: todayMs },
				{ status: 'cancelled', updatedAt: todayMs },
				{ status: 'pending', updatedAt: todayMs }
			],
			MID_DAY
		);
		expect(counts).toEqual({ completedToday: 0, completedYesterday: 0 });
	});

	it('honors the injected now at the midnight edge', () => {
		// Thirty seconds into July 4th: a task updated one minute earlier
		// belongs to YESTERDAY; exact local midnight belongs to today.
		const justAfterMidnight = new Date(2026, 6, 4, 0, 0, 30);
		const counts = computeTaskCounts(
			[
				{ status: 'completed', updatedAt: new Date(2026, 6, 3, 23, 59).getTime() },
				{ status: 'completed', updatedAt: new Date(2026, 6, 4, 0, 0, 0).getTime() }
			],
			justAfterMidnight
		);
		expect(counts).toEqual({ completedToday: 1, completedYesterday: 1 });
	});

	it('returns zeros for an empty list', () => {
		expect(computeTaskCounts([], MID_DAY)).toEqual({
			completedToday: 0,
			completedYesterday: 0
		});
	});
});

describe('parseMemoryPulse', () => {
	it('extracts learned-memory and eval counts by section', () => {
		const memory = parseMemoryPulse({
			columns: ['section', 'n', 'passes'],
			rows: [
				['memories_today', 3, 0],
				['evals_today', 12, 11]
			]
		});
		expect(memory.memoriesToday).toBe(3);
		expect(memory.evals.casesToday).toBe(12);
		expect(memory.evals.passesToday).toBe(11);
	});

	it('zero-fills empty and partial results', () => {
		expect(parseMemoryPulse(emptyResponse())).toEqual({
			memoriesToday: 0,
			evals: { casesToday: 0, passesToday: 0 }
		});
		const partial = parseMemoryPulse({
			columns: ['section', 'n', 'passes'],
			rows: [['evals_today', 2, 2]]
		});
		expect(partial.memoriesToday).toBe(0);
		expect(partial.evals.casesToday).toBe(2);
	});
});

describe('formatDelta', () => {
	it('formats count deltas as rounded percentages', () => {
		expect(formatDelta(59, 50, 'count')).toBe('+18%');
		expect(formatDelta(41, 50, 'count')).toBe('−18%');
		expect(formatDelta(0, 4, 'count')).toBe('−100%');
	});

	it('formats currency deltas as absolute dollar amounts', () => {
		expect(formatDelta(1.25, 2.51, 'currency')).toBe('−$1.26');
		expect(formatDelta(2.51, 1.25, 'currency')).toBe('+$1.26');
	});

	it('says "new today" when yesterday was zero and today is not', () => {
		expect(formatDelta(5, 0, 'count')).toBe('new today');
		expect(formatDelta(0.42, 0, 'currency')).toBe('new today');
	});

	it('returns empty when both are zero', () => {
		expect(formatDelta(0, 0, 'count')).toBe('');
		expect(formatDelta(0, 0, 'currency')).toBe('');
	});

	it('suppresses noise deltas (equal counts, sub-cent currency drift)', () => {
		expect(formatDelta(50, 50, 'count')).toBe('');
		expect(formatDelta(1.2501, 1.2499, 'currency')).toBe('');
	});
});

describe('pulseIsEmpty', () => {
	it('is true when every metric is zero', () => {
		expect(pulseIsEmpty(zeroSnapshot())).toBe(true);
	});

	it('is false when any single metric is nonzero', () => {
		const spend = zeroSnapshot();
		spend.llm.spendToday = 0.01;
		expect(pulseIsEmpty(spend)).toBe(false);

		const yesterdayOnly = zeroSnapshot();
		yesterdayOnly.llm.callsYesterday = 3;
		expect(pulseIsEmpty(yesterdayOnly)).toBe(false);

		const tasks = zeroSnapshot();
		tasks.tasks.completedYesterday = 1;
		expect(pulseIsEmpty(tasks)).toBe(false);

		const coding = zeroSnapshot();
		coding.codingRunsToday = 1;
		expect(pulseIsEmpty(coding)).toBe(false);

		const memories = zeroSnapshot();
		memories.memoriesToday = 2;
		expect(pulseIsEmpty(memories)).toBe(false);

		const evals = zeroSnapshot();
		evals.evals.casesToday = 5;
		expect(pulseIsEmpty(evals)).toBe(false);
	});
});
