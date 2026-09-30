import { describe, expect, it } from 'vitest';

import {
	groupingAffordanceLabel,
	groupingMayCollapse,
	groupingTotalsReconcile,
	parseAttentionGroupingMetadata,
	parseAttentionGroupingPage,
	parseAttentionPairCorrectionReceipt
} from './attentionGrouping';

const health = {
	candidate_total: 148,
	member_total: 148,
	cluster_total: 11,
	representative_total: 11,
	collapsed_member_total: 137,
	scored_pair_total: 210,
	cannot_link_total: 3,
	fallback_ungrouped_total: 2,
	totals_reconcile: true
};

describe('attention grouping contracts', () => {
	it('parses reconciled enforced grouping and permits collapse', () => {
		const page = parseAttentionGroupingPage({
			grouping_mode: 'enforced',
			grouping_snapshot_id: 'group-snapshot-1',
			grouping_generation: 7,
			grouping_scope: 'eligible_universe',
			grouping_health: health
		});
		expect(page?.health.collapsed_member_total).toBe(137);
		expect(groupingMayCollapse(page)).toBe(true);
	});

	it('keeps shadow and unreconciled grouping diagnostic-only', () => {
		const shadow = parseAttentionGroupingPage({
			grouping_mode: 'shadow',
			grouping_snapshot_id: 'group-snapshot-1',
			grouping_generation: 7,
			grouping_scope: 'eligible_universe',
			grouping_health: health
		});
		const unreconciled = parseAttentionGroupingPage({
			grouping_mode: 'enforced',
			grouping_snapshot_id: 'group-snapshot-1',
			grouping_generation: 7,
			grouping_scope: 'eligible_universe',
			grouping_health: { ...health, totals_reconcile: false }
		});
		expect(groupingMayCollapse(shadow)).toBe(false);
		expect(groupingMayCollapse(unreconciled)).toBe(false);
		expect(
			groupingTotalsReconcile({
				...unreconciled!,
				health: { ...health, member_total: 147, totals_reconcile: true }
			})
		).toBe(false);
	});

	it('parses an exceeded pair budget as reconciled singleton fallback diagnostics', () => {
		const page = parseAttentionGroupingPage({
			grouping_mode: 'disabled',
			grouping_snapshot_id: null,
			grouping_generation: 7,
			grouping_scope: 'eligible_universe',
			grouping_health: {
				candidate_total: 12,
				member_total: 12,
				cluster_total: 12,
				representative_total: 12,
				collapsed_member_total: 0,
				scored_pair_total: 0,
				cannot_link_total: 0,
				fallback_ungrouped_total: 12,
				totals_reconcile: true,
				pair_evaluation_budget: 50,
				required_pair_evaluations: 66,
				budget_exceeded: true
			}
		});

		expect(page?.health).toMatchObject({
			pair_evaluation_budget: 50,
			required_pair_evaluations: 66,
			budget_exceeded: true
		});
		expect(groupingTotalsReconcile(page)).toBe(true);
		expect(groupingMayCollapse(page)).toBe(false);
	});

	it('parses member evidence and distinguishes preview affordances', () => {
		const metadata = parseAttentionGroupingMetadata({
			cluster_id: 'cluster-1',
			representative_id: 'candidate-1',
			is_representative: true,
			member_count: 12,
			related_count: 11,
			model_version: 'pair-gbt-v1',
			snapshot_id: 'group-snapshot-1',
			merge_probability: 0.98
		});
		expect(metadata?.merge_probability).toBe(0.98);
		expect(groupingAffordanceLabel(metadata!, null)).toBeNull();
		expect(
			groupingAffordanceLabel(metadata!, {
				mode: 'shadow',
				snapshot_id: 'group-snapshot-1',
				generation: 7,
				scope: 'eligible_universe',
				health
			})
		).toBe('Grouping preview · 11 related updates');
	});

	it('rejects malformed totals and parses an exact recompute receipt', () => {
		expect(
			parseAttentionGroupingPage({
				grouping_mode: 'enforced',
				grouping_snapshot_id: 'snapshot',
				grouping_generation: 1,
				grouping_scope: 'eligible_universe',
				grouping_health: { ...health, member_total: 3.5 }
			})
		).toBeNull();
		expect(
			parseAttentionPairCorrectionReceipt({
				pair_label_id: 'pair-1',
				inserted: true,
				label: 'not_duplicate',
				canonical_left_id: 'candidate-1',
				canonical_right_id: 'candidate-2',
				affected_cluster_ids: ['cluster-1', 'cluster-2'],
				grouping_generation: 8,
				recomputed: true
			})
		).toMatchObject({ label: 'not_duplicate', grouping_generation: 8 });
	});
});
