import { describe, expect, it } from 'vitest';

import {
	ATTENTION_RANK_RECOMPUTE_BASE,
	parseAttentionRankRecomputeHealth,
	parseAttentionRankRecomputeJobResponse,
	parseAttentionRankRecomputeReference,
	type AttentionRankRecomputeBinding
} from './attentionRankRecompute';

const binding: AttentionRankRecomputeBinding = {
	job_id: 'job-1',
	outcome_id: 'outcome-1',
	origin_surface: 'follow_up',
	raw_candidate_id: 'ann-1',
	source_revision: 'revision-1',
	outcome: 'irrelevant',
	decision_id: 'decision-1',
	delivery_id: 'delivery-1',
	impression_id: 'impression-1',
	affected_rank_before: 7,
	enqueue_policy_snapshot_id: 'policy-1',
	enqueue_posterior_version: 13
};

function job(
	status: 'pending' | 'succeeded' | 'stale' = 'pending',
	posteriorVersion: number | null = 13,
	enqueueSnapshot: string | null = 'policy-1',
	resultSnapshot: string | null = 'policy-1',
	semantics: string = 'current_universe_diagnostic'
) {
	const completed = status === 'pending' ? null : 2_000;
	return {
		schema_version: 1,
		job: {
			job_id: 'job-1', outcome_id: 'outcome-1', status,
			origin_surface: 'follow_up', canonical_candidate_id: 'follow_up:ann-1',
			raw_candidate_id: 'ann-1', source_revision: 'revision-1', outcome: 'irrelevant',
			decision_id: 'decision-1', delivery_id: 'delivery-1', impression_id: 'impression-1',
			affected_rank_before: 7, enqueue_policy_snapshot_id: enqueueSnapshot,
			enqueue_posterior_version: posteriorVersion, attempts: status === 'pending' ? 0 : 1,
			next_retry_at: null, lease_expires_at: null, created_at: 1_000,
			updated_at: completed ?? 1_000, completed_at: completed,
			reason: status === 'stale' ? 'source_revision_changed' : null,
			result: status === 'succeeded' ? {
				semantics, affected_rank_after: 4,
				affected_rank_delta: -3, current_source_revision: 'revision-1',
				universe_digest: 'digest-2',
				recompute_generation: { follow_up: 9, worth_a_look: 4 },
				policy_snapshot_id: resultSnapshot, posterior_version: posteriorVersion, completed_at: 2_000
			} : null
		}
	};
}

describe('attention rank recompute contracts', () => {
	it('accepts only an exact pending job reference with no manufactured after-rank', () => {
		const reference = parseAttentionRankRecomputeReference({
			enqueue_status: 'enqueued', job_id: 'job-1', job_status: 'pending',
			status_href: `${ATTENTION_RANK_RECOMPUTE_BASE}/jobs/job-1`,
			affected_rank_before: 7, affected_rank_after: null, affected_rank_delta: null,
			result_semantics: 'current_universe_diagnostic', reason: null
		});
		expect(reference?.affected_rank_after).toBeNull();
		expect(parseAttentionRankRecomputeReference({ ...reference, affected_rank_after: 4 })).toBeNull();
	});

	it('accepts a served-universe result and still rejects an unknown provenance', () => {
		// A job resolved against the projection its decision was served from
		// reports a historical universe. Rejecting that label would null every
		// attributed result while the page still rendered -- invisibly.
		const served = parseAttentionRankRecomputeJobResponse(
			job('succeeded', 13, 'policy-1', 'policy-1', 'served_universe_diagnostic'),
			binding
		);
		expect(served?.result?.semantics).toBe('served_universe_diagnostic');

		const current = parseAttentionRankRecomputeJobResponse(job('succeeded'), binding);
		expect(current?.result?.semantics).toBe('current_universe_diagnostic');

		expect(
			parseAttentionRankRecomputeJobResponse(
				job('succeeded', 13, 'policy-1', 'policy-1', 'future_universe_diagnostic'),
				binding
			)
		).toBeNull();

		const reference = (semantics: string) =>
			parseAttentionRankRecomputeReference({
				enqueue_status: 'enqueued', job_id: 'job-1', job_status: 'pending',
				status_href: `${ATTENTION_RANK_RECOMPUTE_BASE}/jobs/job-1`,
				affected_rank_before: 7, affected_rank_after: null, affected_rank_delta: null,
				result_semantics: semantics, reason: null
			});
		expect(reference('served_universe_diagnostic')?.result_semantics).toBe(
			'served_universe_diagnostic'
		);
		expect(reference('not_a_semantics')).toBeNull();
	});

	it('binds pending and succeeded jobs to outcome, candidate, revision, and delivery attribution', () => {
		expect(parseAttentionRankRecomputeJobResponse(job('pending'), binding)?.status).toBe('pending');
		const succeeded = parseAttentionRankRecomputeJobResponse(job('succeeded'), binding);
		expect(succeeded?.result).toMatchObject({
			affected_rank_after: 4,
			universe_digest: 'digest-2',
			recompute_generation: { follow_up: 9, worth_a_look: 4 },
			policy_snapshot_id: 'policy-1',
			posterior_version: 13
		});
		expect(parseAttentionRankRecomputeJobResponse(job('succeeded'), {
			...binding, delivery_id: 'other-delivery'
		})).toBeNull();
	});

	it('accepts a reconstructed decision id when the client sent none', () => {
		const unbound = { ...binding, decision_id: null, delivery_id: null, impression_id: null };
		const pending = job('pending');
		expect(parseAttentionRankRecomputeJobResponse(pending, unbound)?.decision_id).toBe(
			'decision-1'
		);
	});

	it('keeps stale terminal status accepted without inventing a result', () => {
		const stale = parseAttentionRankRecomputeJobResponse(job('stale'), binding);
		expect(stale).toMatchObject({ status: 'stale', reason: 'source_revision_changed', result: null });
		const unlinkable = job('stale');
		unlinkable.job.reason = 'no_served_decision';
		expect(parseAttentionRankRecomputeJobResponse(unlinkable, binding)?.reason)
			.toBe('no_served_decision');
	});

	it('accepts nullable posterior bindings and rejects a scalar generation', () => {
		const baselineBinding = {
			...binding,
			enqueue_policy_snapshot_id: null,
			enqueue_posterior_version: null
		};
		expect(parseAttentionRankRecomputeJobResponse(
			job('succeeded', null, null, null), baselineBinding
		)?.result?.posterior_version).toBeNull();
		const succeeded = job('succeeded');
		expect(parseAttentionRankRecomputeJobResponse({
			...succeeded,
			job: {
				...succeeded.job,
				result: { ...succeeded.job.result, recompute_generation: 9 }
			}
		}, binding)).toBeNull();
	});

	it('ignores additive envelope and job diagnostics while preserving the result generation', () => {
		const succeeded = job('succeeded');
		const parsed = parseAttentionRankRecomputeJobResponse({
			...succeeded,
			trace_id: 'trace-1',
			job: { ...succeeded.job, recompute_generation: 9 }
		}, binding);
		expect(parsed?.result?.recompute_generation).toEqual({ follow_up: 9, worth_a_look: 4 });
	});

	it('strictly parses enabled and configuration-disabled queue health', () => {
		const disabled = {
			schema_version: 1, enabled: false, paused: true,
			pause_reason: 'rank_recompute_disabled',
			queue: { pending: 2, in_flight: 0, retry: 1, succeeded: 4, stale: 1, dead: 0,
				next_retry_at: 2_000, oldest_pending_at: 1_000 },
			worker: { batch_size: 8, concurrency: 2, interval_secs: 5, max_retries: 4,
				lease_secs: 30, retention_days: 14 }
		};
		expect(parseAttentionRankRecomputeHealth(disabled)?.pause_reason)
			.toBe('rank_recompute_disabled');
		expect(parseAttentionRankRecomputeHealth({ ...disabled, pause_reason: 'disabled' })).toBeNull();
	});
});
