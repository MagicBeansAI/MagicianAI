import { describe, expect, it } from 'vitest';

import {
	actionabilityPresentation,
	feedbackReceiptMessage,
	followUpRankDeltaLabel,
	parseAttentionActionabilityCard,
	parseAttentionActionabilityPage,
	parseAttentionFeedbackReceipt,
	parseChannelFollowUpLearningHealth,
	parseChannelFollowUpLearningRank
} from './channelFollowUpLearning';

describe('channel Follow-up learning contracts', () => {
	it('parses a typed outcome receipt and explains immediate re-scoring', () => {
		const receipt = parseAttentionFeedbackReceipt({
			outcome_id: 'outcome-1',
			outcome: 'irrelevant',
			surface: 'follow_up',
			feedback_recorded: true,
			affected_candidates: 37,
			rescore_status: 'completed',
			embedding_contract: 'all-minilm-l6-v2',
			diagnostic_href: '/attention?outcome=outcome-1'
		});

		expect(receipt?.outcome).toBe('irrelevant');
		expect(feedbackReceiptMessage('Dismissed — Spam / junk', receipt)).toBe(
			'Dismissed — Spam / junk. Feedback recorded; 37 related candidates re-scored.'
		);
	});

	it('rejects malformed receipts instead of presenting invented learning evidence', () => {
		expect(
			parseAttentionFeedbackReceipt({
				outcome_id: 'outcome-1',
				outcome: 'irrelevant',
				surface: 'follow_up',
				feedback_recorded: true,
				affected_candidates: -1,
				rescore_status: 'completed'
			})
		).toBeNull();
	});

	it('accepts legacy receipts but strictly preserves a new pending rank job boundary', () => {
		const base = {
			outcome_id: 'outcome-rank', outcome: 'irrelevant', surface: 'follow_up',
			feedback_recorded: true, affected_candidates: 2, rescore_status: 'completed',
			posterior_update: null
		};
		expect(parseAttentionFeedbackReceipt(base)?.rank_recompute).toBeUndefined();
		const receipt = parseAttentionFeedbackReceipt({
			...base,
			rank_recompute: {
				enqueue_status: 'enqueued', job_id: 'job-1', job_status: 'pending',
				status_href: '/api/magician/v2/channel-assist/attention-learning/rank-recompute/jobs/job-1',
				affected_rank_before: 8, affected_rank_after: null, affected_rank_delta: null,
				result_semantics: 'current_universe_diagnostic', reason: null
			}
		});
		expect(receipt?.rank_recompute?.job_status).toBe('pending');
		expect(feedbackReceiptMessage('Dismissed', receipt)).toContain('Rank recompute queued');
		expect(parseAttentionFeedbackReceipt({
			...base,
			rank_recompute: { ...receipt?.rank_recompute, affected_rank_after: 4 }
		})).toBeNull();
	});

	it('adds a quiet posterior update receipt with uncertainty and affected ranks', () => {
		const receipt = parseAttentionFeedbackReceipt({
			outcome_id: 'outcome-2',
			outcome: 'useful',
			surface: 'worth_a_look',
			feedback_recorded: true,
			affected_candidates: 4,
			rescore_status: 'completed',
			posterior_update: {
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
			}
		});

		expect(receipt?.posterior_update).toMatchObject({
			status: 'updated',
			posterior_version_after: 13,
			affected_rank_after: 4
		});
		expect(feedbackReceiptMessage('Marked useful', receipt)).toContain(
			'Personal posterior updated · version 13 · uncertainty 0.310 → 0.270 · affected rank 7 → 4.'
		);
	});

	it('parses evidence-preserving health and rank metadata', () => {
		expect(
			parseChannelFollowUpLearningHealth({
				total_active: 1014,
				source_family_counts: { promise: 961, comms_ingest: 53 },
				embedded_candidates: 810,
				embedding_coverage: 0.7988,
				learned_rank_changes: 64
			})
		).toMatchObject({ total_active: 1014, embedded_candidates: 810 });

		const rank = parseChannelFollowUpLearningRank({
			baseline_rank: 4,
			learned_rank: 17,
			rank_delta: -13,
			learning_score: 0.12
		});
		expect(followUpRankDeltaLabel(rank, true)).toBe(
			'Learned rank 17 · down 13 from baseline'
		);
		expect(followUpRankDeltaLabel(rank, false)).toBe(
			'Rank preview 17 · down 13 from baseline'
		);
	});

	it('parses calibrated actionability contracts without confusing shadow with enforcement', () => {
		expect(
			parseAttentionActionabilityPage({
				actionability_mode: 'shadow',
				actionability_snapshot_id: 'snapshot-42',
				semantic_extraction_coverage: 0.82,
				actionability_scored_count: 82,
				actionability_fallback_count: 18
			})
		).toEqual({
			mode: 'shadow',
			snapshot_id: 'snapshot-42',
			semantic_extraction_coverage: 0.82,
			scored_count: 82,
			fallback_count: 18
		});

		const shadow = parseAttentionActionabilityCard({
			actionability_probability: 0.84,
			actionability_explanation: {
				code: 'direct_owner_request',
				label: 'Direct request to you'
			},
			actionability_model_version: 'actionability-gbt-v1',
			actionability_snapshot_id: 'snapshot-42',
			semantic_feature_status: 'succeeded',
			actionability_score_status: 'scored',
			actionability_mode: 'shadow'
		});
		const enforced = shadow ? { ...shadow, mode: 'enforced' as const } : null;
		expect(actionabilityPresentation(shadow)).toMatchObject({
			label: 'Actionability preview 84% · Direct request to you',
			active: false
		});
		expect(actionabilityPresentation(enforced)?.active).toBe(true);
	});

	it('rejects malformed actionability and explains deterministic Slice 1 fallback', () => {
		expect(
			parseAttentionActionabilityPage({
				actionability_mode: 'shadow',
				actionability_snapshot_id: null,
				semantic_extraction_coverage: 1.2,
				actionability_scored_count: 8.5,
				actionability_fallback_count: 1
			})
		).toBeNull();
		expect(
			parseAttentionActionabilityCard({
				actionability_probability: 2,
				actionability_explanation: null,
				actionability_model_version: null,
				actionability_snapshot_id: null,
				semantic_feature_status: 'succeeded',
				actionability_score_status: 'scored',
				actionability_mode: 'shadow'
			})
		).toBeNull();

		expect(
			actionabilityPresentation({
				probability: null,
				explanation: null,
				model_version: null,
				snapshot_id: null,
				semantic_feature_status: 'missing',
				score_status: 'fallback',
				mode: 'enforced'
			})?.label
		).toBe('Actionability unavailable · semantic features pending · Slice 1 fallback');
		expect(
			actionabilityPresentation({
				probability: null,
				explanation: null,
				model_version: null,
				snapshot_id: null,
				semantic_feature_status: 'missing',
				score_status: 'disabled',
				mode: 'disabled'
			})
		).toBeNull();
	});
});
