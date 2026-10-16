import { describe, expect, it } from 'vitest';
import {
	buildLlmUsageOverviewSql,
	llmUsageWindowWhere,
	parseLlmUsageOverview
} from './overview';

describe('LLM usage overview', () => {
	it('builds one aggregate query for all four canonical metrics', () => {
		const sql = buildLlmUsageOverviewSql('agent_id = \'crew-1\'');
		expect(sql).toContain('SUM(cost_usd) AS spend_usd');
		expect(sql).toContain('COUNT(*) AS calls');
		expect(sql).toContain('AS retry_rate');
		expect(sql).toContain('AVG(latency_ms) AS avg_latency_ms');
		expect(sql).toContain("WHERE agent_id = 'crew-1'");
	});

	it('preserves zero values while leaving unavailable aggregates null', () => {
		expect(
			parseLlmUsageOverview({
				spend_usd: 0,
				calls: '0',
				retry_rate: null,
				avg_latency_ms: undefined
			})
		).toEqual({ spendUsd: 0, calls: 0, retryRate: null, avgLatencyMs: null });
		expect(parseLlmUsageOverview(undefined)).toEqual({
			spendUsd: null,
			calls: null,
			retryRate: null,
			avgLatencyMs: null
		});
	});

	it('bounds the standalone overview window', () => {
		expect(llmUsageWindowWhere(7)).toContain('INTERVAL 7 DAYS');
		expect(llmUsageWindowWhere(0)).toContain('INTERVAL 1 DAYS');
		expect(llmUsageWindowWhere(Number.NaN)).toContain('INTERVAL 7 DAYS');
	});
});
