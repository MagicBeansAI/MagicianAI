import { describe, expect, it } from 'vitest';

import {
	parseAttentionDecisionItem,
	parseAttentionImpressionReceipt,
	parseAttentionRoutingPage
} from './attentionRouting';

const shadowItem = {
	decision_id: 'decision-1',
	candidate_id: 'candidate-1',
	source_revision: 'revision-1',
	baseline_route: 'follow_up',
	learned_route: 'worth_a_look',
	served_route: 'follow_up',
	routing_mode: 'shadow',
	routing_snapshot_id: 'routing-snapshot-1',
	routing_model_version: 'lane-router-v1',
	learned_route_confidence: 0.94,
	utility_margin: 0.31,
	route_reason: 'shadow_only',
	served_rank: 1,
	selected: true,
	route_applied: false,
	canary_assigned: false
};

describe('attention routing contracts', () => {
	it('parses a complete shadow decision while preserving the served baseline', () => {
		const item = parseAttentionDecisionItem(shadowItem, {
			decision_id: 'decision-1',
			candidate_id: 'candidate-1',
			source_revision: 'revision-1'
		});
		expect(item).toMatchObject({
			baseline_route: 'follow_up',
			learned_route: 'worth_a_look',
			served_route: 'follow_up',
			routing_mode: 'shadow',
			route_applied: false
		});
	});

	it('accepts only an explicitly applied canary learned route', () => {
		const applied = parseAttentionDecisionItem({
			...shadowItem,
			served_route: 'worth_a_look',
			routing_mode: 'canary',
			route_reason: 'learned_route_applied',
			route_applied: true,
			canary_assigned: true
		});
		const invalidShadow = parseAttentionDecisionItem({
			...shadowItem,
			served_route: 'worth_a_look',
			route_applied: true,
			canary_assigned: true
		});
		expect(applied?.route_applied).toBe(true);
		expect(invalidShadow).toBeNull();
	});

	it('accepts the backend snapshot-invalid degradation vocabulary and rejects selected rank zero', () => {
		expect(parseAttentionDecisionItem({ ...shadowItem, route_reason: 'snapshot_invalid' }))
			.not.toBeNull();
		expect(parseAttentionDecisionItem({ ...shadowItem, served_rank: 0 })).toBeNull();
	});

	it('parses complete-universe and historical impression health', () => {
		const page = parseAttentionRoutingPage({
			decision: {
				decision_id: 'decision-1',
				decided_at: 1_725_000_000_000,
				surface: 'follow_up',
				routing_mode: 'shadow',
				routing_snapshot_id: 'routing-snapshot-1',
				eligible_item_count: 120,
				selected_item_count: 12,
				returned_item_count: 12,
				complete_universe_recorded: true,
				complete_cross_lane_universe: true,
				degradation_reason: null
			},
			impression_policy: {
				min_visible_ms: 1_000,
				visibility_rule_version: 'visible-50-dwell-v1'
			},
			routing_health: {
				routing_snapshot_valid: true,
				evaluated_count: 120,
				learned_route_count: 20,
				applied_route_count: 0,
				baseline_retained_count: 120,
				impression_eligible_count: 12,
				decision_item_coverage: 1,
				all_candidates_path: '/api/magician/v2/channel-assist/attention-learning/decisions/decision-1',
				verified_impression_coverage: 0.99,
				impression_dedupe_count: 4
			}
		});
		expect(page?.decision.complete_universe_recorded).toBe(true);
		expect(page?.decision.complete_cross_lane_universe).toBe(true);
		expect(page?.impression_policy.min_visible_ms).toBe(1_000);
		expect(page?.health).toMatchObject({
			evaluated_count: 120,
			verified_impression_coverage: 0.99,
			impression_dedupe_count: 4
		});
	});

	it('parses idempotent verified-impression debug receipts', () => {
		expect(
			parseAttentionImpressionReceipt({
				impression_id: 'impression-1',
				event_id: 'event-1',
				decision_id: 'decision-1',
				candidate_id: 'candidate-1',
				source_revision: 'revision-1',
				surface: 'follow_up',
				accumulated_visible_ms: 1_025,
				min_visible_ms: 1_000,
				visibility_rule_version: 'visible-50-dwell-v1',
				verified: true,
				deduplicated: true
			})
		).toMatchObject({ verified: true, deduplicated: true, accumulated_visible_ms: 1_025 });
	});
});
