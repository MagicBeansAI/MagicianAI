/**
 * Progressive Slice 1 attention-learning contracts shared by the Follow-up
 * store and its web surfaces. Every field is additive so a web client can roll
 * out before or after the backend without making the existing lifecycle
 * actions unavailable.
 */

import {
	parseAttentionPosteriorUpdate,
	posteriorUpdateMessage,
	type AttentionPosteriorUpdate
} from '$lib/attention/attentionBandit';
import {
	parseAttentionRankRecomputeReference,
	type AttentionRankRecomputeReference
} from '$lib/attention/attentionRankRecompute';

export const CHANNEL_FOLLOW_UP_DISMISS_REASONS = [
	'spam',
	'already_handled',
	'duplicate',
	'delegated',
	'not_relevant',
	'wrong_classification'
] as const;

export type ChannelFollowUpDismissReason =
	(typeof CHANNEL_FOLLOW_UP_DISMISS_REASONS)[number];

export type CanonicalAttentionOutcome =
	| 'useful'
	| 'action_completed'
	| 'irrelevant'
	| 'not_actionable'
	| 'duplicate_of'
	| 'obsolete'
	| 'not_owner'
	| 'neutral_seen'
	| 'timing_negative';

export type AttentionSurface = 'follow_up' | 'worth_a_look';

export type AttentionRescoreStatus =
	| 'completed'
	| 'disabled'
	| 'degraded_no_embedding'
	| 'degraded_no_candidates'
	| 'failed';

export interface AttentionFeedbackReceipt {
	outcome_id: string;
	outcome: CanonicalAttentionOutcome;
	surface: AttentionSurface;
	feedback_recorded: boolean;
	affected_candidates: number;
	rescore_status: AttentionRescoreStatus;
	embedding_contract: string | null;
	diagnostic_href: string | null;
	posterior_update: AttentionPosteriorUpdate | null;
	/** Missing is a legacy receipt; new responses emit explicit null/object. */
	rank_recompute?: AttentionRankRecomputeReference | null;
}

export interface ChannelFollowUpLearningRank {
	baseline_rank: number;
	learned_rank: number;
	rank_delta: number;
	learning_score: number | null;
}

export interface ChannelFollowUpLearningHealth {
	total_active: number;
	source_family_counts: Record<string, number>;
	embedded_candidates: number;
	embedding_coverage: number;
	learned_rank_changes: number;
	semantic_ranking_enabled?: boolean;
	coverage_scope?: string;
	rank_policy_version?: string;
}

export type AttentionActionabilityMode = 'disabled' | 'shadow' | 'enforced';
export type SemanticFeatureStatus = 'succeeded' | 'missing' | 'invalid';
export type AttentionActionabilityScoreStatus = 'scored' | 'fallback' | 'disabled';

export interface AttentionActionabilityPage {
	mode: AttentionActionabilityMode;
	snapshot_id: string | null;
	semantic_extraction_coverage: number;
	scored_count: number;
	fallback_count: number;
}

export interface AttentionActionabilityTrainingStatus {
	enabled: boolean;
	auto_install: boolean;
	interval_secs: number;
	last_status: string | null;
	last_reason: string | null;
	usable: number | null;
	unlinked: number | null;
	label_count: number | null;
	positive: number | null;
	negative: number | null;
	auc: number | null;
	ece: number | null;
	last_snapshot_id: string | null;
	installed_snapshot_id: string | null;
	effective_mode: AttentionActionabilityMode | string;
	last_run_at: number | null;
}

export interface AttentionActionabilityExplanation {
	code: string;
	label: string;
}

export interface AttentionActionabilityCard {
	probability: number | null;
	explanation: AttentionActionabilityExplanation | null;
	model_version: string | null;
	snapshot_id: string | null;
	semantic_feature_status: SemanticFeatureStatus;
	score_status: AttentionActionabilityScoreStatus;
	mode: AttentionActionabilityMode;
}

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function finiteNumber(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function nonNegativeInteger(value: unknown): number | null {
	const number = finiteNumber(value);
	return number !== null && number >= 0 ? Math.floor(number) : null;
}

function strictNonNegativeInteger(value: unknown): number | null {
	const number = finiteNumber(value);
	return number !== null && number >= 0 && Number.isInteger(number) ? number : null;
}

function stringValue(value: unknown): string | null {
	return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

const OUTCOMES = new Set<CanonicalAttentionOutcome>([
	'useful',
	'action_completed',
	'irrelevant',
	'not_actionable',
	'duplicate_of',
	'obsolete',
	'not_owner',
	'neutral_seen',
	'timing_negative'
]);

const RESCORE_STATUSES = new Set<AttentionRescoreStatus>([
	'completed',
	'disabled',
	'degraded_no_embedding',
	'degraded_no_candidates',
	'failed'
]);

const SURFACES = new Set<AttentionSurface>(['follow_up', 'worth_a_look']);
const ACTIONABILITY_MODES = new Set<AttentionActionabilityMode>([
	'disabled',
	'shadow',
	'enforced'
]);
const SEMANTIC_FEATURE_STATUSES = new Set<SemanticFeatureStatus>([
	'succeeded',
	'missing',
	'invalid'
]);
const ACTIONABILITY_SCORE_STATUSES = new Set<AttentionActionabilityScoreStatus>([
	'scored',
	'fallback',
	'disabled'
]);

export function parseAttentionFeedbackReceipt(value: unknown): AttentionFeedbackReceipt | null {
	const input = record(value);
	if (!input) return null;
	const outcomeId = stringValue(input.outcome_id);
	const outcome = stringValue(input.outcome);
	const surface = stringValue(input.surface);
	const feedbackRecorded = input.feedback_recorded;
	const affectedCandidates = nonNegativeInteger(input.affected_candidates);
	const rescoreStatus = stringValue(input.rescore_status);
	const hasRankRecompute = Object.prototype.hasOwnProperty.call(input, 'rank_recompute');
	const rankRecompute = !hasRankRecompute || input.rank_recompute === null
		? null
		: parseAttentionRankRecomputeReference(input.rank_recompute);
	if (
		!outcomeId ||
		!outcome ||
		!OUTCOMES.has(outcome as CanonicalAttentionOutcome) ||
		!surface ||
		!SURFACES.has(surface as AttentionSurface) ||
		typeof feedbackRecorded !== 'boolean' ||
		affectedCandidates === null ||
		!rescoreStatus ||
		!RESCORE_STATUSES.has(rescoreStatus as AttentionRescoreStatus) ||
		(hasRankRecompute && input.rank_recompute !== null && !rankRecompute)
	) {
		return null;
	}
	const posteriorUpdate = parseAttentionPosteriorUpdate(input.posterior_update);
	if (rankRecompute && posteriorUpdate && posteriorUpdate.affected_rank_after !== null) return null;
	if (rankRecompute && posteriorUpdate &&
		rankRecompute.affected_rank_before !== posteriorUpdate.affected_rank_before) return null;
	return {
		outcome_id: outcomeId,
		outcome: outcome as CanonicalAttentionOutcome,
		surface: surface as AttentionSurface,
		feedback_recorded: feedbackRecorded,
		affected_candidates: affectedCandidates,
		rescore_status: rescoreStatus as AttentionRescoreStatus,
		embedding_contract: stringValue(input.embedding_contract),
		diagnostic_href: stringValue(input.diagnostic_href),
		posterior_update: posteriorUpdate,
		...(hasRankRecompute ? { rank_recompute: rankRecompute } : {})
	};
}

export function parseChannelFollowUpLearningRank(
	value: unknown
): ChannelFollowUpLearningRank | null {
	const input = record(value);
	if (!input) return null;
	const baselineRank = nonNegativeInteger(input.baseline_rank);
	const learnedRank = nonNegativeInteger(input.learned_rank);
	const rankDelta = finiteNumber(input.rank_delta);
	const learningScore = input.learning_score === null ? null : finiteNumber(input.learning_score);
	if (
		baselineRank === null ||
		baselineRank < 1 ||
		learnedRank === null ||
		learnedRank < 1 ||
		rankDelta === null ||
		(input.learning_score !== null && input.learning_score !== undefined && learningScore === null)
	) {
		return null;
	}
	return {
		baseline_rank: baselineRank,
		learned_rank: learnedRank,
		rank_delta: Math.trunc(rankDelta),
		learning_score: learningScore
	};
}

export function parseChannelFollowUpLearningHealth(
	value: unknown
): ChannelFollowUpLearningHealth | null {
	const input = record(value);
	if (!input) return null;
	const totalActive = nonNegativeInteger(input.total_active);
	const embeddedCandidates = nonNegativeInteger(input.embedded_candidates);
	const embeddingCoverage = finiteNumber(input.embedding_coverage);
	const learnedRankChanges = nonNegativeInteger(input.learned_rank_changes);
	const rawCounts = record(input.source_family_counts);
	if (
		totalActive === null ||
		embeddedCandidates === null ||
		embeddingCoverage === null ||
		embeddingCoverage < 0 ||
		learnedRankChanges === null ||
		!rawCounts
	) {
		return null;
	}
	const boundedCoverage = Math.min(1, embeddingCoverage);
	const sourceFamilyCounts: Record<string, number> = {};
	for (const [family, rawCount] of Object.entries(rawCounts)) {
		const count = nonNegativeInteger(rawCount);
		if (count !== null) sourceFamilyCounts[family] = count;
	}
	return {
		total_active: totalActive,
		source_family_counts: sourceFamilyCounts,
		embedded_candidates: embeddedCandidates,
		embedding_coverage: boundedCoverage,
		learned_rank_changes: learnedRankChanges,
		...(typeof input.semantic_ranking_enabled === 'boolean'
			? { semantic_ranking_enabled: input.semantic_ranking_enabled }
			: {}),
		...(stringValue(input.coverage_scope)
			? { coverage_scope: stringValue(input.coverage_scope)! }
			: {}),
		...(stringValue(input.rank_policy_version)
			? { rank_policy_version: stringValue(input.rank_policy_version)! }
			: {})
	};
}

export function feedbackReceiptMessage(
	baseMessage: string,
	receipt: AttentionFeedbackReceipt | null
): string {
	if (!receipt) return baseMessage;
	const posteriorMessage = posteriorUpdateMessage(receipt.posterior_update);
	const withPosterior = (message: string): string =>
		posteriorMessage ? `${message} ${posteriorMessage}` : message;
	const withRankJob = (message: string): string => {
		if (receipt.rank_recompute === undefined) return message;
		if (receipt.rank_recompute === null) {
			return `${message} Durable rank recompute is unavailable.`;
		}
		if (receipt.rank_recompute.enqueue_status === 'failed') {
			return `${message} Rank recompute could not be queued; the outcome remains recorded.`;
		}
		return `${message} Rank recompute queued; lane order remains server-owned until refresh.`;
	};
	const withDiagnostics = (message: string): string => withRankJob(withPosterior(message));
	if (!receipt.feedback_recorded) {
		return withDiagnostics(
			`${baseMessage}. The action completed, but its learning feedback was not recorded.`
		);
	}
	const affected = receipt.affected_candidates;
	const related = `${affected} related candidate${affected === 1 ? '' : 's'}`;
	if (receipt.rescore_status === 'completed') {
		return withDiagnostics(`${baseMessage}. Feedback recorded; ${related} re-scored.`);
	}
	if (receipt.rescore_status === 'disabled') {
		return withDiagnostics(`${baseMessage}. Feedback recorded; semantic re-scoring is disabled.`);
	}
	if (receipt.rescore_status === 'degraded_no_embedding') {
		return withDiagnostics(`${baseMessage}. Feedback recorded; re-scoring is waiting for an embedding.`);
	}
	if (receipt.rescore_status === 'failed') {
		return withDiagnostics(`${baseMessage}. Feedback recorded; related-candidate re-scoring failed.`);
	}
	return withDiagnostics(
		`${baseMessage}. Feedback recorded; no related candidates required re-scoring.`
	);
}

export function followUpRankDeltaLabel(
	rank: ChannelFollowUpLearningRank | null,
	semanticRankingEnabled: boolean
): string | null {
	if (!rank) return null;
	const prefix = semanticRankingEnabled ? 'Learned rank' : 'Rank preview';
	if (rank.rank_delta === 0) return `${prefix} ${rank.learned_rank} · unchanged`;
	const direction = rank.rank_delta > 0 ? 'up' : 'down';
	return `${prefix} ${rank.learned_rank} · ${direction} ${Math.abs(rank.rank_delta)} from baseline`;
}

export function parseAttentionActionabilityPage(
	value: unknown
): AttentionActionabilityPage | null {
	const input = record(value);
	if (!input) return null;
	const mode = stringValue(input.actionability_mode);
	const extractionCoverage = finiteNumber(input.semantic_extraction_coverage);
	const scoredCount = strictNonNegativeInteger(input.actionability_scored_count);
	const fallbackCount = strictNonNegativeInteger(input.actionability_fallback_count);
	if (
		!mode ||
		!ACTIONABILITY_MODES.has(mode as AttentionActionabilityMode) ||
		extractionCoverage === null ||
		extractionCoverage < 0 ||
		extractionCoverage > 1 ||
		scoredCount === null ||
		fallbackCount === null
	) {
		return null;
	}
	return {
		mode: mode as AttentionActionabilityMode,
		snapshot_id: stringValue(input.actionability_snapshot_id),
		semantic_extraction_coverage: extractionCoverage,
		scored_count: scoredCount,
		fallback_count: fallbackCount
	};
}

export function parseAttentionActionabilityTraining(
	value: unknown
): AttentionActionabilityTrainingStatus | null {
	const root = record(value);
	const input = record(root?.actionability_training) ?? root;
	if (!input) return null;
	if (typeof input.enabled !== 'boolean' || typeof input.auto_install !== 'boolean') {
		return null;
	}
	const interval = finiteNumber(input.interval_secs);
	if (interval === null || interval < 0) return null;
	return {
		enabled: input.enabled,
		auto_install: input.auto_install,
		interval_secs: interval,
		last_status: stringValue(input.last_status),
		last_reason: stringValue(input.last_reason),
		usable: nonNegativeInteger(input.usable),
		unlinked: nonNegativeInteger(input.unlinked),
		label_count: nonNegativeInteger(input.label_count),
		positive: nonNegativeInteger(input.positive),
		negative: nonNegativeInteger(input.negative),
		auc: finiteNumber(input.auc),
		ece: finiteNumber(input.ece),
		last_snapshot_id: stringValue(input.last_snapshot_id),
		installed_snapshot_id: stringValue(input.installed_snapshot_id),
		effective_mode: stringValue(input.effective_mode) ?? 'disabled',
		last_run_at: nonNegativeInteger(input.last_run_at)
	};
}

export function parseAttentionActionabilityCard(
	value: unknown
): AttentionActionabilityCard | null {
	const input = record(value);
	if (!input) return null;
	const mode = stringValue(input.actionability_mode);
	const semanticFeatureStatus = stringValue(input.semantic_feature_status);
	const scoreStatus = stringValue(input.actionability_score_status);
	const rawProbability = input.actionability_probability;
	const probability = rawProbability === null ? null : finiteNumber(rawProbability);
	if (
		!mode ||
		!ACTIONABILITY_MODES.has(mode as AttentionActionabilityMode) ||
		!semanticFeatureStatus ||
		!SEMANTIC_FEATURE_STATUSES.has(semanticFeatureStatus as SemanticFeatureStatus) ||
		!scoreStatus ||
		!ACTIONABILITY_SCORE_STATUSES.has(scoreStatus as AttentionActionabilityScoreStatus) ||
		(rawProbability !== null && probability === null) ||
		(probability !== null && (probability < 0 || probability > 1))
	) {
		return null;
	}
	const explanationRecord = record(input.actionability_explanation);
	const explanationCode = stringValue(explanationRecord?.code);
	const explanationLabel = stringValue(explanationRecord?.label);
	if (
		input.actionability_explanation !== null &&
		input.actionability_explanation !== undefined &&
		(!explanationCode || !explanationLabel)
	) {
		return null;
	}
	return {
		probability,
		explanation:
			explanationCode && explanationLabel
				? { code: explanationCode, label: explanationLabel }
				: null,
		model_version: stringValue(input.actionability_model_version),
		snapshot_id: stringValue(input.actionability_snapshot_id),
		semantic_feature_status: semanticFeatureStatus as SemanticFeatureStatus,
		score_status: scoreStatus as AttentionActionabilityScoreStatus,
		mode: mode as AttentionActionabilityMode
	};
}

export function actionabilityPresentation(
	metadata: AttentionActionabilityCard | null
): { label: string; active: boolean; degraded: boolean } | null {
	if (!metadata || metadata.mode === 'disabled' || metadata.score_status === 'disabled') return null;
	const prefix = metadata.mode === 'enforced' ? 'Actionability' : 'Actionability preview';
	if (metadata.score_status === 'fallback' || metadata.semantic_feature_status !== 'succeeded') {
		const reason =
			metadata.semantic_feature_status === 'invalid'
				? 'invalid semantic features'
				: metadata.semantic_feature_status === 'missing'
					? 'semantic features pending'
					: 'model unavailable';
		return {
			label: `${prefix} unavailable · ${reason} · Slice 1 fallback`,
			active: false,
			degraded: true
		};
	}
	if (metadata.probability === null) {
		return {
			label: `${prefix} unavailable · Slice 1 fallback`,
			active: false,
			degraded: true
		};
	}
	const probability = `${Math.round(metadata.probability * 100)}%`;
	return {
		label: `${prefix} ${probability}${metadata.explanation ? ` · ${metadata.explanation.label}` : ''}`,
		active: metadata.mode === 'enforced',
		degraded: false
	};
}
