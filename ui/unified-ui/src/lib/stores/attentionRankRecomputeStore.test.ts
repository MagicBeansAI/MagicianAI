import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ fetchJob: vi.fn(), fetchHealth: vi.fn() }));
vi.mock('$lib/attention/attentionRankRecompute', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/attention/attentionRankRecompute')>();
	return {
		...actual,
		fetchAttentionRankRecomputeJob: mocks.fetchJob,
		fetchAttentionRankRecomputeHealth: mocks.fetchHealth
	};
});

import type { AttentionFeedbackReceipt } from '$lib/channel/channelFollowUpLearning';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { attentionRankRecomputeStore } from './attentionRankRecomputeStore';

const receipt: AttentionFeedbackReceipt = {
	outcome_id: 'outcome-1', outcome: 'irrelevant', surface: 'follow_up',
	feedback_recorded: true, affected_candidates: 1, rescore_status: 'completed',
	embedding_contract: null, diagnostic_href: null, posterior_update: null,
	rank_recompute: {
		enqueue_status: 'enqueued', job_id: 'job-1', job_status: 'pending',
		status_href: '/api/magician/v2/channel-assist/attention-learning/rank-recompute/jobs/job-1',
		affected_rank_before: 7, affected_rank_after: null, affected_rank_delta: null,
		result_semantics: 'current_universe_diagnostic', reason: null
	}
};

function succeededJob() {
	return {
		job_id: 'job-1', outcome_id: 'outcome-1', status: 'succeeded' as const,
		origin_surface: 'follow_up' as const, canonical_candidate_id: 'follow_up:ann-1',
		raw_candidate_id: 'ann-1', source_revision: 'revision-1', outcome: 'irrelevant' as const,
		decision_id: 'decision-1', delivery_id: 'delivery-1', impression_id: 'impression-1',
		affected_rank_before: 7, enqueue_policy_snapshot_id: null, enqueue_posterior_version: 0,
		attempts: 1, next_retry_at: null, lease_expires_at: null, created_at: 1,
		updated_at: 2, completed_at: 2, reason: null,
		result: {
			semantics: 'current_universe_diagnostic' as const, affected_rank_after: 4,
			affected_rank_delta: -3, current_source_revision: 'revision-1', universe_digest: 'digest',
			recompute_generation: { follow_up: 2, worth_a_look: 3 },
			policy_snapshot_id: null, posterior_version: null, completed_at: 2
		}
	};
}

beforeEach(() => {
	vi.useFakeTimers();
	mocks.fetchJob.mockReset();
	mocks.fetchHealth.mockReset().mockResolvedValue(null);
	scopeIdentityStore.reset();
	attentionRankRecomputeStore.clear();
	attentionRankRecomputeStore.setScope(['anonymous', 'default'].join('\u0000'));
	attentionRankRecomputeStore.mount('test-monitor');
});

afterEach(() => {
	attentionRankRecomputeStore.clear();
	scopeIdentityStore.reset();
	vi.useRealTimers();
});

describe('attentionRankRecomputeStore', () => {
	it('polls a recent accepted outcome and updates only its diagnostic receipt state', async () => {
		mocks.fetchJob.mockResolvedValue({ ok: true, job: succeededJob() });
		attentionRankRecomputeStore.track(receipt, {
			raw_candidate_id: 'ann-1', source_revision: 'revision-1',
			attribution: { decision_id: 'decision-1', candidate_id: 'ann-1',
				source_revision: 'revision-1', delivery_id: 'delivery-1', impression_id: 'impression-1' }
		});

		await vi.advanceTimersByTimeAsync(1_000);

		const tracked = get(attentionRankRecomputeStore).jobs[0];
		expect(tracked?.status).toBe('succeeded');
		expect(tracked?.job?.result?.affected_rank_after).toBe(4);
		expect(mocks.fetchJob).toHaveBeenCalledTimes(1);
	});

	it('cancels pending polling on scope change and never replays feedback', async () => {
		attentionRankRecomputeStore.track(receipt, { raw_candidate_id: 'ann-1' });
		scopeIdentityStore.observe('other', 'workspace');
		attentionRankRecomputeStore.setScope(['other', 'workspace'].join('\u0000'));
		await vi.advanceTimersByTimeAsync(20_000);

		expect(mocks.fetchJob).not.toHaveBeenCalled();
		expect(get(attentionRankRecomputeStore).jobs).toEqual([]);
	});

	it('preserves an accepted outcome when durable enqueue fails without polling a fake job', async () => {
		attentionRankRecomputeStore.track({
			...receipt,
			rank_recompute: {
				enqueue_status: 'failed', job_id: null, job_status: null, status_href: null,
				affected_rank_before: 7, affected_rank_after: null, affected_rank_delta: null,
				result_semantics: 'current_universe_diagnostic', reason: 'enqueue_failed'
			}
		}, { raw_candidate_id: 'ann-1' });

		await vi.advanceTimersByTimeAsync(20_000);

		expect(get(attentionRankRecomputeStore).jobs[0]?.status).toBe('enqueue_failed');
		expect(mocks.fetchJob).not.toHaveBeenCalled();
	});
});
