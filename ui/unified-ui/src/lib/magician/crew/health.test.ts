import { describe, expect, it } from 'vitest';
import { healthBand, healthByAgent, type CrewHealthResponse } from './health';

function response(): CrewHealthResponse {
	return {
		schema_version: 'crew_health.v1',
		formula_version: 1,
		generated_at: '2026-07-10T10:00:00Z',
		scope: { principal: 'anonymous', workspace: 'default' },
		availability: {
			status: 'available',
			llm_analytics: true,
			durable_history: true,
			limitations: []
		},
		agents: [
			{
				agent_id: 'presto',
				overall: {
					score: 82,
					band: 'good',
					observed_at_ms: 1,
					formula_version: 1,
					coverage: { ratio: 1, level: 'high', present: [], missing: [] },
					inputs: {
						runtime_status: 'running',
						state: 'working',
						observation_window_started_at_ms: 0,
						observation_window_ended_at_ms: 1,
						calls_7d: 10,
						spend_usd_7d: 0.2,
						success_rate_7d: 0.9,
						last_llm_call_at_ms: 1,
						last_task_activity_at_ms: 1,
						last_activity_at_ms: 1,
						llm_analytics_available: true,
						task_activity_available: true
					},
					contributions: { baseline: 70, quality: 0, recency: 7, state: 5 }
				},
				rolling_7d: {
					window_started_at_ms: 0,
					window_ended_at_ms: 1,
					calls: 10,
					spend_usd: 0.2,
					success_rate: 0.9,
					last_call_at_ms: 1,
					score_average: 80,
					score_delta: 4,
					trend: 'improving',
					sample_days: 3
				},
				history: []
			}
		]
	};
}

describe('crew health projection helpers', () => {
	it('indexes the canonical response by stable agent id', () => {
		const indexed = healthByAgent(response());
		expect(indexed?.get('presto')?.overall.score).toBe(82);
	});

	it('uses the backend band thresholds for fallback rendering', () => {
		expect(healthBand(70)).toBe('good');
		expect(healthBand(40)).toBe('fair');
		expect(healthBand(39)).toBe('poor');
	});
});
