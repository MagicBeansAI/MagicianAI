import { describe, expect, it } from 'vitest';

import type { AttentionDecisionItem } from './attentionRouting';
import {
	parseAttentionBanditDecision,
	parseAttentionBanditHealth,
	parseAttentionPosteriorUpdate,
	posteriorUpdateMessage
} from './attentionBandit';

const decisionItem: AttentionDecisionItem = {
	decision_id: 'decision-1',
	candidate_id: 'candidate-1',
	source_revision: 'revision-1',
	baseline_route: 'follow_up',
	learned_route: 'follow_up',
	served_route: 'follow_up',
	routing_mode: 'shadow',
	routing_snapshot_id: 'routing-1',
	routing_model_version: 'router-v1',
	learned_route_confidence: 0.9,
	utility_margin: 0.2,
	route_reason: 'shadow_only',
	served_rank: 1,
	selected: true,
	route_applied: false,
	canary_assigned: false
};

const shadowDecision = {
	schema_version: 1,
	mode: 'shadow',
	policy_snapshot_id: 'policy-1',
	policy_model_version: 'contextual-ts-v1',
	posterior_version: 12,
	posterior_uncertainty: 0.31,
	proposed_position: 3,
	served_position: 1,
	served_propensity: 1,
	posterior_draw_count: 16,
	seed_identity: 'seed-1',
	support: true,
	exploration: false,
	applied: false,
	degradation_reason: null
};

describe('personal contextual-bandit contracts', () => {
	it('parses shadow metadata as preview-only baseline service', () => {
		expect(parseAttentionBanditDecision(shadowDecision, decisionItem)).toMatchObject({
			mode: 'shadow',
			proposed_position: 3,
			served_position: 1,
			served_propensity: 1,
			applied: false
		});
	});

	it('accepts an active canary only when the server explicitly applied it', () => {
		const canaryItem: AttentionDecisionItem = {
			...decisionItem,
			routing_mode: 'canary',
			served_rank: 2,
			canary_assigned: true,
			route_reason: 'baseline_route_retained'
		};
		const parsed = parseAttentionBanditDecision(
			{
				...shadowDecision,
				mode: 'canary',
				proposed_position: 2,
				served_position: 2,
				served_propensity: 0.24,
				exploration: true,
				applied: true
			},
			canaryItem
		);
		expect(parsed).toMatchObject({ mode: 'canary', exploration: true, applied: true });
		expect(parseAttentionBanditDecision({ ...shadowDecision, applied: true }, decisionItem))
			.toBeNull();
	});

	it('fails closed on malformed snapshots, positions, and non-applied propensity', () => {
		expect(parseAttentionBanditDecision(undefined, decisionItem)).toBeNull();
		expect(
			parseAttentionBanditDecision(
				{ ...shadowDecision, policy_model_version: null },
				decisionItem
			)
		).toBeNull();
		expect(
			parseAttentionBanditDecision({ ...shadowDecision, served_position: 2 }, decisionItem)
		).toBeNull();
		expect(
			parseAttentionBanditDecision({ ...shadowDecision, served_propensity: 0.5 }, decisionItem)
		).toBeNull();
	});

	it('parses bounded first-page health and rejects an unbounded claim', () => {
		const root = {
			bandit_health: {
				mode: 'shadow',
				policy_snapshot_id: 'policy-1',
				posterior_version: 12,
				posterior_update_count: 7,
				propensity_coverage: 0.96,
				exploration_rate: 0.04,
				support_ok: true,
				first_page_bounded: true,
				degradation_reason: null
			}
		};
		expect(parseAttentionBanditHealth(root)).toMatchObject({
			posterior_update_count: 7,
			first_page_bounded: true
		});
		expect(
			parseAttentionBanditHealth({
				bandit_health: { ...root.bandit_health, first_page_bounded: false }
			})
		).toBeNull();
	});

	it('parses updated and neutral receipts without turning neutral into learning', () => {
		const updated = parseAttentionPosteriorUpdate({
			status: 'updated',
			policy_snapshot_id: 'policy-1',
			posterior_version_before: 12,
			posterior_version_after: 13,
			attribution_quality: 'verified_impression',
			degradation_reason: null,
			uncertainty_before: 0.31,
			uncertainty_after: 0.27,
			affected_rank_before: 7,
			affected_rank_after: 4,
			affected_rank_delta: 3,
			rescore_scheduled: true
		});
		expect(posteriorUpdateMessage(updated)).toContain('version 13');
		expect(posteriorUpdateMessage(updated)).toContain('uncertainty 0.310 → 0.270');

		const neutral = {
			status: 'neutral',
			policy_snapshot_id: 'policy-1',
			posterior_version_before: 13,
			posterior_version_after: 13,
			attribution_quality: 'decision_only',
			degradation_reason: null,
			uncertainty_before: 0.27,
			uncertainty_after: 0.27,
			affected_rank_before: null,
			affected_rank_after: null,
			affected_rank_delta: null,
			rescore_scheduled: false
		};
		expect(parseAttentionPosteriorUpdate(neutral)?.status).toBe('neutral');
		expect(parseAttentionPosteriorUpdate({ ...neutral, rescore_scheduled: true })).toBeNull();

		const degraded = parseAttentionPosteriorUpdate({
			...neutral,
			status: 'degraded',
			attribution_quality: 'mismatch',
			degradation_reason: 'source_revision_mismatch'
		});
		expect(posteriorUpdateMessage(degraded)).toContain(
			'Personal posterior degraded · source revision mismatch; outcome still recorded.'
		);
		const duplicate = parseAttentionPosteriorUpdate({ ...neutral, status: 'duplicate' });
		expect(posteriorUpdateMessage(duplicate)).toBe(
			'Personal posterior already reflected this outcome.'
		);
	});
});
