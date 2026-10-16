import type { CanonicalAttentionOutcome, AttentionSurface } from '$lib/channel/channelFollowUpLearning';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import type { CanonicalAttentionProjectionScope } from './canonicalAttentionProjection';

export const ATTENTION_RANK_RECOMPUTE_BASE =
	'/api/magician/v2/channel-assist/attention-learning/rank-recompute';

export type AttentionRankRecomputeJobStatus =
	| 'pending'
	| 'in_flight'
	| 'retry'
	| 'succeeded'
	| 'stale'
	| 'dead';
export type AttentionRankRecomputeDisplayStatus =
	| AttentionRankRecomputeJobStatus
	| 'enqueue_failed'
	| 'disabled';
export type AttentionRankRecomputeStaleReason =
	| 'candidate_inactive'
	| 'source_revision_changed'
	| 'snapshot_incompatible'
	| 'universe_changed_during_commit'
	| 'generation_changed_during_commit'
	| 'no_served_decision'
	| 'served_projection_unreadable';
export type AttentionRankRecomputeOperationalReason =
	| 'canonical_projection_failed'
	| 'posterior_read_failed'
	| 'rank_recompute_failed'
	| 'lease_expired'
	| 'max_retries_exhausted';

/** Which universe an after-rank is bound to: the one the owner was served, or
 * a freshly computed one. A served result carries a historical
 * `universe_digest`, so the two must not be conflated. */
export type AttentionRankRecomputeSemantics =
	| 'current_universe_diagnostic'
	| 'served_universe_diagnostic';

const SEMANTICS = new Set<AttentionRankRecomputeSemantics>([
	'current_universe_diagnostic',
	'served_universe_diagnostic'
]);

export interface AttentionRankRecomputeReference {
	enqueue_status: 'enqueued' | 'failed';
	job_id: string | null;
	job_status: AttentionRankRecomputeJobStatus | null;
	status_href: string | null;
	affected_rank_before: number | null;
	affected_rank_after: null;
	affected_rank_delta: null;
	result_semantics: AttentionRankRecomputeSemantics;
	reason: 'enqueue_failed' | null;
}

export interface AttentionRankRecomputeResult {
	semantics: AttentionRankRecomputeSemantics;
	affected_rank_after: number;
	affected_rank_delta: number | null;
	current_source_revision: string | null;
	universe_digest: string;
	recompute_generation: {
		follow_up: number;
		worth_a_look: number;
	};
	policy_snapshot_id: string | null;
	posterior_version: number | null;
	completed_at: number;
}

export interface AttentionRankRecomputeJob {
	job_id: string;
	outcome_id: string;
	status: AttentionRankRecomputeJobStatus;
	origin_surface: AttentionSurface;
	canonical_candidate_id: string;
	raw_candidate_id: string;
	source_revision: string | null;
	outcome: CanonicalAttentionOutcome;
	decision_id: string | null;
	delivery_id: string | null;
	impression_id: string | null;
	affected_rank_before: number | null;
	enqueue_policy_snapshot_id: string | null;
	enqueue_posterior_version: number | null;
	attempts: number;
	next_retry_at: number | null;
	lease_expires_at: number | null;
	created_at: number;
	updated_at: number;
	completed_at: number | null;
	reason: AttentionRankRecomputeStaleReason | AttentionRankRecomputeOperationalReason | null;
	result: AttentionRankRecomputeResult | null;
}

export interface AttentionRankRecomputeBinding {
	job_id: string;
	outcome_id: string;
	origin_surface: AttentionSurface;
	raw_candidate_id: string;
	source_revision?: string | null;
	outcome: CanonicalAttentionOutcome;
	decision_id: string | null;
	delivery_id: string | null;
	impression_id: string | null;
	affected_rank_before: number | null;
	enqueue_policy_snapshot_id?: string | null;
	enqueue_posterior_version?: number | null;
}

export interface AttentionRankRecomputeHealth {
	schema_version: 1;
	enabled: boolean;
	paused: boolean;
	pause_reason: 'rank_recompute_disabled' | null;
	queue: {
		pending: number;
		in_flight: number;
		retry: number;
		succeeded: number;
		stale: number;
		dead: number;
		next_retry_at: number | null;
		oldest_pending_at: number | null;
	};
	worker: {
		batch_size: number;
		concurrency: number;
		interval_secs: number;
		max_retries: number;
		lease_secs: number;
		retention_days: number;
	};
}

export type AttentionRankRecomputeFetchResult =
	| { ok: true; job: AttentionRankRecomputeJob }
	| { ok: false; error: string; retryable: boolean };

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? value as Record<string, unknown>
		: null;
}

function exact(value: Record<string, unknown>, keys: readonly string[]): boolean {
	return Object.keys(value).length === keys.length && keys.every((key) =>
		Object.prototype.hasOwnProperty.call(value, key));
}

/** Required keys must be present; extra diagnostic fields are ignored. */
function hasRequiredKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
	return keys.every((key) => Object.prototype.hasOwnProperty.call(value, key));
}

/** The server may reconstruct a missing ledger binding after enqueue. */
function compatibleOptionalId(expected: string | null, actual: string | null): boolean {
	return expected === null || expected === actual;
}

function canonicalMatches(surface: string, canonicalId: string, rawId: string): boolean {
	return canonicalId === `${surface}:${rawId}` || canonicalId === rawId;
}

function bounded(value: unknown, max = 500): string | null {
	return typeof value === 'string' && value.trim().length > 0 && [...value].length <= max &&
		![...value].some((character) => /\p{Cc}/u.test(character)) ? value : null;
}

function nullableBounded(value: unknown, max = 500): string | null | undefined {
	return value === null ? null : bounded(value, max) ?? undefined;
}

function integer(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function nullableInteger(value: unknown): number | null | undefined {
	return value === null ? null : integer(value) ?? undefined;
}

function rank(value: unknown): number | null | undefined {
	const parsed = nullableInteger(value);
	return parsed === 0 ? undefined : parsed;
}

const JOB_STATUSES = new Set<AttentionRankRecomputeJobStatus>([
	'pending', 'in_flight', 'retry', 'succeeded', 'stale', 'dead'
]);
const OUTCOMES = new Set<CanonicalAttentionOutcome>([
	'useful', 'action_completed', 'irrelevant', 'not_actionable', 'duplicate_of',
	'obsolete', 'not_owner', 'neutral_seen', 'timing_negative'
]);
const STALE_REASONS = new Set<AttentionRankRecomputeStaleReason>([
	'candidate_inactive', 'source_revision_changed', 'snapshot_incompatible',
	'universe_changed_during_commit', 'generation_changed_during_commit',
	'no_served_decision', 'served_projection_unreadable'
]);
const OPERATIONAL_REASONS = new Set<AttentionRankRecomputeOperationalReason>([
	'canonical_projection_failed', 'posterior_read_failed', 'rank_recompute_failed',
	'lease_expired', 'max_retries_exhausted'
]);

export function parseAttentionRankRecomputeReference(
	value: unknown
): AttentionRankRecomputeReference | null {
	const input = record(value);
	if (!input || !exact(input, [
		'enqueue_status', 'job_id', 'job_status', 'status_href', 'affected_rank_before',
		'affected_rank_after', 'affected_rank_delta', 'result_semantics', 'reason'
	])) return null;
	const jobId = nullableBounded(input.job_id, 200);
	const jobStatus = input.job_status === null ? null : bounded(input.job_status, 16);
	const href = nullableBounded(input.status_href, 1_000);
	const before = rank(input.affected_rank_before);
	const reason = input.reason === null ? null : bounded(input.reason, 120);
	if (jobId === undefined || href === undefined || before === undefined ||
		input.affected_rank_after !== null || input.affected_rank_delta !== null ||
		!SEMANTICS.has(input.result_semantics as AttentionRankRecomputeSemantics) ||
		(jobStatus !== null && !JOB_STATUSES.has(jobStatus as AttentionRankRecomputeJobStatus))) return null;
	if (input.enqueue_status === 'enqueued') {
		if (!jobId || jobStatus !== 'pending' ||
			href !== `${ATTENTION_RANK_RECOMPUTE_BASE}/jobs/${encodeURIComponent(jobId)}` || reason !== null) return null;
	} else if (input.enqueue_status === 'failed') {
		if (jobId !== null || jobStatus !== null || href !== null || reason !== 'enqueue_failed') return null;
	} else return null;
	return {
		enqueue_status: input.enqueue_status,
		job_id: jobId,
		job_status: jobStatus as AttentionRankRecomputeJobStatus | null,
		status_href: href,
		affected_rank_before: before,
		affected_rank_after: null,
		affected_rank_delta: null,
		result_semantics: input.result_semantics as AttentionRankRecomputeSemantics,
		reason: reason as 'enqueue_failed' | null
	};
}

function parseResult(value: unknown): AttentionRankRecomputeResult | null {
	const input = record(value);
	if (!input || !exact(input, [
		'semantics', 'affected_rank_after', 'affected_rank_delta', 'current_source_revision',
		'universe_digest', 'recompute_generation', 'policy_snapshot_id', 'posterior_version',
		'completed_at'
	])) return null;
	const after = rank(input.affected_rank_after);
	const delta = input.affected_rank_delta === null
		? null
		: typeof input.affected_rank_delta === 'number' && Number.isSafeInteger(input.affected_rank_delta)
			? input.affected_rank_delta : undefined;
	const revision = nullableBounded(input.current_source_revision);
	const digest = bounded(input.universe_digest);
	const generationInput = record(input.recompute_generation);
	const followUpGeneration = integer(generationInput?.follow_up);
	const worthALookGeneration = integer(generationInput?.worth_a_look);
	const snapshot = nullableBounded(input.policy_snapshot_id);
	const posterior = nullableInteger(input.posterior_version);
	const completed = integer(input.completed_at);
	if (after === null || after === undefined || delta === undefined || revision === undefined || !digest ||
		!generationInput || !exact(generationInput, ['follow_up', 'worth_a_look']) ||
		followUpGeneration === null || worthALookGeneration === null ||
		snapshot === undefined || posterior === undefined || completed === null ||
		(snapshot === null) !== (posterior === null) ||
		!SEMANTICS.has(input.semantics as AttentionRankRecomputeSemantics)) return null;
	return {
		semantics: input.semantics as AttentionRankRecomputeSemantics,
		affected_rank_after: after,
		affected_rank_delta: delta,
		current_source_revision: revision,
		universe_digest: digest,
		recompute_generation: {
			follow_up: followUpGeneration,
			worth_a_look: worthALookGeneration
		},
		policy_snapshot_id: snapshot,
		posterior_version: posterior,
		completed_at: completed
	};
}

export function parseAttentionRankRecomputeJobResponse(
	value: unknown,
	expected: AttentionRankRecomputeBinding
): AttentionRankRecomputeJob | null {
	const envelope = record(value);
	const input = record(envelope?.job);
	if (!envelope || !hasRequiredKeys(envelope, ['schema_version', 'job']) || envelope.schema_version !== 1 ||
		!input || !hasRequiredKeys(input, [
			'job_id', 'outcome_id', 'status', 'origin_surface', 'canonical_candidate_id',
			'raw_candidate_id', 'source_revision', 'outcome', 'decision_id', 'delivery_id',
			'impression_id', 'affected_rank_before', 'enqueue_policy_snapshot_id',
			'enqueue_posterior_version', 'attempts', 'next_retry_at', 'lease_expires_at',
			'created_at', 'updated_at', 'completed_at', 'reason', 'result'
		])) return null;
	const jobId = bounded(input.job_id, 200);
	const outcomeId = bounded(input.outcome_id, 200);
	const status = bounded(input.status, 16);
	const canonicalId = bounded(input.canonical_candidate_id);
	const rawId = bounded(input.raw_candidate_id);
	const revision = nullableBounded(input.source_revision);
	const outcome = bounded(input.outcome, 32);
	const decisionId = nullableBounded(input.decision_id, 200);
	const deliveryId = nullableBounded(input.delivery_id, 200);
	const impressionId = nullableBounded(input.impression_id, 200);
	const before = rank(input.affected_rank_before);
	const enqueueSnapshot = nullableBounded(input.enqueue_policy_snapshot_id, 200);
	const enqueuePosterior = nullableInteger(input.enqueue_posterior_version);
	const attempts = integer(input.attempts);
	const nextRetry = nullableInteger(input.next_retry_at);
	const lease = nullableInteger(input.lease_expires_at);
	const created = integer(input.created_at);
	const updated = integer(input.updated_at);
	const completed = nullableInteger(input.completed_at);
	const reason = nullableBounded(input.reason, 120);
	const result = input.result === null ? null : parseResult(input.result);
	if (!jobId || !outcomeId || !status || !JOB_STATUSES.has(status as AttentionRankRecomputeJobStatus) ||
		(input.origin_surface !== 'follow_up' && input.origin_surface !== 'worth_a_look') ||
		!canonicalId || !rawId || revision === undefined || !outcome ||
		!OUTCOMES.has(outcome as CanonicalAttentionOutcome) || decisionId === undefined ||
		deliveryId === undefined || impressionId === undefined || before === undefined ||
		enqueueSnapshot === undefined || enqueuePosterior === undefined || attempts === null ||
		nextRetry === undefined || lease === undefined || created === null || updated === null ||
		completed === undefined || reason === undefined || (input.result !== null && !result) ||
		updated < created || !canonicalMatches(String(input.origin_surface), canonicalId, rawId) ||
		jobId !== expected.job_id || outcomeId !== expected.outcome_id ||
		input.origin_surface !== expected.origin_surface || rawId !== expected.raw_candidate_id ||
		(Object.prototype.hasOwnProperty.call(expected, 'source_revision') &&
			revision !== expected.source_revision) || outcome !== expected.outcome ||
		!compatibleOptionalId(expected.decision_id, decisionId) ||
		!compatibleOptionalId(expected.delivery_id, deliveryId) ||
		!compatibleOptionalId(expected.impression_id, impressionId) ||
		before !== expected.affected_rank_before) return null;
	if ((Object.prototype.hasOwnProperty.call(expected, 'enqueue_policy_snapshot_id') &&
		enqueueSnapshot !== expected.enqueue_policy_snapshot_id) ||
		(Object.prototype.hasOwnProperty.call(expected, 'enqueue_posterior_version') &&
			enqueuePosterior !== expected.enqueue_posterior_version)) return null;
	const terminal = status === 'succeeded' || status === 'stale' || status === 'dead';
	if (terminal !== (completed !== null) ||
		(terminal && completed !== updated) ||
		(status === 'pending' && (attempts !== 0 || nextRetry !== null || lease !== null || reason !== null || result !== null)) ||
		(status === 'in_flight' && (attempts < 1 || nextRetry !== null || lease === null ||
			lease <= updated || reason !== null || result !== null)) ||
		(status === 'retry' && (nextRetry === null || lease !== null || !reason ||
			attempts < 1 || nextRetry < updated ||
			!OPERATIONAL_REASONS.has(reason as AttentionRankRecomputeOperationalReason) || result !== null)) ||
		(status === 'succeeded' && (!result || reason !== null || nextRetry !== null || lease !== null ||
			attempts < 1 || result.completed_at !== completed || result.current_source_revision !== revision ||
			result.affected_rank_delta !== (before === null ? null : result.affected_rank_after - before) ||
			result.policy_snapshot_id !== enqueueSnapshot ||
			(enqueuePosterior !== null && (result.posterior_version === null ||
				result.posterior_version < enqueuePosterior)))) ||
		(status === 'stale' && (!reason || !STALE_REASONS.has(reason as AttentionRankRecomputeStaleReason) ||
			attempts < 1 || result !== null || nextRetry !== null || lease !== null)) ||
		(status === 'dead' && (!reason || !OPERATIONAL_REASONS.has(reason as AttentionRankRecomputeOperationalReason) ||
			attempts < 1 || result !== null || nextRetry !== null || lease !== null))) return null;
	return {
		job_id: jobId,
		outcome_id: outcomeId,
		status: status as AttentionRankRecomputeJobStatus,
		origin_surface: input.origin_surface,
		canonical_candidate_id: canonicalId,
		raw_candidate_id: rawId,
		source_revision: revision,
		outcome: outcome as CanonicalAttentionOutcome,
		decision_id: decisionId,
		delivery_id: deliveryId,
		impression_id: impressionId,
		affected_rank_before: before,
		enqueue_policy_snapshot_id: enqueueSnapshot,
		enqueue_posterior_version: enqueuePosterior,
		attempts,
		next_retry_at: nextRetry,
		lease_expires_at: lease,
		created_at: created,
		updated_at: updated,
		completed_at: completed,
		reason: reason as AttentionRankRecomputeJob['reason'],
		result
	};
}

export function parseAttentionRankRecomputeHealth(value: unknown): AttentionRankRecomputeHealth | null {
	const input = record(value);
	const queue = record(input?.queue);
	const worker = record(input?.worker);
	if (!input || !exact(input, ['schema_version', 'enabled', 'paused', 'pause_reason', 'queue', 'worker']) ||
		input.schema_version !== 1 || typeof input.enabled !== 'boolean' || typeof input.paused !== 'boolean' ||
		!queue || !exact(queue, [
			'pending', 'in_flight', 'retry', 'succeeded', 'stale', 'dead',
			'next_retry_at', 'oldest_pending_at'
		]) || !worker || !exact(worker, [
			'batch_size', 'concurrency', 'interval_secs', 'max_retries', 'lease_secs', 'retention_days'
		])) return null;
	const pauseReason = input.pause_reason === null ? null : bounded(input.pause_reason, 120);
	const pending = integer(queue.pending);
	const inFlight = integer(queue.in_flight);
	const retry = integer(queue.retry);
	const succeeded = integer(queue.succeeded);
	const stale = integer(queue.stale);
	const dead = integer(queue.dead);
	const nextRetry = nullableInteger(queue.next_retry_at);
	const oldestPending = nullableInteger(queue.oldest_pending_at);
	const batchSize = integer(worker.batch_size);
	const concurrency = integer(worker.concurrency);
	const interval = integer(worker.interval_secs);
	const maxRetries = integer(worker.max_retries);
	const lease = integer(worker.lease_secs);
	const retention = integer(worker.retention_days);
	if (pauseReason === undefined || pending === null || inFlight === null || retry === null ||
		succeeded === null || stale === null || dead === null || nextRetry === undefined ||
		oldestPending === undefined || batchSize === null || batchSize < 1 || concurrency === null ||
		concurrency < 1 || interval === null || interval < 1 || maxRetries === null || lease === null ||
		lease < 1 || retention === null || retention < 1 ||
		(input.enabled ? input.paused || pauseReason !== null : !input.paused ||
			pauseReason !== 'rank_recompute_disabled')) return null;
	return {
		schema_version: 1,
		enabled: input.enabled,
		paused: input.paused,
		pause_reason: pauseReason as 'rank_recompute_disabled' | null,
		queue: { pending, in_flight: inFlight, retry, succeeded, stale, dead,
			next_retry_at: nextRetry, oldest_pending_at: oldestPending },
		worker: { batch_size: batchSize, concurrency, interval_secs: interval,
			max_retries: maxRetries, lease_secs: lease, retention_days: retention }
	};
}

function scopedUrl(path: string, _scope: CanonicalAttentionProjectionScope): string {
	return path;
}

export async function fetchAttentionRankRecomputeJob(
	statusHref: string,
	scope: CanonicalAttentionProjectionScope,
	expected: AttentionRankRecomputeBinding
): Promise<AttentionRankRecomputeFetchResult> {
	if (statusHref !== `${ATTENTION_RANK_RECOMPUTE_BASE}/jobs/${encodeURIComponent(expected.job_id)}`) {
		return { ok: false, error: 'Invalid rank recompute status binding', retryable: false };
	}
	try {
		const response = await fetch(scopedUrl(statusHref, scope), {
			headers: scopedRequestHeaders()
		});
		if (!response.ok) {
			return {
				ok: false,
				error: `HTTP ${response.status}`,
				retryable: response.status === 400 || response.status === 404 || response.status >= 500
			};
		}
		const job = parseAttentionRankRecomputeJobResponse(await response.json().catch(() => null), expected);
		return job
			? { ok: true, job }
			: { ok: false, error: 'Malformed rank recompute job', retryable: true };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error), retryable: true };
	}
}

export async function fetchAttentionRankRecomputeHealth(
	scope: CanonicalAttentionProjectionScope
): Promise<AttentionRankRecomputeHealth | null> {
	try {
		const response = await fetch(scopedUrl(`${ATTENTION_RANK_RECOMPUTE_BASE}/status`, scope), {
			headers: scopedRequestHeaders()
		});
		if (!response.ok) return null;
		return parseAttentionRankRecomputeHealth(await response.json().catch(() => null));
	} catch {
		return null;
	}
}
