/**
 * Defensive client contracts for the Today/Square "Worth a look" surface.
 *
 * List reads remain compatible with the original summary-only cards. Rich
 * briefs, source reads, recommendations, and contextual actions are optional:
 * an old or partially rolled-out backend therefore degrades to Details rather
 * than making either page fail to render.
 */

import {
	parseAttentionFeedbackReceipt,
	parseAttentionActionabilityCard,
	parseAttentionActionabilityPage,
	parseAttentionActionabilityTraining,
	parseChannelFollowUpLearningHealth,
	parseChannelFollowUpLearningRank,
	type AttentionActionabilityCard,
	type AttentionActionabilityPage,
	type AttentionActionabilityTrainingStatus,
	type AttentionFeedbackReceipt,
	type ChannelFollowUpLearningHealth
} from '$lib/channel/channelFollowUpLearning';
import {
	parseAttentionGroupingMetadata,
	parseAttentionGroupingPage,
	type AttentionGroupingMetadata,
	type AttentionGroupingPage
} from '$lib/attention/attentionGrouping';
import {
	parseAttentionDecisionItem,
	parseAttentionRoutingPage,
	type AttentionDecisionItem,
	type AttentionRoutingPage
} from '$lib/attention/attentionRouting';
import {
	parseAttentionBanditDecision,
	parseAttentionBanditHealth,
	type AttentionBanditDecision,
	type AttentionBanditHealth,
	type AttentionFeedbackAttribution
} from '$lib/attention/attentionBandit';
import { attentionRankRecomputeStore } from '$lib/stores/attentionRankRecomputeStore';
import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';
import {
	parseAttentionSemanticExtractionHealth,
	type AttentionSemanticExtractionHealth
} from '$lib/attention/attentionSemanticExtraction';
import {
	canonicalAttentionProjectionFromResponse,
	type CanonicalAttentionProjection,
	type CanonicalAttentionProjectionScope,
	type CrossLaneReconciliationUnavailableReason
} from '$lib/attention/canonicalAttentionProjection';

export type ResurfacingAction = 'open' | 'acknowledge' | 'dismiss' | 'owner_work';

/** Optional dismiss reason (channel-assist vocabulary). Tunes how hard the
 *  ranker suppresses similar future items; omit for a plain dismiss. */
export type ResurfacingDismissReason =
	| 'spam'
	| 'already_handled'
	| 'duplicate'
	| 'delegated'
	| 'not_relevant';

export const RESURFACING_ACTION_KINDS = [
	'view_details',
	'open_source',
	'show_original',
	'ask_presto',
	'create_task',
	'create_reminder',
	'share',
	'save_to_memory',
	'summarize_deeper'
] as const;

export type ResurfacingActionKind = (typeof RESURFACING_ACTION_KINDS)[number];
export type ResurfacingDetailStatus = 'complete' | 'partial' | 'source_omits_details';
export type ResurfacingBriefStatus = 'v2' | 'legacy';
export type ResurfacingSourceStatus =
	| 'available'
	| 'newer_available'
	| 'stale'
	| 'offline'
	| 'deleted'
	| 'suppressed'
	| 'unsupported'
	| 'unavailable';
export type ResurfacingSideEffect =
	| 'none'
	| 'creates_task'
	| 'creates_reminder'
	| 'creates_share_draft'
	| 'creates_memory_candidate';
export type ResurfacingRecommendationSource = 'curator' | 'deterministic';
export type ResurfacingRecommendationEvent = 'presented' | 'selected' | 'completed';

export interface ResurfacingChangeFact {
	aspect: string;
	before: string | null;
	after: string | null;
	effective_text: string | null;
}

export interface ResurfacingTemporalFact {
	kind: string;
	text: string;
	at_ms: number | null;
	timezone: string | null;
}

export interface ResurfacingBrief {
	schema_version: number;
	key_facts: string[];
	changes: ResurfacingChangeFact[];
	temporal_facts: ResurfacingTemporalFact[];
	detail_status: ResurfacingDetailStatus;
	missing_details: string[];
}

export interface ResurfacingActionCapability {
	kind: ResurfacingActionKind;
	label: string;
	requires_input: boolean;
	side_effect: ResurfacingSideEffect;
}

export interface ResurfacingRecommendation {
	kind: ResurfacingActionKind;
	label: string;
	rationale: string;
	confidence: number;
	content_revision: string | null;
	source: ResurfacingRecommendationSource;
}

export interface ResurfacingCard {
	candidate_id: string;
	/** Curator line. The structured brief remains authoritative for facts. */
	line: string;
	why_now: string;
	source_title: string;
	summary: string;
	source_kind: string;
	source_ref: string;
	source_revision?: string | null;
	source_route?: string | null;
	open_url?: string | null;
	detail_label: string;
	temporal_anchor_at: number | null;
	brief: ResurfacingBrief | null;
	brief_status: ResurfacingBriefStatus;
	content_revision: string | null;
	source_updated: boolean;
	recommended_action: ResurfacingRecommendation | null;
	actions: ResurfacingActionCapability[];
	baseline_rank?: number;
	learned_rank?: number;
	rank_delta?: number;
	learning_score?: number | null;
	actionability_probability?: number | null;
	actionability_explanation?: { code: string; label: string } | null;
	actionability_model_version?: string | null;
	actionability_snapshot_id?: string | null;
	semantic_feature_status?: 'succeeded' | 'missing' | 'invalid';
	actionability_score_status?: 'scored' | 'fallback' | 'disabled';
	actionability_mode?: 'disabled' | 'shadow' | 'enforced';
	/** Normalized additive metadata. Absent means Slice 2 was not available and
	 *  does not affect the existing card or its actions. */
	actionability?: AttentionActionabilityCard;
	grouping?: AttentionGroupingMetadata;
	grouping_page?: AttentionGroupingPage;
	decision_item?: AttentionDecisionItem;
	/** Display-only server decision; the client never samples or reorders. */
	bandit_decision?: AttentionBanditDecision;
	routing_page?: AttentionRoutingPage;
}

export type ResurfacingSourceMetadata =
	| {
			kind: 'comm';
			provider: string;
			account_alias: string;
			account_email: string | null;
			thread_id: string;
			message_id: string;
			received_at: number;
			evidence_message_ids: string[];
	  }
	| {
			kind: 'task';
			task_id: string;
			status: string;
			outcome: string | null;
			updated_at: string;
	  }
	| {
			kind: 'memory';
			tier: string;
			key: string;
			updated_at: string | null;
	  }
	| {
			kind: 'web';
			url: string;
	  };

export interface ResurfacingEvidenceMessage {
	message_id: string;
	subject: string | null;
	summary: string | null;
	received_at: number;
	body: string | null;
	truncated: boolean;
	attachment_count: number;
}

export type ResurfacingOriginalContent =
	| {
			kind: 'comm';
			message_id: string;
			subject: string | null;
			summary: string | null;
			received_at: number;
			body: string | null;
			evidence_messages: ResurfacingEvidenceMessage[];
	  }
	| {
			kind: 'task';
			task_id: string;
			title: string;
			status: string;
			outcome: string | null;
	  }
	| {
			kind: 'memory';
			tier: string;
			key: string;
			summary: string;
			updated_at: string | null;
	  }
	| {
			kind: 'web';
			url: string;
			title: string;
			summary: string;
	  };

export interface ResurfacingDetail {
	candidate_id: string;
	source_kind: string;
	status: ResurfacingSourceStatus;
	title: string | null;
	summary: string | null;
	brief: ResurfacingBrief | null;
	content_revision: string | null;
	source_revision: string | null;
	source_updated: boolean;
	has_newer: boolean;
	source_route: string | null;
	open_url: string | null;
	source: ResurfacingSourceMetadata | null;
	recommended_action: ResurfacingRecommendation | null;
	actions: ResurfacingActionCapability[];
	original: ResurfacingOriginalContent | null;
	temporal_anchor_at: number | null;
}

export interface ResurfacingCursor {
	surfaced_at: number;
	score: number;
	candidate_id: string;
}

interface ResurfacingCrossLaneReconciliationBase {
	schema_version: 1;
	authoritative_lane: 'follow_up';
	principal: string;
	workspace: string;
	follow_up_source_total: number | null;
	worth_a_look_source_total: number | null;
	raw_source_total: number | null;
	visible_source_total: number | null;
	duplicate_hidden_total: number | null;
	raw_scanned_total: number | null;
	visible_page_total: number | null;
	duplicate_hidden_page_total: number | null;
	reconciliation_digest: string | null;
}

export interface ResurfacingCrossLaneReconciliationSucceeded
	extends ResurfacingCrossLaneReconciliationBase {
	status: 'succeeded';
	reason: null;
	follow_up_source_total: number;
	worth_a_look_source_total: number;
	raw_source_total: number;
	visible_source_total: number;
	duplicate_hidden_total: number;
	raw_scanned_total: number;
	visible_page_total: number;
	duplicate_hidden_page_total: number;
	reconciliation_digest: string;
}

export interface ResurfacingCrossLaneReconciliationUnavailable
	extends ResurfacingCrossLaneReconciliationBase {
	status: 'unavailable';
	reason: CrossLaneReconciliationUnavailableReason;
}

export type ResurfacingCrossLaneReconciliation =
	| ResurfacingCrossLaneReconciliationSucceeded
	| ResurfacingCrossLaneReconciliationUnavailable;

export interface ResurfacingPage {
	cards: ResurfacingCard[];
	total: number;
	limit: number;
	offset: number;
	has_more: boolean;
	next_cursor: ResurfacingCursor | null;
	health: ChannelFollowUpLearningHealth | null;
	actionability: AttentionActionabilityPage | null;
	actionability_training: AttentionActionabilityTrainingStatus | null;
	grouping: AttentionGroupingPage | null;
	routing: AttentionRoutingPage | null;
	bandit: AttentionBanditHealth | null;
	semantic_extraction: AttentionSemanticExtractionHealth | null;
	/** Strict all-or-nothing Follow-up/Worth union; null means legacy fallback. */
	canonical_attention_projection: CanonicalAttentionProjection | null;
	/** Server-owned exact-identity filtering proof. Cards are publishable only
	 * when this is a valid succeeded record bound to the current scope. */
	cross_lane_reconciliation: ResurfacingCrossLaneReconciliation | null;
	cross_lane_reconciliation_error: string | null;
	semantic_ranking_enabled: boolean;
}

export type ResurfacingContextualActionResult =
	| { kind: 'task'; task_id: string; route: string }
	| {
			kind: 'reminder';
			reminder_id: string | null;
			provider: string | null;
			app_url: string | null;
			/** Compatibility fields for results stored before native reminders. */
			task_id: string | null;
			route: string | null;
			at: string;
			timezone: string | null;
	  }
	| {
			kind: 'memory_candidate';
			learning_candidate_id: string;
			route: string;
			review_required: boolean;
	  }
	| {
			kind: 'ask_presto';
			route: string;
			context: { context_type: string; candidate_id: string };
	  }
	| { kind: 'share_draft'; task_id: string; route: string; approval_required: boolean }
	| {
			kind: 'deeper_summary';
			summary: string;
			key_points: string[];
			recommended_actions: string[];
			caveats: string[];
	  };

export interface ResurfacingContextualActionResponse {
	candidate_id: string;
	action: ResurfacingActionKind;
	result_ref: string;
	replayed: boolean;
	result: ResurfacingContextualActionResult;
}

export interface ResurfacingContextualActionRequest {
	kind: ResurfacingActionKind;
	idempotency_key: string;
	content_revision?: string | null;
	input?: Record<string, unknown>;
}

export class ResurfacingApiError extends Error {
	constructor(
		message: string,
		public readonly status: number,
		public readonly code: string | null = null
	) {
		super(message);
		this.name = 'ResurfacingApiError';
	}
}

function asRecord(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function asString(value: unknown): string {
	return typeof value === 'string' ? value : '';
}

function asNullableString(value: unknown): string | null {
	return typeof value === 'string' && value.trim() ? value : null;
}

function asNum(value: unknown, fallback = 0): number {
	return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function asNonNegativeInt(value: unknown): number {
	return Math.max(0, Math.floor(asNum(value)));
}

function asBool(value: unknown): boolean {
	return value === true;
}

function asOptionalMs(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function strictCount(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function exactKeys(source: Record<string, unknown>, keys: readonly string[]): boolean {
	const actual = Object.keys(source);
	return actual.length === keys.length &&
		keys.every((key) => Object.prototype.hasOwnProperty.call(source, key));
}

function boundedRequiredString(value: unknown, maxLength: number): string | null {
	return typeof value === 'string' && value.trim().length > 0 && value.length <= maxLength
		? value
		: null;
}

const CROSS_LANE_UNAVAILABLE_REASONS = new Set<CrossLaneReconciliationUnavailableReason>([
	'follow_up_store_unavailable',
	'follow_up_load_unavailable',
	'follow_up_source_changed',
	'worth_a_look_load_unavailable',
	'malformed_worth_comm_source_ref',
	'canonical_projection_unavailable'
]);

export function parseResurfacingCrossLaneReconciliation(
	value: unknown,
	expectedScope?: CanonicalAttentionProjectionScope
): ResurfacingCrossLaneReconciliation | null {
	const source = asRecord(value);
	if (!source || !exactKeys(source, [
		'schema_version', 'status', 'reason', 'authoritative_lane', 'principal', 'workspace',
		'follow_up_source_total', 'worth_a_look_source_total', 'raw_source_total',
		'visible_source_total', 'duplicate_hidden_total', 'raw_scanned_total',
		'visible_page_total', 'duplicate_hidden_page_total', 'reconciliation_digest'
	]) || source.schema_version !== 1 || source.authoritative_lane !== 'follow_up') return null;
	const principal = boundedRequiredString(source.principal, 256);
	const workspace = boundedRequiredString(source.workspace, 256);
	if (!principal || !workspace ||
		(expectedScope && (principal !== expectedScope.principal || workspace !== expectedScope.workspace))) {
		return null;
	}
	const nullableCount = (entry: unknown): number | null | undefined =>
		entry === null ? null : strictCount(entry) ?? undefined;
	const followUpSourceTotal = nullableCount(source.follow_up_source_total);
	const worthSourceTotal = nullableCount(source.worth_a_look_source_total);
	const rawSourceTotal = nullableCount(source.raw_source_total);
	const visibleSourceTotal = nullableCount(source.visible_source_total);
	const duplicateHiddenTotal = nullableCount(source.duplicate_hidden_total);
	const rawScannedTotal = nullableCount(source.raw_scanned_total);
	const visiblePageTotal = nullableCount(source.visible_page_total);
	const duplicateHiddenPageTotal = nullableCount(source.duplicate_hidden_page_total);
	const digest = source.reconciliation_digest === null
		? null
		: boundedRequiredString(source.reconciliation_digest, 512) ?? undefined;
	if (
		followUpSourceTotal === undefined ||
		worthSourceTotal === undefined ||
		rawSourceTotal === undefined ||
		visibleSourceTotal === undefined ||
		duplicateHiddenTotal === undefined ||
		rawScannedTotal === undefined ||
		visiblePageTotal === undefined ||
		duplicateHiddenPageTotal === undefined ||
		digest === undefined
	) return null;
	if (source.status === 'unavailable') {
		if (
			!CROSS_LANE_UNAVAILABLE_REASONS.has(
				source.reason as CrossLaneReconciliationUnavailableReason
			) ||
			followUpSourceTotal !== null ||
			worthSourceTotal !== null ||
			rawSourceTotal !== null ||
			visibleSourceTotal !== null ||
			duplicateHiddenTotal !== null ||
			rawScannedTotal !== null ||
			visiblePageTotal !== null ||
			duplicateHiddenPageTotal !== null ||
			digest !== null
		) return null;
		return {
			schema_version: 1,
			status: 'unavailable',
			reason: source.reason as CrossLaneReconciliationUnavailableReason,
			authoritative_lane: 'follow_up',
			principal,
			workspace,
			follow_up_source_total: followUpSourceTotal,
			worth_a_look_source_total: worthSourceTotal,
			raw_source_total: rawSourceTotal,
			visible_source_total: visibleSourceTotal,
			duplicate_hidden_total: duplicateHiddenTotal,
			raw_scanned_total: rawScannedTotal,
			visible_page_total: visiblePageTotal,
			duplicate_hidden_page_total: duplicateHiddenPageTotal,
			reconciliation_digest: digest
		};
	}
	if (
		source.status !== 'succeeded' ||
		source.reason !== null ||
		followUpSourceTotal === null ||
		worthSourceTotal === null ||
		rawSourceTotal === null ||
		visibleSourceTotal === null ||
		duplicateHiddenTotal === null ||
		rawScannedTotal === null ||
		visiblePageTotal === null ||
		duplicateHiddenPageTotal === null ||
		digest === null ||
		rawSourceTotal !== worthSourceTotal ||
		rawSourceTotal !== visibleSourceTotal + duplicateHiddenTotal ||
		rawScannedTotal !== visiblePageTotal + duplicateHiddenPageTotal
	) return null;
	return {
		schema_version: 1,
		status: 'succeeded',
		reason: null,
		authoritative_lane: 'follow_up',
		principal,
		workspace,
		follow_up_source_total: followUpSourceTotal,
		worth_a_look_source_total: worthSourceTotal,
		raw_source_total: rawSourceTotal,
		visible_source_total: visibleSourceTotal,
		duplicate_hidden_total: duplicateHiddenTotal,
		raw_scanned_total: rawScannedTotal,
		visible_page_total: visiblePageTotal,
		duplicate_hidden_page_total: duplicateHiddenPageTotal,
		reconciliation_digest: digest
	};
}

function asStringList(value: unknown): string[] {
	if (!Array.isArray(value)) return [];
	return value
		.filter((entry): entry is string => typeof entry === 'string')
		.map((entry) => entry.trim())
		.filter(Boolean);
}

function isActionKind(value: unknown): value is ResurfacingActionKind {
	return (
		typeof value === 'string' &&
		(RESURFACING_ACTION_KINDS as readonly string[]).includes(value)
	);
}

function mapBrief(value: unknown): ResurfacingBrief | null {
	const record = asRecord(value);
	if (!record) return null;
	const changes = Array.isArray(record.changes)
		? record.changes.flatMap((entry): ResurfacingChangeFact[] => {
				const change = asRecord(entry);
				if (!change) return [];
				const aspect = asString(change.aspect).trim();
				const before = asNullableString(change.before);
				const after = asNullableString(change.after);
				const effectiveText = asNullableString(change.effective_text);
				if (!aspect && !before && !after && !effectiveText) return [];
				return [{ aspect, before, after, effective_text: effectiveText }];
			})
		: [];
	const temporalFacts = Array.isArray(record.temporal_facts)
		? record.temporal_facts.flatMap((entry): ResurfacingTemporalFact[] => {
				const fact = asRecord(entry);
				if (!fact) return [];
				const text = asString(fact.text).trim();
				if (!text) return [];
				return [
					{
						kind: asString(fact.kind).trim() || 'date',
						text,
						at_ms: asOptionalMs(fact.at_ms),
						timezone: asNullableString(fact.timezone)
					}
				];
			})
		: [];
	const rawStatus = asString(record.detail_status);
	const detailStatus: ResurfacingDetailStatus =
		rawStatus === 'complete' || rawStatus === 'source_omits_details' ? rawStatus : 'partial';
	return {
		schema_version: asNonNegativeInt(record.schema_version),
		key_facts: asStringList(record.key_facts),
		changes,
		temporal_facts: temporalFacts,
		detail_status: detailStatus,
		missing_details: asStringList(record.missing_details)
	};
}

function mapCapability(value: unknown): ResurfacingActionCapability | null {
	const record = asRecord(value);
	if (!record || !isActionKind(record.kind)) return null;
	const rawSideEffect = asString(record.side_effect);
	const sideEffect: ResurfacingSideEffect = [
		'creates_task',
		'creates_reminder',
		'creates_share_draft',
		'creates_memory_candidate'
	].includes(rawSideEffect)
		? (rawSideEffect as ResurfacingSideEffect)
		: 'none';
	return {
		kind: record.kind,
		label: asString(record.label).trim() || actionLabel(record.kind),
		requires_input: asBool(record.requires_input),
		side_effect: sideEffect
	};
}

function mapCapabilities(value: unknown): ResurfacingActionCapability[] {
	if (!Array.isArray(value)) return [];
	const seen = new Set<ResurfacingActionKind>();
	const result: ResurfacingActionCapability[] = [];
	for (const raw of value) {
		const capability = mapCapability(raw);
		if (!capability || seen.has(capability.kind)) continue;
		seen.add(capability.kind);
		result.push(capability);
	}
	return result;
}

function mapRecommendation(value: unknown): ResurfacingRecommendation | null {
	const record = asRecord(value);
	if (!record || !isActionKind(record.kind)) return null;
	const source = asString(record.source);
	if (source !== 'curator' && source !== 'deterministic') return null;
	return {
		kind: record.kind,
		label: asString(record.label).trim() || actionLabel(record.kind),
		rationale: asString(record.rationale).trim(),
		confidence: Math.min(1, Math.max(0, asNum(record.confidence))),
		content_revision: asNullableString(record.content_revision),
		source,
	};
}

function mapSourceStatus(value: unknown): ResurfacingSourceStatus {
	const status = asString(value);
	if (
		[
			'available',
			'newer_available',
			'stale',
			'offline',
			'deleted',
			'suppressed',
			'unsupported',
			'unavailable'
		].includes(status)
	) {
		return status as ResurfacingSourceStatus;
	}
	return 'unavailable';
}

function mapSource(value: unknown): ResurfacingSourceMetadata | null {
	const record = asRecord(value);
	if (!record) return null;
	switch (record.kind) {
		case 'comm':
			return {
				kind: 'comm',
				provider: asString(record.provider),
				account_alias: asString(record.account_alias),
				account_email: asNullableString(record.account_email),
				thread_id: asString(record.thread_id),
				message_id: asString(record.message_id),
				received_at: asNum(record.received_at),
				evidence_message_ids: asStringList(record.evidence_message_ids)
			};
		case 'task':
			return {
				kind: 'task',
				task_id: asString(record.task_id),
				status: asString(record.status),
				outcome: asNullableString(record.outcome),
				updated_at: asString(record.updated_at)
			};
		case 'memory':
			return {
				kind: 'memory',
				tier: asString(record.tier),
				key: asString(record.key),
				updated_at: asNullableString(record.updated_at)
			};
		case 'web':
			return {
				kind: 'web',
				url: asString(record.url)
			};
		default:
			return null;
	}
}

function mapEvidence(value: unknown): ResurfacingEvidenceMessage | null {
	const record = asRecord(value);
	if (!record) return null;
	const messageId = asString(record.message_id).trim();
	if (!messageId) return null;
	return {
		message_id: messageId,
		subject: asNullableString(record.subject),
		summary: asNullableString(record.summary),
		received_at: asNum(record.received_at),
		body: asNullableString(record.body),
		truncated: asBool(record.truncated),
		attachment_count: asNonNegativeInt(record.attachment_count)
	};
}

function mapOriginal(value: unknown): ResurfacingOriginalContent | null {
	const record = asRecord(value);
	if (!record) return null;
	switch (record.kind) {
		case 'comm':
			return {
				kind: 'comm',
				message_id: asString(record.message_id),
				subject: asNullableString(record.subject),
				summary: asNullableString(record.summary),
				received_at: asNum(record.received_at),
				body: asNullableString(record.body),
				evidence_messages: Array.isArray(record.evidence_messages)
					? record.evidence_messages.flatMap((entry) => {
							const evidence = mapEvidence(entry);
							return evidence ? [evidence] : [];
						})
					: []
			};
		case 'task':
			return {
				kind: 'task',
				task_id: asString(record.task_id),
				title: asString(record.title),
				status: asString(record.status),
				outcome: asNullableString(record.outcome)
			};
		case 'memory':
			return {
				kind: 'memory',
				tier: asString(record.tier),
				key: asString(record.key),
				summary: asString(record.summary),
				updated_at: asNullableString(record.updated_at)
			};
		case 'web':
			return {
				kind: 'web',
				url: asString(record.url),
				title: asString(record.title),
				summary: asString(record.summary)
			};
		default:
			return null;
	}
}

export function actionLabel(kind: ResurfacingActionKind): string {
	switch (kind) {
		case 'view_details': return 'Details';
		case 'open_source': return 'Open source';
		case 'show_original': return 'Original';
		case 'ask_presto': return 'Ask Presto';
		case 'create_task': return 'Create task';
		case 'create_reminder': return 'Create reminder';
		case 'share': return 'Share';
		case 'save_to_memory': return 'Save to memory';
		case 'summarize_deeper': return 'Summarize deeper';
	}
}

function emptyPage(): ResurfacingPage {
	return {
		cards: [],
		total: 0,
		limit: 0,
		offset: 0,
		has_more: false,
		next_cursor: null,
		health: null,
		actionability: null,
		actionability_training: null,
		grouping: null,
		routing: null,
		bandit: null,
		semantic_extraction: null,
		canonical_attention_projection: null,
		cross_lane_reconciliation: null,
		cross_lane_reconciliation_error: 'cross_lane_reconciliation_missing',
		semantic_ranking_enabled: false
	};
}

/** Pure list mapper. Invalid rows are dropped; invalid optional fields degrade. */
export function mapResurfacingPayload(
	json: unknown,
	routingOverride: AttentionRoutingPage | null = null
): ResurfacingCard[] {
	const root = asRecord(json);
	if (!root || !Array.isArray(root.cards)) return [];
	const out: ResurfacingCard[] = [];
	const groupingPage = parseAttentionGroupingPage(root);
	const parsedRouting = routingOverride ?? parseAttentionRoutingPage(root);
	const routingPage = parsedRouting?.decision.surface === 'worth_a_look' ? parsedRouting : null;
	for (const raw of root.cards) {
		const record = asRecord(raw);
		if (!record) continue;
		const candidateId = asString(record.candidate_id).trim();
		if (!candidateId) continue;
		const rank = parseChannelFollowUpLearningRank(record);
		const actionability = parseAttentionActionabilityCard(record);
		const grouping = parseAttentionGroupingMetadata(record.grouping);
		const hasRevision = Object.prototype.hasOwnProperty.call(record, 'source_revision');
		const sourceRevision =
			record.source_revision === null
				? null
				: typeof record.source_revision === 'string' && record.source_revision.trim()
					? record.source_revision.trim()
					: undefined;
		const decisionItem =
			routingPage && hasRevision && sourceRevision !== undefined
				? parseAttentionDecisionItem(record.decision_item, {
						decision_id: routingPage.decision.decision_id,
						candidate_id: candidateId,
						source_revision: sourceRevision,
						routing_mode: routingPage.decision.routing_mode,
						routing_snapshot_id: routingPage.decision.routing_snapshot_id
					})
				: null;
		const banditDecision = decisionItem
			? parseAttentionBanditDecision(record.bandit_decision, decisionItem)
			: null;
		out.push({
			candidate_id: candidateId,
			line: asString(record.line),
			why_now: asString(record.why_now),
			source_title: asString(record.source_title),
			summary: asString(record.summary),
			source_kind: asString(record.source_kind),
			source_ref: asString(record.source_ref),
			...(sourceRevision === null
				? { source_revision: null }
				: typeof sourceRevision === 'string'
					? { source_revision: sourceRevision }
					: {}),
			...(Object.prototype.hasOwnProperty.call(record, 'source_route')
				? { source_route: asNullableString(record.source_route) }
				: {}),
			...(Object.prototype.hasOwnProperty.call(record, 'open_url')
				? { open_url: asNullableString(record.open_url) }
				: {}),
			detail_label: asString(record.detail_label),
			temporal_anchor_at: asOptionalMs(record.temporal_anchor_at),
			brief: mapBrief(record.brief),
			brief_status: record.brief_status === 'v2' ? 'v2' : 'legacy',
			content_revision: asNullableString(record.content_revision),
			source_updated: asBool(record.source_updated),
			recommended_action: mapRecommendation(record.recommended_action),
			actions: mapCapabilities(record.actions),
			...(rank ?? {}),
			...(actionability ? { actionability } : {}),
			...(grouping ? { grouping } : {}),
			...(groupingPage ? { grouping_page: groupingPage } : {}),
			...(decisionItem ? { decision_item: decisionItem } : {}),
			...(banditDecision ? { bandit_decision: banditDecision } : {}),
			...(routingPage ? { routing_page: routingPage } : {})
		});
	}
	return out;
}

export function mapResurfacingPagePayload(
	json: unknown,
	expectedScope?: CanonicalAttentionProjectionScope
): ResurfacingPage {
	const record = asRecord(json);
	if (!record) return emptyPage();
	const mappedCards = mapResurfacingPayload(json);
	const reconciliation = parseResurfacingCrossLaneReconciliation(
		record.cross_lane_reconciliation,
		expectedScope
	);
	const rawTotal = strictCount(record.total);
	const reconciliationError = !reconciliation
		? 'cross_lane_reconciliation_malformed'
		: reconciliation.status === 'unavailable'
			? reconciliation.reason
			: reconciliation.visible_page_total !== mappedCards.length ||
				rawTotal === null || rawTotal !== reconciliation.visible_source_total
				? 'cross_lane_reconciliation_count_mismatch'
				: null;
	const cards = reconciliationError === null ? mappedCards : [];
	const health = parseChannelFollowUpLearningHealth(record.health);
	const cursor = asRecord(record.next_cursor);
	let nextCursor: ResurfacingCursor | null = null;
	if (cursor) {
		const candidateId = asString(cursor.candidate_id).trim();
		const surfacedAt = asNum(cursor.surfaced_at, NaN);
		const score = asNum(cursor.score, NaN);
		if (candidateId && Number.isFinite(surfacedAt) && Number.isFinite(score)) {
			nextCursor = { surfaced_at: surfacedAt, score, candidate_id: candidateId };
		}
	}
	const parsedRouting = parseAttentionRoutingPage(record);
	const routing =
		parsedRouting?.decision.surface === 'worth_a_look' ||
		parsedRouting?.decision.complete_cross_lane_universe
			? parsedRouting
			: null;
	return {
		cards,
		total: asNonNegativeInt(record.total),
		limit: asNonNegativeInt(record.limit),
		offset: asNonNegativeInt(record.offset),
		has_more: asBool(record.has_more),
		next_cursor: nextCursor,
		health,
		actionability: parseAttentionActionabilityPage(record),
		actionability_training: parseAttentionActionabilityTraining(record),
		grouping: parseAttentionGroupingPage(record),
		routing,
		bandit: parseAttentionBanditHealth(record),
		semantic_extraction: parseAttentionSemanticExtractionHealth(record),
		canonical_attention_projection: canonicalAttentionProjectionFromResponse(record),
		cross_lane_reconciliation: reconciliation,
		cross_lane_reconciliation_error: reconciliationError,
		semantic_ranking_enabled:
			asBool(record.semantic_ranking_enabled) || health?.semantic_ranking_enabled === true
	};
}

/** Pure read mapper shared by Detail and Original endpoints. */
export function mapResurfacingDetailPayload(json: unknown): ResurfacingDetail | null {
	const record = asRecord(json);
	if (!record) return null;
	const candidateId = asString(record.candidate_id).trim();
	if (!candidateId) return null;
	return {
		candidate_id: candidateId,
		source_kind: asString(record.source_kind),
		status: mapSourceStatus(record.status),
		title: asNullableString(record.title),
		summary: asNullableString(record.summary),
		brief: mapBrief(record.brief),
		content_revision: asNullableString(record.content_revision),
		source_revision: asNullableString(record.source_revision),
		source_updated: asBool(record.source_updated),
		has_newer: asBool(record.has_newer),
		source_route: asNullableString(record.source_route),
		open_url: asNullableString(record.open_url),
		source: mapSource(record.source),
		recommended_action: mapRecommendation(record.recommended_action),
		actions: mapCapabilities(record.actions),
		original: mapOriginal(record.original),
		temporal_anchor_at: asOptionalMs(record.temporal_anchor_at)
	};
}

function mapContextualResult(value: unknown): ResurfacingContextualActionResult | null {
	const record = asRecord(value);
	if (!record) return null;
	switch (record.kind) {
		case 'task':
			return { kind: 'task', task_id: asString(record.task_id), route: asString(record.route) };
		case 'reminder':
			return {
				kind: 'reminder',
				reminder_id: asNullableString(record.reminder_id),
				provider: asNullableString(record.provider),
				app_url: asNullableString(record.app_url),
				task_id: asNullableString(record.task_id),
				route: asNullableString(record.route),
				at: asString(record.at),
				timezone: asNullableString(record.timezone)
			};
		case 'memory_candidate':
			return {
				kind: 'memory_candidate',
				learning_candidate_id: asString(record.learning_candidate_id),
				route: asString(record.route),
				review_required: asBool(record.review_required)
			};
		case 'ask_presto': {
			const context = asRecord(record.context);
			if (!context) return null;
			return {
				kind: 'ask_presto',
				route: asString(record.route),
				context: {
					context_type: asString(context.context_type),
					candidate_id: asString(context.candidate_id)
				}
			};
		}
		case 'share_draft':
			return {
				kind: 'share_draft',
				task_id: asString(record.task_id),
				route: asString(record.route),
				approval_required: asBool(record.approval_required)
			};
		case 'deeper_summary':
			return {
				kind: 'deeper_summary',
				summary: asString(record.summary),
				key_points: asStringList(record.key_points),
				recommended_actions: asStringList(record.recommended_actions),
				caveats: asStringList(record.caveats)
			};
		default:
			return null;
	}
}

function mapContextualResponse(json: unknown): ResurfacingContextualActionResponse | null {
	const record = asRecord(json);
	const result = record ? mapContextualResult(record.result) : null;
	if (!record || !result || !isActionKind(record.action)) return null;
	const candidateId = asString(record.candidate_id).trim();
	if (!candidateId) return null;
	return {
		candidate_id: candidateId,
		action: record.action,
		result_ref: asString(record.result_ref),
		replayed: asBool(record.replayed),
		result
	};
}

const RESURFACING_BASE_ENDPOINT = '/api/magician/v2/channel-assist/resurfacing';
const RESURFACING_TODAY_ENDPOINT = `${RESURFACING_BASE_ENDPOINT}/today`;

async function responseError(response: Response, fallback: string): Promise<ResurfacingApiError> {
	const body: unknown = await response.json().catch(() => null);
	const record = asRecord(body);
	const message = asString(record?.error).trim() || fallback;
	const code = asNullableString(record?.error_code) ?? asNullableString(record?.code);
	return new ResurfacingApiError(message, response.status, code);
}

async function getJson(url: string): Promise<unknown> {
	const response = await fetch(url);
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
	return response.json();
}

export interface FetchResurfacingTodayOptions {
	limit?: number;
	offset?: number;
	cursor?: ResurfacingCursor | null;
}

/** Today's resurfacing page. Throws on transport/server failures. */
export async function fetchResurfacingTodayPage(
	options: FetchResurfacingTodayOptions = {}
): Promise<ResurfacingPage> {
	const params = new URLSearchParams();
	const scope = getCurrentScopeIdentity();
	if (typeof options.limit === 'number' && Number.isFinite(options.limit)) {
		params.set('limit', String(Math.max(1, Math.floor(options.limit))));
	}
	if (options.cursor) {
		params.set('cursor_surfaced_at', String(options.cursor.surfaced_at));
		params.set('cursor_score', String(options.cursor.score));
		params.set('cursor_candidate_id', options.cursor.candidate_id);
	} else if (typeof options.offset === 'number' && Number.isFinite(options.offset)) {
		params.set('offset', String(Math.max(0, Math.floor(options.offset))));
	}
	const url = params.size
		? `${RESURFACING_TODAY_ENDPOINT}?${params.toString()}`
		: RESURFACING_TODAY_ENDPOINT;
	try {
		const response = await fetch(url, {
			headers: {
			}
		});
		const body: unknown = await response.json().catch(() => null);
		if (!response.ok) {
			const unavailable = parseResurfacingCrossLaneReconciliation(
				asRecord(body)?.cross_lane_reconciliation,
				scope
			);
			const reason = unavailable?.status === 'unavailable'
				? unavailable.reason
				: 'cross_lane_reconciliation_unavailable';
			throw new ResurfacingApiError(reason, response.status, 'cross_lane_reconciliation_unavailable');
		}
		const page = mapResurfacingPagePayload(body, scope);
		if (page.cross_lane_reconciliation_error ||
			page.cross_lane_reconciliation?.status !== 'succeeded') {
			throw new ResurfacingApiError(
				page.cross_lane_reconciliation_error ?? 'cross_lane_reconciliation_unavailable',
				502,
				'cross_lane_reconciliation_unavailable'
			);
		}
		return page;
	} catch (error) {
		if (error instanceof ResurfacingApiError) {
			throw new ResurfacingApiError(`Worth a Look unavailable: ${error.message}`, error.status, error.code);
		}
		throw error;
	}
}

export interface ResurfacingGroupMembersResult {
	items: ResurfacingCard[];
	total: number;
	cluster: AttentionGroupingMetadata | null;
}

/** Full evidence expansion for one Worth-a-look group. */
export async function fetchResurfacingGroupMembers(
	clusterId: string,
	routingPage: AttentionRoutingPage | null = null
): Promise<ResurfacingGroupMembersResult> {
	const scope = getCurrentScopeIdentity();
	const params = new URLSearchParams();
	const response = await fetch(
		`${RESURFACING_BASE_ENDPOINT}/groups/${encodeURIComponent(clusterId)}/members?${params.toString()}`,
		{
			headers: {
			}
		}
	);
	const body: unknown = await response.json().catch(() => null);
	const reconciliation = parseResurfacingCrossLaneReconciliation(
		asRecord(body)?.cross_lane_reconciliation,
		scope
	);
	if (!response.ok || reconciliation?.status !== 'succeeded') {
		const reason = reconciliation?.status === 'unavailable'
			? reconciliation.reason
			: 'cross_lane_reconciliation_unavailable';
		throw new ResurfacingApiError(reason, response.ok ? 502 : response.status, 'cross_lane_reconciliation_unavailable');
	}
	const record = asRecord(body);
	if (!record || !Array.isArray(record.items)) {
		throw new ResurfacingApiError('Malformed resurfacing group response', 502);
	}
	const items = mapResurfacingPayload({ cards: record.items }, routingPage);
	const total = strictCount(record.total);
	const cluster = asRecord(record.cluster);
	const parsedCluster = parseAttentionGroupingMetadata(cluster?.grouping ?? cluster);
	if (total === null || total !== record.items.length || items.length !== total || !parsedCluster) {
		throw new ResurfacingApiError('Malformed resurfacing group response', 502);
	}
	return {
		items,
		total,
		cluster: parsedCluster
	};
}

export async function fetchResurfacingDetail(candidateId: string): Promise<ResurfacingDetail> {
	const payload = await getJson(
		`${RESURFACING_BASE_ENDPOINT}/${encodeURIComponent(candidateId)}/detail`
	);
	const detail = mapResurfacingDetailPayload(payload);
	if (!detail) throw new ResurfacingApiError('Malformed resurfacing detail response', 502);
	return detail;
}

export async function fetchResurfacingOriginal(candidateId: string): Promise<ResurfacingDetail> {
	const payload = await getJson(
		`${RESURFACING_BASE_ENDPOINT}/${encodeURIComponent(candidateId)}/original`
	);
	const detail = mapResurfacingDetailPayload(payload);
	if (!detail) throw new ResurfacingApiError('Malformed resurfacing original response', 502);
	return detail;
}

export type ResurfacingActionResult =
	| { ok: true; feedbackReceipt: AttentionFeedbackReceipt | null }
	| { ok: false; error: string };

/** Legacy explicit useful / acknowledge / dismiss feedback. */
export async function postResurfacingAction(
	candidateId: string,
	action: ResurfacingAction,
	reason?: ResurfacingDismissReason,
	attribution?: AttentionFeedbackAttribution | null
): Promise<ResurfacingActionResult> {
	try {
		const canonicalCandidateId = `worth_a_look:${candidateId}`;
		const exactAttribution =
			attribution &&
			(attribution.candidate_id === candidateId ||
				attribution.candidate_id === canonicalCandidateId)
				? attribution
				: null;
		const payload = {
			action,
			...(reason && action === 'dismiss' ? { reason } : {}),
			...(exactAttribution ? { attribution: exactAttribution } : {})
		};
		const response = await fetch(
			`${RESURFACING_BASE_ENDPOINT}/${encodeURIComponent(candidateId)}/action`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				// Attribution is optional. Explicit actions still succeed if there was
				// no verified impression; acknowledge remains a neutral outcome.
				body: JSON.stringify(payload)
			}
		);
		if (!response.ok) {
			const error = await responseError(response, `HTTP ${response.status}`);
			return { ok: false, error: error.message };
		}
		const body: unknown = await response.json().catch(() => null);
		const record = asRecord(body);
		const feedbackReceipt = parseAttentionFeedbackReceipt(record?.feedback_receipt);
		attentionRankRecomputeStore.track(feedbackReceipt, {
			raw_candidate_id: candidateId,
			...(exactAttribution && Object.prototype.hasOwnProperty.call(exactAttribution, 'source_revision')
				? { source_revision: exactAttribution.source_revision }
				: {}),
			attribution: exactAttribution
		});
		return {
			ok: true,
			feedbackReceipt
		};
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

/**
 * Run a contextual action against a message follow-up.
 *
 * Same contract as the Worth-a-look lane — the server shares one action path
 * across both — so the response mapping and error handling are reused verbatim
 * rather than duplicated per surface.
 */
export async function postChannelFollowUpContextualAction(
	annotationId: string,
	request: ResurfacingContextualActionRequest
): Promise<ResurfacingContextualActionResponse> {
	const response = await fetch(
		`/api/magician/v2/channel-assist/follow-ups/${encodeURIComponent(annotationId)}/actions`,
		{
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ ...request, input: request.input ?? {} })
		}
	);
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
	const mapped = mapContextualResponse(await response.json());
	if (!mapped) throw new ResurfacingApiError('Malformed follow-up action response', 502);
	if (mapped.action !== request.kind) {
		throw new ResurfacingApiError('Mismatched follow-up action response', 502);
	}
	return mapped;
}

export async function postResurfacingContextualAction(
	candidateId: string,
	request: ResurfacingContextualActionRequest
): Promise<ResurfacingContextualActionResponse> {
	const response = await fetch(
		`${RESURFACING_BASE_ENDPOINT}/${encodeURIComponent(candidateId)}/actions`,
		{
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ ...request, input: request.input ?? {} })
		}
	);
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
	const mapped = mapContextualResponse(await response.json());
	if (!mapped) throw new ResurfacingApiError('Malformed resurfacing action response', 502);
	if (mapped.candidate_id !== candidateId || mapped.action !== request.kind) {
		throw new ResurfacingApiError('Mismatched resurfacing action response', 502);
	}
	return mapped;
}

export async function postResurfacingRecommendationEvent(
	candidateId: string,
	request: {
		kind: ResurfacingActionKind;
		content_revision: string | null;
		event: ResurfacingRecommendationEvent;
	}
): Promise<{ recorded: boolean }> {
	const response = await fetch(
		`${RESURFACING_BASE_ENDPOINT}/${encodeURIComponent(candidateId)}/recommendation-event`,
		{
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify(request)
		}
	);
	if (!response.ok) throw await responseError(response, `HTTP ${response.status}`);
	const record = asRecord(await response.json());
	return { recorded: asBool(record?.recorded) };
}
