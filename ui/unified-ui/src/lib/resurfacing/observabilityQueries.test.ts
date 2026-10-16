import { describe, expect, it } from 'vitest';

import {
	emptyObservability,
	mapObservabilityPayload,
	type ResurfacingObservability
} from './observabilityQueries';

// A representative full payload, shaped exactly like
// `get_resurfacing_observability_handler` emits.
const FULL_PAYLOAD = {
	pipeline: {
		// Intentionally out of order to prove the mapper re-orders to the
		// operator sequence scorer → curator → retention.
		retention: {
			total: 4,
			successes: 4,
			failures: 0,
			total_produced: 0,
			avg_duration_ms: 12.5,
			last_started_at: 1_700_000_300_000,
			last_error: null
		},
		scorer: {
			total: 10,
			successes: 8,
			failures: 2,
			total_produced: 25,
			avg_duration_ms: 42.0,
			last_started_at: 1_700_000_100_000,
			last_error: 'embed timeout'
		},
		curator: {
			total: 6,
			successes: 6,
			failures: 0,
			total_produced: 9,
			avg_duration_ms: 8.0,
			last_started_at: 1_700_000_200_000,
			last_error: null
		}
	},
	recent_runs: [
		{
			kind: 'scorer',
			started_at: 1_700_000_100_000,
			duration_ms: 41,
			produced: 3,
			success: false,
			error: 'embed timeout'
		},
		{
			kind: 'curator',
			started_at: 1_700_000_200_000,
			duration_ms: 8,
			produced: 2,
			success: true,
			error: null
		}
	],
	funnel: {
		by_state: { candidate: 5, surfaced: 3, acted: 2, dismissed: 1, snoozed: 0 },
		by_lane: { memory: 6, task: 3, comm: 2 },
		pending: 5,
		surfaced: 3,
		detail: [
			{ state: 'candidate', source_kind: 'memory', count: 4 },
			{ state: 'surfaced', source_kind: 'task', count: 3 }
		]
	},
	watermarks: [
		{ corpus_kind: 'memory', cursor: 1_700_000_000_000 },
		{ corpus_kind: 'task', cursor: 1_699_000_000_000 }
	],
	sizes: {
		candidates: 11,
		embeddings: 11,
		phrasing: 3,
		dismissed_signals: 1,
		affinity_signals: 2,
		runs: 20,
		recommendations: 4,
		action_claims: 3,
		action_events: 8,
		routing_repairs: 2
	},
	briefs: {
		comm_total: 9,
		comm_surfaced: 3,
		with_brief: 7,
		legacy: 2,
		complete: 4,
		partial: 2,
		source_omits_details: 1
	},
	recommendations: {
		shown: 4,
		selected: 2,
		completed: 1,
		acceptance_rate: 0.5,
		completion_rate: 0.5,
		by_kind: { create_task: { recommended: 4, selected: 2, completed: 1 } }
	},
	actions: {
		started: 3,
		completed: 2,
		failed: 1,
		completion_rate: 2 / 3,
		by_kind: { create_task: { started: 3, completed: 2, failed: 1 } },
		errors: { downstream_failed: 1 }
	},
	routing_repair: {
		total: 2,
		by_outcome: { accepted_worth_a_look: 1, rerouted_or_withheld: 1 }
	},
	engagement: [
		{
			source_kind: 'memory',
			positive: 4,
			negative: 1,
			engagement_rate: 0.8,
			utility_multiplier: 1.25
		}
	]
};

describe('mapObservabilityPayload', () => {
	it('maps a full payload into the typed shape', () => {
		const obs = mapObservabilityPayload(FULL_PAYLOAD);

		// Pipeline re-ordered to scorer → curator → retention.
		expect(obs.pipeline.map((p) => p.kind)).toEqual(['scorer', 'curator', 'retention']);
		const scorer = obs.pipeline[0];
		expect(scorer.total).toBe(10);
		expect(scorer.failures).toBe(2);
		expect(scorer.total_produced).toBe(25);
		expect(scorer.avg_duration_ms).toBe(42);
		expect(scorer.last_started_at).toBe(1_700_000_100_000);
		expect(scorer.last_error).toBe('embed timeout');
		expect(obs.pipeline[2].last_error).toBeNull();

		expect(obs.recent_runs).toHaveLength(2);
		expect(obs.recent_runs[0]).toMatchObject({
			kind: 'scorer',
			duration_ms: 41,
			produced: 3,
			success: false,
			error: 'embed timeout'
		});
		expect(obs.recent_runs[1].success).toBe(true);
		expect(obs.recent_runs[1].error).toBeNull();

		expect(obs.funnel.pending).toBe(5);
		expect(obs.funnel.surfaced).toBe(3);
		expect(obs.funnel.by_state.acted).toBe(2);
		expect(obs.funnel.by_lane.memory).toBe(6);
		expect(obs.funnel.detail).toHaveLength(2);

		expect(obs.watermarks).toHaveLength(2);
		expect(obs.watermarks[0]).toEqual({ corpus_kind: 'memory', cursor: 1_700_000_000_000 });

		expect(obs.sizes.candidates).toBe(11);
		expect(obs.sizes.runs).toBe(20);
		expect(obs.sizes.routing_repairs).toBe(2);
		expect(obs.briefs.legacy).toBe(2);
		expect(obs.recommendations.acceptance_rate).toBe(0.5);
		expect(obs.actions.errors.downstream_failed).toBe(1);
		expect(obs.routing_repair.by_outcome.rerouted_or_withheld).toBe(1);

		expect(obs.engagement).toHaveLength(1);
		expect(obs.engagement[0]).toEqual({
			source_kind: 'memory',
			positive: 4,
			negative: 1,
			engagement_rate: 0.8,
			utility_multiplier: 1.25
		});
	});

	it('coerces stringified numerics from the wire (SQLite BIGINT-as-string)', () => {
		const obs = mapObservabilityPayload({
			pipeline: { scorer: { total: '7', successes: '7', avg_duration_ms: '3.5' } },
			sizes: { candidates: '11', runs: '20' },
			funnel: { by_state: { candidate: '5' }, pending: '5' }
		});
		expect(obs.pipeline[0].total).toBe(7);
		expect(obs.pipeline[0].avg_duration_ms).toBe(3.5);
		expect(obs.sizes.candidates).toBe(11);
		expect(obs.funnel.by_state.candidate).toBe(5);
		expect(obs.funnel.pending).toBe(5);
	});

	it('returns safe defaults for missing sections (engine has not run yet)', () => {
		const obs = mapObservabilityPayload({});
		expect(obs.pipeline).toEqual([]);
		expect(obs.recent_runs).toEqual([]);
		expect(obs.watermarks).toEqual([]);
		expect(obs.engagement).toEqual([]);
		expect(obs.funnel).toEqual({
			by_state: {},
			by_lane: {},
			candidate_pool: 0,
			cooling: 0,
			eligible: 0,
			pending: 0,
			surfaced: 0,
			detail: []
		});
		expect(obs.sizes).toEqual({
			candidates: 0,
			embeddings: 0,
			phrasing: 0,
			dismissed_signals: 0,
			affinity_signals: 0,
			runs: 0,
			recommendations: 0,
			action_claims: 0,
			action_events: 0,
			routing_repairs: 0
		});
	});

	it('never throws on malformed / non-object input', () => {
		for (const bad of [null, undefined, 42, 'nope', [], { pipeline: 5, recent_runs: 'x', funnel: 9, sizes: [], engagement: {} }]) {
			expect(() => mapObservabilityPayload(bad)).not.toThrow();
		}
		const obs = mapObservabilityPayload(null);
		expect(obs).toEqual(emptyObservability());
	});

	it('drops non-object rows inside array sections without throwing', () => {
		const obs = mapObservabilityPayload({
			recent_runs: [null, 3, { kind: 'scorer', duration_ms: 5, success: true }],
			engagement: ['nope', { source_kind: 'task', positive: 1, negative: 0 }],
			watermarks: [42, { corpus_kind: 'memory', cursor: 1 }],
			funnel: { detail: [null, { state: 'acted', source_kind: 'memory', count: 2 }] }
		});
		expect(obs.recent_runs).toHaveLength(1);
		expect(obs.recent_runs[0].kind).toBe('scorer');
		expect(obs.engagement).toHaveLength(1);
		expect(obs.engagement[0].source_kind).toBe('task');
		expect(obs.watermarks).toHaveLength(1);
		expect(obs.funnel.detail).toHaveLength(1);
	});

	it('maps never-run timestamps to null and missing errors to null', () => {
		const obs: ResurfacingObservability = mapObservabilityPayload({
			pipeline: { scorer: { total: 0, last_started_at: null } },
			recent_runs: [{ kind: 'scorer', started_at: 'not-a-number' }]
		});
		expect(obs.pipeline[0].last_started_at).toBeNull();
		expect(obs.pipeline[0].last_error).toBeNull();
		expect(obs.recent_runs[0].started_at).toBeNull();
	});

	it('preserves extra pipeline kinds after the known ordering', () => {
		const obs = mapObservabilityPayload({
			pipeline: {
				retention: { total: 1 },
				experimental_pass: { total: 2 },
				scorer: { total: 3 }
			}
		});
		expect(obs.pipeline.map((p) => p.kind)).toEqual([
			'scorer',
			'retention',
			'experimental_pass'
		]);
	});
});
