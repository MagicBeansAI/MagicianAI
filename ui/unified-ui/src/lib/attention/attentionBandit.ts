import type { AttentionDecisionItem } from './attentionRouting';

export type AttentionBanditMode = 'disabled' | 'shadow' | 'canary';
export type AttentionPosteriorUpdateStatus =
	| 'updated'
	| 'duplicate'
	| 'neutral'
	| 'degraded'
	| 'disabled';
export type AttentionAttributionQuality =
	| 'verified_impression'
	| 'decision_only'
	| 'missing'
	| 'mismatch'
	| 'outside_window';

export interface AttentionBanditDecision {
	schema_version: 1;
	mode: AttentionBanditMode;
	policy_snapshot_id: string | null;
	policy_model_version: string | null;
	posterior_version: number;
	posterior_uncertainty: number | null;
	proposed_position: number | null;
	served_position: number;
	served_propensity: number;
	posterior_draw_count: number;
	seed_identity: string;
	support: boolean;
	exploration: boolean;
	applied: boolean;
	degradation_reason: string | null;
}

export interface AttentionBanditHealth {
	mode: AttentionBanditMode;
	policy_snapshot_id: string | null;
	posterior_version: number;
	posterior_update_count: number;
	propensity_coverage: number;
	exploration_rate: number;
	support_ok: boolean;
	first_page_bounded: boolean;
	degradation_reason: string | null;
}

export interface AttentionFeedbackAttribution {
	decision_id: string;
	candidate_id: string;
	source_revision?: string | null;
	impression_id?: string;
	delivery_id?: string;
}

export interface AttentionPosteriorUpdate {
	status: AttentionPosteriorUpdateStatus;
	policy_snapshot_id: string | null;
	posterior_version_before: number | null;
	posterior_version_after: number | null;
	attribution_quality: AttentionAttributionQuality;
	degradation_reason: string | null;
	uncertainty_before: number | null;
	uncertainty_after: number | null;
	affected_rank_before: number | null;
	affected_rank_after: number | null;
	affected_rank_delta: number | null;
	rescore_scheduled: boolean;
}

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function boundedString(value: unknown, maxCharacters = 200): string | null {
	return typeof value === 'string' &&
		value.trim().length > 0 &&
		[...value].length <= maxCharacters &&
		![...value].some((character) => /\p{Cc}/u.test(character))
		? value
		: null;
}

function optionalString(value: unknown, maxCharacters = 200): string | null | undefined {
	if (value === null || value === undefined) return null;
	return boundedString(value, maxCharacters) ?? undefined;
}

function safeInteger(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function unsigned32(value: unknown): number | null {
	const parsed = safeInteger(value);
	return parsed !== null && parsed <= 4_294_967_295 ? parsed : null;
}

function finiteNonNegative(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

function probability(value: unknown): number | null {
	const parsed = finiteNonNegative(value);
	return parsed !== null && parsed <= 1 ? parsed : null;
}

function optionalInteger(value: unknown): number | null | undefined {
	if (value === null || value === undefined) return null;
	return safeInteger(value) ?? undefined;
}

function optionalUnsigned32(value: unknown): number | null | undefined {
	if (value === null || value === undefined) return null;
	return unsigned32(value) ?? undefined;
}

function optionalNonNegative(value: unknown): number | null | undefined {
	if (value === null || value === undefined) return null;
	return finiteNonNegative(value) ?? undefined;
}

const BANDIT_MODES = new Set<AttentionBanditMode>(['disabled', 'shadow', 'canary']);
const UPDATE_STATUSES = new Set<AttentionPosteriorUpdateStatus>([
	'updated',
	'duplicate',
	'neutral',
	'degraded',
	'disabled'
]);
const ATTRIBUTION_QUALITIES = new Set<AttentionAttributionQuality>([
	'verified_impression',
	'decision_only',
	'missing',
	'mismatch',
	'outside_window'
]);

export function parseAttentionBanditDecision(
	value: unknown,
	decisionItem?: AttentionDecisionItem | null
): AttentionBanditDecision | null {
	const input = record(value);
	if (!input || input.schema_version !== 1) return null;
	const mode = boundedString(input.mode, 16);
	const snapshotId = optionalString(input.policy_snapshot_id);
	const modelVersion = optionalString(input.policy_model_version);
	const posteriorVersion = safeInteger(input.posterior_version);
	const uncertainty = optionalNonNegative(input.posterior_uncertainty);
	const proposedPosition = optionalUnsigned32(input.proposed_position);
	const servedPosition = unsigned32(input.served_position);
	const servedPropensity = probability(input.served_propensity);
	const drawCount = unsigned32(input.posterior_draw_count);
	const seedIdentity = boundedString(input.seed_identity, 200);
	const degradationReason = optionalString(input.degradation_reason, 200);
	if (
		!mode ||
		!BANDIT_MODES.has(mode as AttentionBanditMode) ||
		snapshotId === undefined ||
		modelVersion === undefined ||
		(snapshotId === null) !== (modelVersion === null) ||
		posteriorVersion === null ||
		uncertainty === undefined ||
		proposedPosition === undefined ||
		(proposedPosition !== null && proposedPosition < 1) ||
		servedPosition === null ||
		servedPropensity === null ||
		drawCount === null ||
		!seedIdentity ||
		typeof input.support !== 'boolean' ||
		typeof input.exploration !== 'boolean' ||
		typeof input.applied !== 'boolean' ||
		degradationReason === undefined
	) {
		return null;
	}
	if (input.applied) {
		if (
			mode !== 'canary' ||
			!input.support ||
			degradationReason !== null ||
			snapshotId === null ||
			modelVersion === null
		) {
			return null;
		}
	} else if (decisionItem?.selected && servedPropensity !== 1) {
		return null;
	}
	if (input.exploration && mode !== 'canary') return null;
	if (
		decisionItem &&
		(decisionItem.served_rank !== servedPosition ||
			(decisionItem.selected && servedPosition < 1))
	) {
		return null;
	}
	return {
		schema_version: 1,
		mode: mode as AttentionBanditMode,
		policy_snapshot_id: snapshotId,
		policy_model_version: modelVersion,
		posterior_version: posteriorVersion,
		posterior_uncertainty: uncertainty,
		proposed_position: proposedPosition,
		served_position: servedPosition,
		served_propensity: servedPropensity,
		posterior_draw_count: drawCount,
		seed_identity: seedIdentity,
		support: input.support,
		exploration: input.exploration,
		applied: input.applied,
		degradation_reason: degradationReason
	};
}

export function parseAttentionBanditHealth(value: unknown): AttentionBanditHealth | null {
	const root = record(value);
	const input = record(root?.bandit_health);
	if (!input) return null;
	const mode = boundedString(input.mode, 16);
	const snapshotId = optionalString(input.policy_snapshot_id);
	const posteriorVersion = safeInteger(input.posterior_version);
	const updateCount = safeInteger(input.posterior_update_count);
	const propensityCoverage = probability(input.propensity_coverage);
	const explorationRate = probability(input.exploration_rate);
	const degradationReason = optionalString(input.degradation_reason, 200);
	if (
		!mode ||
		!BANDIT_MODES.has(mode as AttentionBanditMode) ||
		snapshotId === undefined ||
		posteriorVersion === null ||
		updateCount === null ||
		propensityCoverage === null ||
		explorationRate === null ||
		typeof input.support_ok !== 'boolean' ||
		input.first_page_bounded !== true ||
		degradationReason === undefined
	) {
		return null;
	}
	return {
		mode: mode as AttentionBanditMode,
		policy_snapshot_id: snapshotId,
		posterior_version: posteriorVersion,
		posterior_update_count: updateCount,
		propensity_coverage: propensityCoverage,
		exploration_rate: explorationRate,
		support_ok: input.support_ok,
		first_page_bounded: input.first_page_bounded,
		degradation_reason: degradationReason
	};
}

export function parseAttentionPosteriorUpdate(value: unknown): AttentionPosteriorUpdate | null {
	const input = record(value);
	if (!input) return null;
	const status = boundedString(input.status, 16);
	const snapshotId = optionalString(input.policy_snapshot_id);
	const beforeVersion = optionalInteger(input.posterior_version_before);
	const afterVersion = optionalInteger(input.posterior_version_after);
	const quality = boundedString(input.attribution_quality, 32);
	const degradationReason = optionalString(input.degradation_reason, 200);
	const uncertaintyBefore = optionalNonNegative(input.uncertainty_before);
	const uncertaintyAfter = optionalNonNegative(input.uncertainty_after);
	const rankBefore = optionalUnsigned32(input.affected_rank_before);
	const rankAfter = optionalUnsigned32(input.affected_rank_after);
	const rankDelta = input.affected_rank_delta === null || input.affected_rank_delta === undefined
		? null
		: typeof input.affected_rank_delta === 'number' &&
				Number.isSafeInteger(input.affected_rank_delta)
			? input.affected_rank_delta
			: undefined;
	if (
		!status ||
		!UPDATE_STATUSES.has(status as AttentionPosteriorUpdateStatus) ||
		snapshotId === undefined ||
		beforeVersion === undefined ||
		afterVersion === undefined ||
		!quality ||
		!ATTRIBUTION_QUALITIES.has(quality as AttentionAttributionQuality) ||
		degradationReason === undefined ||
		uncertaintyBefore === undefined ||
		uncertaintyAfter === undefined ||
		rankBefore === undefined ||
		rankAfter === undefined ||
		rankDelta === undefined ||
		typeof input.rescore_scheduled !== 'boolean'
	) {
		return null;
	}
	if (status === 'updated' && (afterVersion === null || uncertaintyAfter === null)) return null;
	if (status === 'neutral' && input.rescore_scheduled) return null;
	return {
		status: status as AttentionPosteriorUpdateStatus,
		policy_snapshot_id: snapshotId,
		posterior_version_before: beforeVersion,
		posterior_version_after: afterVersion,
		attribution_quality: quality as AttentionAttributionQuality,
		degradation_reason: degradationReason,
		uncertainty_before: uncertaintyBefore,
		uncertainty_after: uncertaintyAfter,
		affected_rank_before: rankBefore,
		affected_rank_after: rankAfter,
		affected_rank_delta: rankDelta,
		rescore_scheduled: input.rescore_scheduled
	};
}

export function posteriorUpdateMessage(update: AttentionPosteriorUpdate | null): string | null {
	if (!update) return null;
	if (update.status === 'neutral') return 'Personal posterior unchanged · neutral outcome.';
	if (update.status === 'updated') {
		const uncertainty =
			update.uncertainty_before !== null && update.uncertainty_after !== null
				? ` · uncertainty ${update.uncertainty_before.toFixed(3)} → ${update.uncertainty_after.toFixed(3)}`
				: '';
		const rank =
			update.affected_rank_before !== null && update.affected_rank_after !== null
				? ` · affected rank ${update.affected_rank_before} → ${update.affected_rank_after}`
				: '';
		return `Personal posterior updated${update.posterior_version_after === null ? '' : ` · version ${update.posterior_version_after}`}${uncertainty}${rank}.`;
	}
	if (update.status === 'duplicate') return 'Personal posterior already reflected this outcome.';
	if (update.status === 'disabled') return 'Personal posterior disabled; outcome still recorded.';
	return `Personal posterior degraded${update.degradation_reason ? ` · ${update.degradation_reason.replace(/_/g, ' ')}` : ''}; outcome still recorded.`;
}
