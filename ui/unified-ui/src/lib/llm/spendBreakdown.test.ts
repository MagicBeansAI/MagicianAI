import { describe, expect, it } from 'vitest';
import type { LocalDayBoundaries } from '$lib/today/pulseQueries';
import {
	buildSpendBreakdownQueries,
	parseSpendRows,
	summarizeSpend,
	rollupSpendRows
} from './spendBreakdown';

const BOUNDS: LocalDayBoundaries = {
	yesterdayStartMs: 1000,
	todayStartMs: 2000,
	tomorrowStartMs: 3000
};

describe('buildSpendBreakdownQueries', () => {
	const specs = buildSpendBreakdownQueries(BOUNDS);
	const pick = (window: string, dimension: string) =>
		specs.find((s) => s.window === window && s.dimension === dimension);

	it('emits exactly six specs: 3 windows x 2 dimensions', () => {
		expect(specs).toHaveLength(6);
		expect(new Set(specs.map((s) => `${s.window}:${s.dimension}`))).toEqual(
			new Set(['today:operation', 'today:model', '7d:operation', '7d:model', '30d:operation', '30d:model'])
		);
	});

	it('windows the today queries with the injected local-day boundary literals, not INTERVAL', () => {
		const sql = pick('today', 'operation')!.sql;
		expect(sql).toContain('timestamp_ms >= 2000');
		expect(sql).toContain('timestamp_ms < 3000');
		expect(sql).not.toContain('INTERVAL');
	});

	it('windows 7d and 30d as rolling INTERVAL windows off now()', () => {
		expect(pick('7d', 'model')!.sql).toContain(
			'timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)'
		);
		expect(pick('30d', 'model')!.sql).toContain(
			'timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS)'
		);
	});

	it('groups each spec by its dimension column, aliased to label, only for paid calls', () => {
		const op = pick('30d', 'operation')!.sql;
		expect(op).toContain(`COALESCE(NULLIF(operation, ''), 'unattributed') AS label`);
		expect(op).toContain(`GROUP BY COALESCE(NULLIF(operation, ''), 'unattributed')`);
		const model = pick('30d', 'model')!.sql;
		expect(model).toContain(`COALESCE(NULLIF(model, ''), 'unknown') AS label`);
		expect(model).toContain(`GROUP BY COALESCE(NULLIF(model, ''), 'unknown')`);
		for (const spec of specs) {
			expect(spec.sql).toContain('FROM llm_calls');
			expect(spec.sql).toContain('HAVING SUM(cost_usd) > 0');
			expect(spec.sql).toContain('ORDER BY spend DESC');
			// parity with the pulse band: exclude chunk-summary bookkeeping rows
			expect(spec.sql).toContain("(COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')");
		}
	});

	it('never drops rows from the window total via an IS NOT NULL dimension filter', () => {
		// Regression guard. The window total is summed client-side from these
		// rows, so a `WHERE <dimension> IS NOT NULL` silently removed paid calls
		// from the headline `$`: Spend read $2.20 while "Today vs yesterday"
		// read $2.33 for the byte-identical window, with nothing on screen
		// explaining the gap. Unattributed rows must be BUCKETED, never filtered.
		for (const spec of specs) {
			expect(spec.sql).not.toContain('operation IS NOT NULL');
			expect(spec.sql).not.toContain('model IS NOT NULL');
		}
	});
});

describe('parseSpendRows', () => {
	it('reads label/spend/calls, coerces stringified numbers, and sorts by spend desc', () => {
		const rows = parseSpendRows([
			{ label: 'agentic_decision', spend: 1.19, calls: '11' },
			{ label: 'memory_user_promotion', spend: '1.46', calls: 28 }
		]);
		expect(rows).toEqual([
			{ label: 'memory_user_promotion', spend: 1.46, calls: 28 },
			{ label: 'agentic_decision', spend: 1.19, calls: 11 }
		]);
	});

	it('drops rows with an empty/absent label', () => {
		const rows = parseSpendRows([
			{ label: '', spend: 5, calls: 1 },
			{ label: null, spend: 5, calls: 1 },
			{ label: 'chat_inline', spend: 0.07, calls: 1 }
		]);
		expect(rows.map((r) => r.label)).toEqual(['chat_inline']);
	});
});

describe('summarizeSpend', () => {
	it('totals spend and calls across the rows', () => {
		const total = summarizeSpend([
			{ label: 'a', spend: 1.46, calls: 28 },
			{ label: 'b', spend: 1.19, calls: 11 }
		]);
		expect(total.totalSpend).toBeCloseTo(2.65, 5);
		expect(total.totalCalls).toBe(39);
	});

	it('is zero for an empty window', () => {
		expect(summarizeSpend([])).toEqual({ totalSpend: 0, totalCalls: 0 });
	});
});

describe('rollupSpendRows', () => {
	const rows: Array<{ label: string; spend: number; calls: number }> = [
		{ label: 'a', spend: 5, calls: 1 },
		{ label: 'b', spend: 4, calls: 1 },
		{ label: 'c', spend: 3, calls: 1 },
		{ label: 'd', spend: 2, calls: 1 },
		{ label: 'e', spend: 1, calls: 1 }
	];

	it('keeps the top N and folds the remainder into a more bucket', () => {
		const { shown, moreCount, moreSpend } = rollupSpendRows(rows, 3);
		expect(shown.map((r) => r.label)).toEqual(['a', 'b', 'c']);
		expect(moreCount).toBe(2);
		expect(moreSpend).toBeCloseTo(3, 5); // d(2) + e(1)
	});

	it('shows everything with no more-bucket when rows fit within N', () => {
		const { shown, moreCount, moreSpend } = rollupSpendRows(rows, 10);
		expect(shown).toHaveLength(5);
		expect(moreCount).toBe(0);
		expect(moreSpend).toBe(0);
	});
});
