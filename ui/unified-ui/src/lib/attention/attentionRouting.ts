/**
 * Slice 4 decision, routing, and verified-impression contracts. Everything is
 * additive: malformed or absent metadata must never remove or reroute a card.
 */

export type AttentionSurface = 'follow_up' | 'worth_a_look';
export type AttentionRoute = AttentionSurface | 'non_surfaced';
export type AttentionRoutingMode = 'baseline' | 'shadow' | 'canary';

/**
 * Canonical decision-ledger identity for a surface-local candidate id.
 *
 * Decisions, decision items, outcomes, and delivery items are all keyed by this
 * surface-qualified form, because one identity space has to span both lanes —
 * a bare id cannot say which lane it belongs to, and the same underlying source
 * can appear in both. Rows rendered from a surface-local store carry the raw id,
 * so anything matched against the ledger normalizes through here first.
 */
export function canonicalCandidateId(
	surface: AttentionSurface,
	rawCandidateId: string
): string {
	return `${surface}:${rawCandidateId}`;
}
export const ATTENTION_MIN_VISIBLE_MS_MAX = 60_000;
export const ATTENTION_VISIBLE_MS_MAX = 86_400_000;
export const ATTENTION_EVENT_ID_MAX_CHARS = 200;
export const ATTENTION_DECISION_ID_MAX_CHARS = 200;
export const ATTENTION_CANDIDATE_ID_MAX_CHARS = 500;
export const ATTENTION_SOURCE_REVISION_MAX_CHARS = 500;
export const ATTENTION_VISIBILITY_RULE_MAX_CHARS = 100;
export const ATTENTION_CLIENT_TYPE_MAX_CHARS = 64;
export const ATTENTION_CLIENT_VERSION_MAX_CHARS = 128;
export const ATTENTION_VIEWPORT_CLASS_MAX_CHARS = 64;
export type AttentionRouteReason =
	| 'baseline_mode'
	| 'shadow_only'
	| 'not_in_canary'
	| 'snapshot_missing'
	| 'snapshot_invalid'
	| 'contract_mismatch'
	| 'below_confidence_gate'
	| 'below_margin_gate'
	| 'learned_route_applied'
	| 'knn_demotion_applied'
	| 'knn_promotion_applied'
	| 'knn_lane_shadow'
	| 'hard_ineligible'
	| 'cross_lane_universe_incomplete'
	| 'baseline_route_retained';

export interface AttentionDecisionItem {
	decision_id: string;
	candidate_id: string;
	source_revision: string | null;
	baseline_route: AttentionRoute;
	learned_route: AttentionRoute;
	served_route: AttentionRoute;
	routing_mode: AttentionRoutingMode;
	routing_snapshot_id: string | null;
	routing_model_version: string | null;
	learned_route_confidence: number | null;
	utility_margin: number | null;
	route_reason: AttentionRouteReason;
	served_rank: number;
	selected: boolean;
	route_applied: boolean;
	canary_assigned: boolean;
}

export interface AttentionDecision {
	decision_id: string;
	decided_at: number;
	surface: AttentionSurface;
	routing_mode: AttentionRoutingMode;
	routing_snapshot_id: string | null;
	eligible_item_count: number;
	selected_item_count: number;
	returned_item_count: number;
	complete_universe_recorded: boolean;
	complete_cross_lane_universe?: boolean;
	degradation_reason: string | null;
}

export interface AttentionImpressionPolicy {
	min_visible_ms: number;
	visibility_rule_version: string;
}

export interface AttentionRoutingHealth {
	routing_snapshot_valid: boolean;
	evaluated_count: number;
	learned_route_count: number;
	applied_route_count: number;
	baseline_retained_count: number;
	impression_eligible_count: number;
	decision_item_coverage: number;
	all_candidates_path: string;
	verified_impression_coverage?: number;
	/** Scope-level verified impressions; not tied to any single decision. */
	verified_impression_total?: number;
	impression_dedupe_count?: number;
}

export interface AttentionRoutingPage {
	decision: AttentionDecision;
	impression_policy: AttentionImpressionPolicy;
	health: AttentionRoutingHealth;
}

export interface AttentionImpressionRequest {
	event_id: string;
	decision_id: string;
	candidate_id: string;
	source_revision: string | null;
	surface: AttentionSurface;
	visible_ms: number;
	visibility_rule_version: string;
	client_type: 'web';
	client_version: string;
	viewport_class: string;
}

export interface AttentionImpressionReceipt {
	impression_id: string;
	event_id: string;
	decision_id: string;
	candidate_id: string;
	source_revision: string | null;
	surface: AttentionSurface;
	accumulated_visible_ms: number;
	min_visible_ms: number;
	visibility_rule_version: string;
	verified: boolean;
	deduplicated: boolean;
}

export type AttentionImpressionResult =
	| { ok: true; receipt: AttentionImpressionReceipt }
	| { ok: false; error: string; code: string | null; retryable: boolean };

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function stringValue(value: unknown): string | null {
	return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function boundedProtocolString(value: unknown, maxCharacters: number): string | null {
	return typeof value === 'string' &&
		value.trim().length > 0 &&
		[...value].length <= maxCharacters &&
		![...value].some((character) => /\p{Cc}/u.test(character))
		? value
		: null;
}

function nullableString(value: unknown): string | null | undefined {
	if (value === null) return null;
	return stringValue(value) ?? undefined;
}

function safeInteger(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function finite(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function probability(value: unknown): number | null {
	const parsed = finite(value);
	return parsed !== null && parsed >= 0 && parsed <= 1 ? parsed : null;
}

const SURFACES = new Set<AttentionSurface>(['follow_up', 'worth_a_look']);
const ROUTES = new Set<AttentionRoute>(['follow_up', 'worth_a_look', 'non_surfaced']);
const MODES = new Set<AttentionRoutingMode>(['baseline', 'shadow', 'canary']);
const REASONS = new Set<AttentionRouteReason>([
	'baseline_mode',
	'shadow_only',
	'not_in_canary',
	'snapshot_missing',
	'snapshot_invalid',
	'contract_mismatch',
	'below_confidence_gate',
	'below_margin_gate',
	'learned_route_applied',
	'knn_demotion_applied',
	'knn_promotion_applied',
	'knn_lane_shadow',
	'hard_ineligible',
	'cross_lane_universe_incomplete',
	'baseline_route_retained'
]);

export function parseAttentionDecisionItem(
	value: unknown,
	expected?: {
		decision_id?: string;
		candidate_id?: string;
		source_revision?: string | null;
		routing_mode?: AttentionRoutingMode;
		routing_snapshot_id?: string | null;
	}
): AttentionDecisionItem | null {
	const input = record(value);
	if (!input) return null;
	const decisionId = boundedProtocolString(input.decision_id, ATTENTION_DECISION_ID_MAX_CHARS);
	const candidateId = boundedProtocolString(input.candidate_id, ATTENTION_CANDIDATE_ID_MAX_CHARS);
	const sourceRevision = input.source_revision === null
		? null
		: boundedProtocolString(input.source_revision, ATTENTION_SOURCE_REVISION_MAX_CHARS) ?? undefined;
	const baselineRoute = stringValue(input.baseline_route);
	const learnedRoute = stringValue(input.learned_route);
	const servedRoute = stringValue(input.served_route);
	const mode = stringValue(input.routing_mode);
	const snapshotId = input.routing_snapshot_id === undefined
		? null
		: nullableString(input.routing_snapshot_id);
	const modelVersion = input.routing_model_version === undefined
		? null
		: nullableString(input.routing_model_version);
	const confidence = input.learned_route_confidence === null || input.learned_route_confidence === undefined
		? null
		: probability(input.learned_route_confidence);
	const margin = input.utility_margin === null || input.utility_margin === undefined
		? null
		: finite(input.utility_margin);
	const reason = stringValue(input.route_reason);
	const servedRank = safeInteger(input.served_rank);
	if (
		!decisionId ||
		!candidateId ||
		sourceRevision === undefined ||
		!baselineRoute ||
		!ROUTES.has(baselineRoute as AttentionRoute) ||
		!learnedRoute ||
		!ROUTES.has(learnedRoute as AttentionRoute) ||
		!servedRoute ||
		!ROUTES.has(servedRoute as AttentionRoute) ||
		!mode ||
		!MODES.has(mode as AttentionRoutingMode) ||
		snapshotId === undefined ||
		modelVersion === undefined ||
		(confidence === null &&
			input.learned_route_confidence !== null &&
			input.learned_route_confidence !== undefined) ||
		(margin === null && input.utility_margin !== null && input.utility_margin !== undefined) ||
		!reason ||
		!REASONS.has(reason as AttentionRouteReason) ||
		servedRank === null ||
		typeof input.selected !== 'boolean' ||
		typeof input.route_applied !== 'boolean' ||
		typeof input.canary_assigned !== 'boolean'
	) {
		return null;
	}
	if (input.selected && servedRank < 1) return null;
	if (input.route_applied) {
		if (servedRoute !== learnedRoute || learnedRoute === baselineRoute) {
			return null;
		}
		const knnApplied = reason === 'knn_demotion_applied' || reason === 'knn_promotion_applied';
		if (reason === 'learned_route_applied') {
			if (mode !== 'canary' || !input.canary_assigned) return null;
		} else if (!knnApplied) {
			return null;
		}
	} else if (servedRoute !== baselineRoute) {
		return null;
	}
	if (
		(expected?.decision_id && expected.decision_id !== decisionId) ||
		(expected?.candidate_id && expected.candidate_id !== candidateId) ||
		(expected && Object.prototype.hasOwnProperty.call(expected, 'source_revision') &&
			expected.source_revision !== sourceRevision) ||
		(expected?.routing_mode && expected.routing_mode !== mode) ||
		(expected && Object.prototype.hasOwnProperty.call(expected, 'routing_snapshot_id') &&
			expected.routing_snapshot_id !== snapshotId)
	) {
		return null;
	}
	return {
		decision_id: decisionId,
		candidate_id: candidateId,
		source_revision: sourceRevision,
		baseline_route: baselineRoute as AttentionRoute,
		learned_route: learnedRoute as AttentionRoute,
		served_route: servedRoute as AttentionRoute,
		routing_mode: mode as AttentionRoutingMode,
		routing_snapshot_id: snapshotId,
		routing_model_version: modelVersion,
		learned_route_confidence: confidence,
		utility_margin: margin,
		route_reason: reason as AttentionRouteReason,
		served_rank: servedRank,
		selected: input.selected,
		route_applied: input.route_applied,
		canary_assigned: input.canary_assigned
	};
}

export function parseAttentionRoutingPage(value: unknown): AttentionRoutingPage | null {
	const root = record(value);
	const rawDecision = record(root?.decision);
	const rawPolicy = record(root?.impression_policy);
	const rawHealth = record(root?.routing_health);
	if (!rawDecision || !rawPolicy || !rawHealth) return null;
	const decisionId = boundedProtocolString(
		rawDecision.decision_id,
		ATTENTION_DECISION_ID_MAX_CHARS
	);
	const decidedAt = safeInteger(rawDecision.decided_at);
	const surface = stringValue(rawDecision.surface);
	const mode = stringValue(rawDecision.routing_mode);
	const snapshotId = rawDecision.routing_snapshot_id === undefined
		? null
		: nullableString(rawDecision.routing_snapshot_id);
	const eligible = safeInteger(rawDecision.eligible_item_count);
	const selected = safeInteger(rawDecision.selected_item_count);
	const returned = safeInteger(rawDecision.returned_item_count);
	const degradationReason = rawDecision.degradation_reason === undefined
		? null
		: nullableString(rawDecision.degradation_reason);
	const completeCrossLaneUniverse = rawDecision.complete_cross_lane_universe === undefined
		? false
		: rawDecision.complete_cross_lane_universe;
	const minVisibleMs = safeInteger(rawPolicy.min_visible_ms);
	const ruleVersion = boundedProtocolString(
		rawPolicy.visibility_rule_version,
		ATTENTION_VISIBILITY_RULE_MAX_CHARS
	);
	const evaluated = safeInteger(rawHealth.evaluated_count);
	const learned = safeInteger(rawHealth.learned_route_count);
	const applied = safeInteger(rawHealth.applied_route_count);
	const retained = safeInteger(rawHealth.baseline_retained_count);
	const impressionEligible = safeInteger(rawHealth.impression_eligible_count);
	const decisionCoverage = probability(rawHealth.decision_item_coverage);
	const allCandidatesPath = stringValue(rawHealth.all_candidates_path);
	const verifiedCoverage = rawHealth.verified_impression_coverage === undefined
		? undefined
		: probability(rawHealth.verified_impression_coverage);
	const verifiedTotal = rawHealth.verified_impression_total === undefined
		? undefined
		: safeInteger(rawHealth.verified_impression_total);
	const dedupeCount = rawHealth.impression_dedupe_count === undefined
		? undefined
		: safeInteger(rawHealth.impression_dedupe_count);
	if (
		!decisionId ||
		decidedAt === null ||
		!surface ||
		!SURFACES.has(surface as AttentionSurface) ||
		!mode ||
		!MODES.has(mode as AttentionRoutingMode) ||
		snapshotId === undefined ||
		eligible === null ||
		selected === null ||
		returned === null ||
		typeof rawDecision.complete_universe_recorded !== 'boolean' ||
		typeof completeCrossLaneUniverse !== 'boolean' ||
		degradationReason === undefined ||
		minVisibleMs === null ||
		minVisibleMs < 1 ||
		minVisibleMs > ATTENTION_MIN_VISIBLE_MS_MAX ||
		!ruleVersion ||
		typeof rawHealth.routing_snapshot_valid !== 'boolean' ||
		evaluated === null ||
		learned === null ||
		applied === null ||
		retained === null ||
		impressionEligible === null ||
		decisionCoverage === null ||
		!allCandidatesPath ||
		verifiedCoverage === null ||
		dedupeCount === null
	) {
		return null;
	}
	return {
		decision: {
			decision_id: decisionId,
			decided_at: decidedAt,
			surface: surface as AttentionSurface,
			routing_mode: mode as AttentionRoutingMode,
			routing_snapshot_id: snapshotId,
			eligible_item_count: eligible,
			selected_item_count: selected,
			returned_item_count: returned,
			complete_universe_recorded: rawDecision.complete_universe_recorded,
			complete_cross_lane_universe: completeCrossLaneUniverse,
			degradation_reason: degradationReason
		},
		impression_policy: {
			min_visible_ms: minVisibleMs,
			visibility_rule_version: ruleVersion
		},
		health: {
			routing_snapshot_valid: rawHealth.routing_snapshot_valid,
			evaluated_count: evaluated,
			learned_route_count: learned,
			applied_route_count: applied,
			baseline_retained_count: retained,
			impression_eligible_count: impressionEligible,
			decision_item_coverage: decisionCoverage,
			all_candidates_path: allCandidatesPath,
			...(verifiedCoverage === undefined
				? {}
				: { verified_impression_coverage: verifiedCoverage }),
			...(verifiedTotal === undefined || verifiedTotal === null
				? {}
				: { verified_impression_total: verifiedTotal }),
			...(dedupeCount === undefined ? {} : { impression_dedupe_count: dedupeCount })
		}
	};
}

export function parseAttentionImpressionReceipt(
	value: unknown
): AttentionImpressionReceipt | null {
	const input = record(value);
	if (!input) return null;
	const impressionId = boundedProtocolString(input.impression_id, ATTENTION_EVENT_ID_MAX_CHARS);
	const eventId = boundedProtocolString(input.event_id, ATTENTION_EVENT_ID_MAX_CHARS);
	const decisionId = boundedProtocolString(input.decision_id, ATTENTION_DECISION_ID_MAX_CHARS);
	const candidateId = boundedProtocolString(input.candidate_id, ATTENTION_CANDIDATE_ID_MAX_CHARS);
	const sourceRevision = input.source_revision === null
		? null
		: boundedProtocolString(input.source_revision, ATTENTION_SOURCE_REVISION_MAX_CHARS) ?? undefined;
	const surface = stringValue(input.surface);
	const accumulatedVisibleMs = safeInteger(input.accumulated_visible_ms);
	const minVisibleMs = safeInteger(input.min_visible_ms);
	const ruleVersion = boundedProtocolString(
		input.visibility_rule_version,
		ATTENTION_VISIBILITY_RULE_MAX_CHARS
	);
	if (
		!impressionId ||
		!eventId ||
		!decisionId ||
		!candidateId ||
		sourceRevision === undefined ||
		!surface ||
		!SURFACES.has(surface as AttentionSurface) ||
		accumulatedVisibleMs === null ||
		accumulatedVisibleMs > ATTENTION_VISIBLE_MS_MAX ||
		minVisibleMs === null ||
		minVisibleMs < 1 ||
		minVisibleMs > ATTENTION_MIN_VISIBLE_MS_MAX ||
		!ruleVersion ||
		typeof input.verified !== 'boolean' ||
		typeof input.deduplicated !== 'boolean'
	) {
		return null;
	}
	return {
		impression_id: impressionId,
		event_id: eventId,
		decision_id: decisionId,
		candidate_id: candidateId,
		source_revision: sourceRevision,
		surface: surface as AttentionSurface,
		accumulated_visible_ms: accumulatedVisibleMs,
		min_visible_ms: minVisibleMs,
		visibility_rule_version: ruleVersion,
		verified: input.verified,
		deduplicated: input.deduplicated
	};
}

function errorDetails(value: unknown): { code: string | null; message: string | null } {
	const input = record(value);
	const error = record(input?.error);
	return {
		code: stringValue(error?.code),
		message: stringValue(error?.message)
	};
}

function validImpressionRequest(request: AttentionImpressionRequest): boolean {
	return Boolean(
		boundedProtocolString(request.event_id, ATTENTION_EVENT_ID_MAX_CHARS) &&
		boundedProtocolString(request.decision_id, ATTENTION_DECISION_ID_MAX_CHARS) &&
		boundedProtocolString(request.candidate_id, ATTENTION_CANDIDATE_ID_MAX_CHARS) &&
		(request.source_revision === null ||
			boundedProtocolString(request.source_revision, ATTENTION_SOURCE_REVISION_MAX_CHARS)) &&
		SURFACES.has(request.surface) &&
		Number.isSafeInteger(request.visible_ms) &&
		request.visible_ms >= 1 &&
		request.visible_ms <= ATTENTION_VISIBLE_MS_MAX &&
		boundedProtocolString(
			request.visibility_rule_version,
			ATTENTION_VISIBILITY_RULE_MAX_CHARS
		) &&
		boundedProtocolString(request.client_type, ATTENTION_CLIENT_TYPE_MAX_CHARS) &&
		boundedProtocolString(request.client_version, ATTENTION_CLIENT_VERSION_MAX_CHARS) &&
		boundedProtocolString(request.viewport_class, ATTENTION_VIEWPORT_CLASS_MAX_CHARS)
	);
}

export async function postAttentionImpression(
	request: AttentionImpressionRequest
): Promise<AttentionImpressionResult> {
	if (!validImpressionRequest(request)) {
		return {
			ok: false,
			error: 'Invalid verified-impression contract',
			code: 'invalid_impression_request',
			retryable: false
		};
	}
	try {
		const response = await fetch(
			'/api/magician/v2/channel-assist/attention-learning/impressions',
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(request)
			}
		);
		const body: unknown = await response.json().catch(() => null);
		if (!response.ok) {
			const details = errorDetails(body);
			return {
				ok: false,
				error: details.message ?? `HTTP ${response.status}`,
				code: details.code,
				retryable: response.status >= 500
			};
		}
		const receipt = parseAttentionImpressionReceipt(body);
		return receipt
			? { ok: true, receipt }
			: {
					ok: false,
					error: 'Malformed impression receipt',
					code: null,
					retryable: false
				};
	} catch (error) {
		return {
			ok: false,
			error: error instanceof Error ? error.message : String(error),
			code: null,
			retryable: true
		};
	}
}
