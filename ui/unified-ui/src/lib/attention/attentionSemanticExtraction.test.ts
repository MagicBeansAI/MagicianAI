import { describe, expect, it } from 'vitest';

import {
	parseAttentionSemanticExtractionHealth,
	semanticExtractionCountsReconcile,
	semanticExtractionCoverageReconciles,
	semanticExtractionSurfacesReconcile
} from './attentionSemanticExtraction';

const followUp = {
	active_total: 6,
	compatible_revision: 4,
	succeeded: 4,
	missing: 1,
	invalid: 0,
	pending: 0,
	in_flight: 0,
	retry: 1,
	dead: 0,
	coverage: 4 / 6
};

const worthALook = {
	active_total: 4,
	compatible_revision: 3,
	succeeded: 3,
	missing: 0,
	invalid: 1,
	pending: 0,
	in_flight: 0,
	retry: 0,
	dead: 0,
	coverage: 0.75
};

const payload = {
	semantic_extraction_health: {
		schema_version: 1,
		enabled: true,
		paused: false,
		pause_reason: null,
		degradation_reason: null,
		contract: {
			semantic_schema_version: 2,
			extractor_contract: 'attention-semantic-v2',
			prompt_version: 'attention-extract-v4',
			model: 'gpt-5-mini',
			profile: 'foreground-safe'
		},
		checkpoint: {
			cursor: 'candidate-10',
			updated_at: 1_725_000_000_000,
			lease_owner: null,
			lease_expires_at: null
		},
		totals: {
			active_total: 10,
			compatible_revision: 7,
			succeeded: 7,
			missing: 1,
			invalid: 1,
			pending: 0,
			in_flight: 0,
			retry: 1,
			dead: 0,
			coverage: 0.7
		},
		surfaces: { follow_up: followUp, worth_a_look: worthALook },
		queue: { pending: 2, in_flight: 1, retry: 1, dead: 0, next_retry_at: null }
	}
};

describe('semantic extraction health contract', () => {
	it('parses full active-universe surface counts, contract, queue, and checkpoint', () => {
		const parsed = parseAttentionSemanticExtractionHealth(payload);
		expect(parsed).toMatchObject({
			enabled: true,
			paused: false,
			contract: { semantic_schema_version: 2 },
			checkpoint: { cursor: 'candidate-10' },
			totals: { active_total: 10, compatible_revision: 7 },
			queue: {
				pending: 2,
				in_flight: 1,
				active_in_flight: 1,
				expired_in_flight: 0,
				retry: 1,
				dead: 0
			}
		});
		expect(semanticExtractionCountsReconcile(parsed!.totals)).toBe(true);
		expect(semanticExtractionCoverageReconciles(parsed!.totals)).toBe(true);
		expect(semanticExtractionSurfacesReconcile(parsed)).toBe(true);
	});

	it('accepts empty universes only with complete compatible coverage', () => {
		const empty = {
			active_total: 0,
			compatible_revision: 0,
			succeeded: 0,
			missing: 0,
			invalid: 0,
			pending: 0,
			in_flight: 0,
			retry: 0,
			dead: 0,
			coverage: 1
		};
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...payload.semantic_extraction_health,
					totals: empty,
					surfaces: { follow_up: empty, worth_a_look: empty },
					queue: { pending: 0, in_flight: 0, retry: 0, dead: 0 }
				}
			})?.totals.coverage
		).toBe(1);
	});

	it('fails closed on missing, malformed, partition, coverage, and surface mismatches', () => {
		expect(parseAttentionSemanticExtractionHealth(undefined)).toBeNull();
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...payload.semantic_extraction_health,
					pause_reason: 'unknown_reason'
				}
			})
		).toBeNull();
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...payload.semantic_extraction_health,
					totals: {
						...payload.semantic_extraction_health.totals,
						compatible_revision: 8,
						succeeded: 7
					}
				}
			})
		).toBeNull();
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...payload.semantic_extraction_health,
					totals: { ...payload.semantic_extraction_health.totals, pending: 3 }
				}
			})
		).toBeNull();
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...payload.semantic_extraction_health,
					totals: { ...payload.semantic_extraction_health.totals, coverage: 0.9 }
				}
			})
		).toBeNull();
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...payload.semantic_extraction_health,
					surfaces: {
						...payload.semantic_extraction_health.surfaces,
						worth_a_look: { ...worthALook, succeeded: 1, missing: 1 }
					}
				}
			})
		).toBeNull();
	});

	it('preserves paused and degraded diagnostics without authorizing client work', () => {
		const parsed = parseAttentionSemanticExtractionHealth({
			semantic_extraction_health: {
				...payload.semantic_extraction_health,
				paused: true,
				pause_reason: 'foreground_pressure',
				degradation_reason: 'checkpoint_lease_expired'
			}
		});
		expect(parsed).toMatchObject({
			paused: true,
			pause_reason: 'foreground_pressure',
			degradation_reason: 'checkpoint_lease_expired'
		});
	});

	it('parses active and expired lease partitions and rejects inconsistent totals', () => {
		const withExpiredLease = {
			semantic_extraction_health: {
				...payload.semantic_extraction_health,
				degradation_reason: 'semantic_extraction_expired_leases',
				queue: {
					pending: 2,
					in_flight: 3,
					active_in_flight: 1,
					expired_in_flight: 2,
					retry: 1,
					dead: 0,
					next_retry_at: null,
					oldest_ready_at: 1_725_000_000_000,
					last_succeeded_at: 1_725_000_000_100
				}
			}
		};
		expect(parseAttentionSemanticExtractionHealth(withExpiredLease)?.queue).toMatchObject({
			in_flight: 3,
			active_in_flight: 1,
			expired_in_flight: 2
		});
		expect(
			parseAttentionSemanticExtractionHealth({
				semantic_extraction_health: {
					...withExpiredLease.semantic_extraction_health,
					queue: {
						...withExpiredLease.semantic_extraction_health.queue,
						expired_in_flight: 3
					}
				}
			})
		).toBeNull();
	});
	it('excludes unsupported source families from coverage while reconciling all active rows', () => {
		const input = structuredClone(payload);
		Object.assign(input.semantic_extraction_health.totals, {active_total: 15, not_applicable: 5});
		Object.assign(input.semantic_extraction_health.surfaces.worth_a_look, {active_total: 9, not_applicable: 5});
		const parsed = parseAttentionSemanticExtractionHealth(input);
		expect(parsed?.totals).toMatchObject({active_total: 15, not_applicable: 5, coverage: 0.7});
		expect(semanticExtractionCountsReconcile(parsed!.totals)).toBe(true);
		expect(semanticExtractionCoverageReconciles(parsed!.totals)).toBe(true);
		Object.assign(input.semantic_extraction_health.surfaces.worth_a_look, {not_applicable: 10});
		expect(parseAttentionSemanticExtractionHealth(input)).toBeNull();
	});

});
