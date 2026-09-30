import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import AttentionRoutingDiagnostic from './AttentionRoutingDiagnostic.svelte';
import type { AttentionDecisionItem, AttentionRoutingPage } from './attentionRouting';

const item: AttentionDecisionItem = {
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

const page: AttentionRoutingPage = {
	decision: {
		decision_id: 'decision-1',
		decided_at: 1,
		surface: 'follow_up',
		routing_mode: 'shadow',
		routing_snapshot_id: 'routing-snapshot-1',
		eligible_item_count: 12,
		selected_item_count: 2,
		returned_item_count: 2,
		complete_universe_recorded: true,
		degradation_reason: null
	},
	impression_policy: { min_visible_ms: 1_000, visibility_rule_version: 'visible-v1' },
	health: {
		routing_snapshot_valid: true,
		evaluated_count: 12,
		learned_route_count: 3,
		applied_route_count: 0,
		baseline_retained_count: 12,
		impression_eligible_count: 2,
		decision_item_coverage: 1,
		all_candidates_path: '/all-candidates/decision-1'
	}
};

describe('AttentionRoutingDiagnostic', () => {
	it('labels a shadow disagreement as preview-only and exposes All candidates', () => {
		const { body } = render(AttentionRoutingDiagnostic, { props: { item, page } });
		expect(body).toContain('Route preview');
		expect(body).toContain('Follow-up → Worth a look');
		expect(body).toContain('Served Follow-up');
		expect(body).toContain('All candidates');
		expect(body).not.toContain('Canary route active');
	});

	it('calls a canary route active only when the decision item says it was applied', () => {
		const applied: AttentionDecisionItem = {
			...item,
			routing_mode: 'canary',
			served_route: 'worth_a_look',
			route_reason: 'learned_route_applied',
			route_applied: true,
			canary_assigned: true
		};
		const { body } = render(AttentionRoutingDiagnostic, { props: { item: applied, page } });
		expect(body).toContain('Canary route active');
		expect(body).toContain('Served Worth a look');
	});
});
