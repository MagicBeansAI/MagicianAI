/**
 * Evidence-preserving Slice 3 grouping contracts shared by Follow-ups and
 * Worth a look. Grouping is additive: callers must keep their Slice 2 list
 * whenever this contract is absent, malformed, disabled, or not reconciled.
 */

export type AttentionGroupingMode = 'disabled' | 'shadow' | 'enforced';
export type AttentionPairLabel = 'same_underlying_item' | 'same_obligation' | 'not_duplicate';
export type AttentionPairSurface = 'follow_up' | 'worth_a_look';

export interface AttentionGroupingHealth {
	candidate_total: number;
	member_total: number;
	cluster_total: number;
	representative_total: number;
	collapsed_member_total: number;
	scored_pair_total: number;
	cannot_link_total: number;
	fallback_ungrouped_total: number;
	totals_reconcile: boolean;
	/** Additive boundedness diagnostics. Absent on older servers. When
	 * `budget_exceeded` is true, the server evaluated no pairs and returned the
	 * complete eligible universe as singleton rows. */
	pair_evaluation_budget?: number;
	required_pair_evaluations?: number;
	budget_exceeded?: boolean;
}

export interface AttentionGroupingPage {
	mode: AttentionGroupingMode;
	snapshot_id: string | null;
	generation: number | null;
	scope: 'eligible_universe' | 'canonical_eligible_universe';
	health: AttentionGroupingHealth;
}

export interface AttentionGroupingMetadata {
	cluster_id: string;
	representative_id: string;
	is_representative: boolean;
	member_count: number;
	related_count: number;
	model_version: string | null;
	snapshot_id: string | null;
	merge_probability: number | null;
}

export interface AttentionPairRef {
	candidate_id: string;
	source_revision: string | null;
}

export interface AttentionPairCorrectionRequest {
	event_id: string;
	surface: AttentionPairSurface;
	left: AttentionPairRef;
	right: AttentionPairRef;
	label: AttentionPairLabel;
	confidence?: number;
}

export interface AttentionPairCorrectionReceipt {
	pair_label_id: string;
	inserted: boolean;
	label: AttentionPairLabel;
	canonical_left_id: string;
	canonical_right_id: string;
	affected_cluster_ids: string[];
	grouping_generation: number;
	recomputed: boolean;
}

export type AttentionPairCorrectionResult =
	| { ok: true; receipt: AttentionPairCorrectionReceipt }
	| { ok: false; error: string };

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function stringValue(value: unknown): string | null {
	return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function nullableString(value: unknown): string | null | undefined {
	if (value === null) return null;
	return typeof value === 'string' && value.trim().length > 0 ? value.trim() : undefined;
}

function safeNonNegativeInteger(value: unknown): number | null {
	return typeof value === 'number' &&
		Number.isSafeInteger(value) &&
		value >= 0
		? value
		: null;
}

function probability(value: unknown): number | null | undefined {
	if (value === null) return null;
	return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1
		? value
		: undefined;
}

const GROUPING_MODES = new Set<AttentionGroupingMode>(['disabled', 'shadow', 'enforced']);
const PAIR_LABELS = new Set<AttentionPairLabel>([
	'same_underlying_item',
	'same_obligation',
	'not_duplicate'
]);

export function parseAttentionGroupingPage(value: unknown): AttentionGroupingPage | null {
	const input = record(value);
	const rawHealth = record(input?.grouping_health);
	if (!input || !rawHealth) return null;
	const mode = stringValue(input.grouping_mode);
	const snapshotId = nullableString(input.grouping_snapshot_id);
	const generation =
		input.grouping_generation === null
			? null
			: safeNonNegativeInteger(input.grouping_generation);
	const scope = input.grouping_scope;
	const candidateTotal = safeNonNegativeInteger(rawHealth.candidate_total);
	const memberTotal = safeNonNegativeInteger(rawHealth.member_total);
	const clusterTotal = safeNonNegativeInteger(rawHealth.cluster_total);
	const representativeTotal = safeNonNegativeInteger(rawHealth.representative_total);
	const collapsedMemberTotal = safeNonNegativeInteger(rawHealth.collapsed_member_total);
	const scoredPairTotal = safeNonNegativeInteger(rawHealth.scored_pair_total);
	const cannotLinkTotal = safeNonNegativeInteger(rawHealth.cannot_link_total);
	const fallbackUngroupedTotal = safeNonNegativeInteger(rawHealth.fallback_ungrouped_total);
	const pairEvaluationBudget = safeNonNegativeInteger(rawHealth.pair_evaluation_budget);
	const requiredPairEvaluations = safeNonNegativeInteger(rawHealth.required_pair_evaluations);
	const budgetExceeded = rawHealth.budget_exceeded;
	const pairBudgetFieldsValid =
		pairEvaluationBudget !== null &&
		requiredPairEvaluations !== null &&
		typeof budgetExceeded === 'boolean';
	if (
		!mode ||
		!GROUPING_MODES.has(mode as AttentionGroupingMode) ||
		snapshotId === undefined ||
		(generation === null && input.grouping_generation !== null) ||
		(scope !== 'eligible_universe' && scope !== 'canonical_eligible_universe') ||
		candidateTotal === null ||
		memberTotal === null ||
		clusterTotal === null ||
		representativeTotal === null ||
		collapsedMemberTotal === null ||
		scoredPairTotal === null ||
		cannotLinkTotal === null ||
		fallbackUngroupedTotal === null ||
		typeof rawHealth.totals_reconcile !== 'boolean'
	) {
		return null;
	}
	return {
		mode: mode as AttentionGroupingMode,
		snapshot_id: snapshotId,
		generation,
		scope,
		health: {
			candidate_total: candidateTotal,
			member_total: memberTotal,
			cluster_total: clusterTotal,
			representative_total: representativeTotal,
			collapsed_member_total: collapsedMemberTotal,
			scored_pair_total: scoredPairTotal,
			cannot_link_total: cannotLinkTotal,
			fallback_ungrouped_total: fallbackUngroupedTotal,
			totals_reconcile: rawHealth.totals_reconcile,
			...(pairBudgetFieldsValid
				? {
						pair_evaluation_budget: pairEvaluationBudget,
						required_pair_evaluations: requiredPairEvaluations,
						budget_exceeded: budgetExceeded
					}
				: {})
		}
	};
}

export function parseAttentionGroupingMetadata(
	value: unknown
): AttentionGroupingMetadata | null {
	const input = record(value);
	if (!input) return null;
	const clusterId = stringValue(input.cluster_id);
	const representativeId = stringValue(input.representative_id);
	const memberCount = safeNonNegativeInteger(input.member_count);
	const relatedCount = safeNonNegativeInteger(input.related_count);
	const modelVersion = nullableString(input.model_version);
	const snapshotId = nullableString(input.snapshot_id);
	const mergeProbability = probability(input.merge_probability);
	if (
		!clusterId ||
		!representativeId ||
		typeof input.is_representative !== 'boolean' ||
		memberCount === null ||
		memberCount < 1 ||
		relatedCount === null ||
		relatedCount > memberCount - 1 ||
		modelVersion === undefined ||
		snapshotId === undefined ||
		mergeProbability === undefined
	) {
		return null;
	}
	return {
		cluster_id: clusterId,
		representative_id: representativeId,
		is_representative: input.is_representative,
		member_count: memberCount,
		related_count: relatedCount,
		model_version: modelVersion,
		snapshot_id: snapshotId,
		merge_probability: mergeProbability
	};
}

export function groupingTotalsReconcile(page: AttentionGroupingPage | null): boolean {
	if (!page?.health.totals_reconcile) return false;
	const health = page.health;
	return (
		health.candidate_total === health.member_total &&
		health.cluster_total === health.representative_total &&
		health.representative_total <= health.member_total &&
		health.collapsed_member_total === health.member_total - health.representative_total
	);
}

export function groupingMayCollapse(page: AttentionGroupingPage | null): boolean {
	return page?.mode === 'enforced' && page.snapshot_id !== null && groupingTotalsReconcile(page);
}

export function groupingAffordanceLabel(
	metadata: AttentionGroupingMetadata,
	page: AttentionGroupingPage | null
): string | null {
	if (!page || metadata.related_count < 1 || page.mode === 'disabled') return null;
	const updates = `${metadata.related_count} related update${metadata.related_count === 1 ? '' : 's'}`;
	return groupingMayCollapse(page) ? updates : `Grouping preview · ${updates}`;
}

export function parseAttentionPairCorrectionReceipt(
	value: unknown
): AttentionPairCorrectionReceipt | null {
	const input = record(value);
	if (!input) return null;
	const pairLabelId = stringValue(input.pair_label_id);
	const label = stringValue(input.label);
	const canonicalLeftId = stringValue(input.canonical_left_id);
	const canonicalRightId = stringValue(input.canonical_right_id);
	const generation = safeNonNegativeInteger(input.grouping_generation);
	const affectedClusterIds = Array.isArray(input.affected_cluster_ids)
		? input.affected_cluster_ids.flatMap((entry) => {
				const id = stringValue(entry);
				return id ? [id] : [];
			})
		: null;
	if (
		!pairLabelId ||
		!label ||
		!PAIR_LABELS.has(label as AttentionPairLabel) ||
		!canonicalLeftId ||
		!canonicalRightId ||
		generation === null ||
		!affectedClusterIds ||
		typeof input.inserted !== 'boolean' ||
		input.recomputed !== true
	) {
		return null;
	}
	return {
		pair_label_id: pairLabelId,
		inserted: input.inserted,
		label: label as AttentionPairLabel,
		canonical_left_id: canonicalLeftId,
		canonical_right_id: canonicalRightId,
		affected_cluster_ids: affectedClusterIds,
		grouping_generation: generation,
		recomputed: true
	};
}

export function createPairCorrectionEventId(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return crypto.randomUUID();
	}
	return `attention-pair-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export async function postAttentionPairCorrection(
	request: AttentionPairCorrectionRequest
): Promise<AttentionPairCorrectionResult> {
	try {
		const response = await fetch(
			'/api/magician/v2/channel-assist/attention-learning/pair-corrections',
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(request)
			}
		);
		const body: unknown = await response.json().catch(() => null);
		if (!response.ok) {
			const error = record(body);
			return {
				ok: false,
				error: stringValue(error?.error) ?? `HTTP ${response.status}`
			};
		}
		const receipt = parseAttentionPairCorrectionReceipt(body);
		return receipt
			? { ok: true, receipt }
			: { ok: false, error: 'Malformed pair-correction receipt' };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}
