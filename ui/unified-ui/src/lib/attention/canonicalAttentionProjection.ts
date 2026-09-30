/**
 * Strict client boundary for the additive canonical Follow-up/Worth-a-look
 * projection.
 *
 * The projection is an all-or-nothing replacement for the two legacy lists.
 * A row is never salvaged from a malformed response: every field, every item,
 * and the complete-universe reconciliation must validate before callers may
 * render either destination lane. This is what prevents a partial rollout from
 * hiding an item that remains available through the legacy endpoints.
 */

import {
	parseAttentionActionabilityPage,
	parseChannelFollowUpLearningHealth,
	type AttentionActionabilityPage,
	type ChannelFollowUpLearningHealth
} from '$lib/channel/channelFollowUpLearning';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import {
	parseAttentionGroupingPage,
	type AttentionGroupingPage
} from '$lib/attention/attentionGrouping';
import {
	parseAttentionDecisionItem,
	parseAttentionRoutingPage,
	type AttentionDecisionItem,
	type AttentionRoutingPage
} from '$lib/attention/attentionRouting';
import {
	parseAttentionBanditHealth,
	type AttentionBanditHealth
} from '$lib/attention/attentionBandit';

export const CANONICAL_ATTENTION_PROJECTION_ENDPOINT =
	'/api/magician/v2/channel-assist/attention-learning/canonical-projection';

export type CanonicalAttentionOriginLane = 'follow_up' | 'worth_a_look';
export type CanonicalAttentionLane = CanonicalAttentionOriginLane | 'non_surfaced';
export type CanonicalAttentionProjectionStatus = 'succeeded' | 'baseline_fallback';
export type CanonicalAttentionPolicyMode = 'baseline' | 'shadow' | 'canary';
export type CanonicalAttentionActionKind =
	| 'open_source'
	| 'useful'
	| 'approve'
	| 'acknowledge'
	| 'dismiss'
	| 'snooze'
	| 'owner_work'
	| 'wrong_lane';

export interface CanonicalAttentionPolicy {
	mode: CanonicalAttentionPolicyMode;
	snapshot_id: string | null;
	model_version: string | null;
	seed_identity: string;
	canary_fraction: number;
}

export interface CanonicalAttentionIntegrity {
	load_complete: true;
	exact_once: true;
	source_total: number;
	follow_up_source_total: number;
	worth_a_look_source_total: number;
	reconciled_total: number;
	grouped_member_total: number;
	materialized_total: number;
	follow_up_lane_total: number;
	worth_a_look_lane_total: number;
	non_surfaced_total: number;
	duplicate_hidden_total: number;
	unmatched_total: 0;
	fallback_reason: string | null;
}

export interface CanonicalAttentionDuplicateAlias {
	owner_canonical_id: string;
	duplicate_canonical_id: string;
	reason: 'exact_source_identity';
}

export type CrossLaneReconciliationUnavailableReason =
	| 'follow_up_store_unavailable'
	| 'follow_up_load_unavailable'
	| 'follow_up_source_changed'
	| 'worth_a_look_load_unavailable'
	| 'malformed_worth_comm_source_ref'
	| 'canonical_projection_unavailable';

export interface CanonicalCrossLaneReconciliation {
	schema_version: 1;
	status: 'succeeded';
	reason: null;
	authoritative_lane: 'follow_up';
	principal: string;
	workspace: string;
	follow_up_source_total: number;
	worth_a_look_source_total: number;
	raw_source_total: number;
	unique_source_total: number;
	duplicate_hidden_total: number;
	alias_record_total: number;
	alias_records_returned: number;
	aliases_truncated: boolean;
	reconciliation_digest: string;
}

export interface CanonicalAttentionGroup {
	cluster_id: string;
	representative_id: string;
	member_ids: string[];
	member_count: number;
}

export interface CanonicalAttentionOriginAction {
	id: string;
	kind: CanonicalAttentionActionKind;
	label: string;
	method: 'get' | 'post';
	href: string;
	requires_confirmation: boolean;
}

export interface CanonicalFollowUpOrigin {
	kind: 'follow_up';
	annotation_id: string;
	provider: string;
	account_alias: string;
	thread_id: string;
}

export interface CanonicalWorthOrigin {
	kind: 'worth_a_look';
	candidate_id: string;
	source_kind: string;
	source_ref: string;
}

export interface CanonicalFollowUpPayload {
	kind: 'follow_up';
	annotation_id: string;
	subject: string | null;
	sender: string | null;
	summary: string | null;
	label: string | null;
	reason: string | null;
	received_at: number | null;
	due_text?: string | null;
	due_at?: number | null;
	open_url: string | null;
}

export interface CanonicalWorthPayload {
	kind: 'worth_a_look';
	candidate_id: string;
	line: string;
	why_now: string;
	summary: string;
	source_title: string;
	source_kind: string;
	source_ref: string;
	open_url: string | null;
	temporal_anchor_at: number | null;
	brief: Record<string, unknown> | null;
}

interface CanonicalAttentionItemBase {
	canonical_id: string;
	source_revision: string | null;
	origin_lane: CanonicalAttentionOriginLane;
	served_lane: CanonicalAttentionLane;
	learned_lane: CanonicalAttentionLane;
	route_reason: string;
	route_applied: boolean;
	group: CanonicalAttentionGroup;
	actions: CanonicalAttentionOriginAction[];
}

export interface CanonicalFollowUpItem extends CanonicalAttentionItemBase {
	origin_lane: 'follow_up';
	origin: CanonicalFollowUpOrigin;
	payload: CanonicalFollowUpPayload;
}

export interface CanonicalWorthItem extends CanonicalAttentionItemBase {
	origin_lane: 'worth_a_look';
	origin: CanonicalWorthOrigin;
	payload: CanonicalWorthPayload;
}

export type CanonicalAttentionItem = CanonicalFollowUpItem | CanonicalWorthItem;

export interface CanonicalAttentionProjection {
	schema_version: 1;
	status: CanonicalAttentionProjectionStatus;
	projection_id: string;
	universe_digest: string;
	source_generation_token: string | null;
	created_at: number;
	policy: CanonicalAttentionPolicy;
	integrity: CanonicalAttentionIntegrity;
	duplicate_aliases: CanonicalAttentionDuplicateAlias[];
	cross_lane_reconciliation: CanonicalCrossLaneReconciliation;
	/** Optional diagnostics never authorize or invalidate the canonical lanes.
	 * Each member is parsed independently so an older persisted projection can
	 * still provide every diagnostic it actually contains. */
	diagnostics: CanonicalAttentionDiagnostics | null;
	lanes: {
		follow_up: CanonicalAttentionItem[];
		worth_a_look: CanonicalAttentionItem[];
		non_surfaced: CanonicalAttentionItem[];
	};
}

export interface CanonicalAttentionDiagnostics {
	health: ChannelFollowUpLearningHealth | null;
	actionability: AttentionActionabilityPage | null;
	grouping: AttentionGroupingPage | null;
	routing: AttentionRoutingPage | null;
	bandit: AttentionBanditHealth | null;
	decision_items: AttentionDecisionItem[];
}

export type CanonicalAttentionProjectionParseResult =
	| { ok: true; projection: CanonicalAttentionProjection }
	| { ok: false; reason: string };

export interface CanonicalAttentionProjectionFetchResult {
	projection: CanonicalAttentionProjection | null;
	fallback_reason: string | null;
}

export interface CanonicalAttentionProjectionScope {
	principal: string;
	workspace: string;
}

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

/**
 * Every required key must be present, and nothing outside `keys` +
 * `optionalKeys` may appear. `optionalKeys` exists for fields the server omits
 * when empty: without it, a field that is absent half the time can only be
 * spelled as "always required" or "unknown", and either choice rejects a valid
 * payload.
 */
function hasExactKeys(
	source: Record<string, unknown>,
	keys: readonly string[],
	optionalKeys: readonly string[] = []
): boolean {
	if (!keys.every((key) => Object.prototype.hasOwnProperty.call(source, key))) return false;
	return Object.keys(source).every(
		(key) => keys.includes(key) || optionalKeys.includes(key)
	);
}

const FOLLOW_UP_ACTION_KINDS: readonly CanonicalAttentionActionKind[] = [
	'open_source',
	'approve',
	'acknowledge',
	'useful',
	'wrong_lane',
	'dismiss',
	'snooze'
];
const WORTH_ACTION_KINDS: readonly CanonicalAttentionActionKind[] = [
	'open_source',
	'useful',
	'owner_work',
	'acknowledge',
	'dismiss'
];
const ACTION_CONFIRMATION: Record<CanonicalAttentionActionKind, boolean> = {
	open_source: false,
	useful: false,
	approve: true,
	acknowledge: false,
	dismiss: true,
	snooze: false,
	owner_work: false,
	wrong_lane: false
};
const DUPLICATE_ALIAS_LIMIT = 100;
const CANONICAL_ID_LIMIT = 512;
const CROSS_LANE_RECONCILIATION_REASONS = new Set<CrossLaneReconciliationUnavailableReason>([
	'follow_up_store_unavailable',
	'follow_up_load_unavailable',
	'follow_up_source_changed',
	'worth_a_look_load_unavailable',
	'malformed_worth_comm_source_ref',
	'canonical_projection_unavailable'
]);

function string(value: unknown, allowEmpty = false): string | null {
	if (typeof value !== 'string') return null;
	return allowEmpty || value.trim().length > 0 ? value : null;
}

function boundedString(value: unknown, maxLength: number): string | null {
	const parsed = string(value);
	return parsed && parsed.length <= maxLength ? parsed : null;
}

/**
 * A nullable text field, where empty is a value rather than an absence.
 *
 * An email with no subject legitimately arrives as `''`. Collapsing that to
 * `undefined` made it indistinguishable from a missing field, so the item was
 * rejected — and because the projection parses all-or-nothing, a single
 * subject-less mail invalidated the whole thing and dropped every surface back
 * to its pre-delivery lane. `undefined` stays reserved for a genuinely absent
 * or wrong-typed field.
 */
function nullableString(value: unknown): string | null | undefined {
	if (value === null) return null;
	if (typeof value !== 'string') return undefined;
	return value;
}

function finiteNumber(value: unknown): number | null {
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function nullableFiniteNumber(value: unknown): number | null | undefined {
	if (value === null) return null;
	const parsed = finiteNumber(value);
	return parsed === null ? undefined : parsed;
}

function count(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function isLane(value: unknown): value is CanonicalAttentionLane {
	return value === 'follow_up' || value === 'worth_a_look' || value === 'non_surfaced';
}

function parsePolicy(value: unknown): CanonicalAttentionPolicy | null {
	const source = record(value);
	if (!source || !hasExactKeys(source, [
		'mode', 'snapshot_id', 'model_version', 'seed_identity', 'canary_fraction'
	])) return null;
	const mode = source.mode;
	const snapshotId = nullableString(source.snapshot_id);
	const modelVersion = nullableString(source.model_version);
	const seedIdentity = string(source.seed_identity);
	const canaryFraction = finiteNumber(source.canary_fraction);
	if (
		(mode !== 'baseline' && mode !== 'shadow' && mode !== 'canary') ||
		snapshotId === undefined ||
		modelVersion === undefined ||
		!seedIdentity ||
		canaryFraction === null ||
		canaryFraction < 0 ||
		canaryFraction > 1
	) return null;
	return {
		mode,
		snapshot_id: snapshotId,
		model_version: modelVersion,
		seed_identity: seedIdentity,
		canary_fraction: canaryFraction
	};
}

function parseIntegrity(value: unknown): CanonicalAttentionIntegrity | null {
	const source = record(value);
	if (!source || !hasExactKeys(source, [
		'load_complete', 'exact_once', 'source_total', 'follow_up_source_total',
		'worth_a_look_source_total', 'reconciled_total', 'grouped_member_total',
		'materialized_total', 'follow_up_lane_total', 'worth_a_look_lane_total',
		'non_surfaced_total', 'duplicate_hidden_total', 'unmatched_total', 'fallback_reason'
	]) || source.load_complete !== true || source.exact_once !== true) return null;
	const sourceTotal = count(source.source_total);
	const followUpSourceTotal = count(source.follow_up_source_total);
	const worthSourceTotal = count(source.worth_a_look_source_total);
	const reconciledTotal = count(source.reconciled_total);
	const groupedMemberTotal = count(source.grouped_member_total);
	const materializedTotal = count(source.materialized_total);
	const followUpLaneTotal = count(source.follow_up_lane_total);
	const worthLaneTotal = count(source.worth_a_look_lane_total);
	const nonSurfacedTotal = count(source.non_surfaced_total);
	const duplicateHiddenTotal = count(source.duplicate_hidden_total);
	const unmatchedTotal = count(source.unmatched_total);
	const fallbackReason = nullableString(source.fallback_reason);
	if (
		sourceTotal === null ||
		followUpSourceTotal === null ||
		worthSourceTotal === null ||
		reconciledTotal === null ||
		groupedMemberTotal === null ||
		materializedTotal === null ||
		followUpLaneTotal === null ||
		worthLaneTotal === null ||
		nonSurfacedTotal === null ||
		duplicateHiddenTotal === null ||
		unmatchedTotal !== 0 ||
		fallbackReason === undefined ||
		sourceTotal !== followUpSourceTotal + worthSourceTotal ||
		reconciledTotal !== sourceTotal ||
		groupedMemberTotal + duplicateHiddenTotal !== sourceTotal ||
		materializedTotal + duplicateHiddenTotal !== sourceTotal ||
		materializedTotal !== followUpLaneTotal + worthLaneTotal + nonSurfacedTotal
	) return null;
	return {
		load_complete: true,
		exact_once: true,
		source_total: sourceTotal,
		follow_up_source_total: followUpSourceTotal,
		worth_a_look_source_total: worthSourceTotal,
		reconciled_total: reconciledTotal,
		grouped_member_total: groupedMemberTotal,
		materialized_total: materializedTotal,
		follow_up_lane_total: followUpLaneTotal,
		worth_a_look_lane_total: worthLaneTotal,
		non_surfaced_total: nonSurfacedTotal,
		duplicate_hidden_total: duplicateHiddenTotal,
		unmatched_total: 0,
		fallback_reason: fallbackReason
	};
}

function parseDuplicateAliases(value: unknown): CanonicalAttentionDuplicateAlias[] | null {
	if (!Array.isArray(value) || value.length > DUPLICATE_ALIAS_LIMIT) return null;
	const aliases: CanonicalAttentionDuplicateAlias[] = [];
	let previousKey: string | null = null;
	const duplicateIds = new Set<string>();
	for (const raw of value) {
		const source = record(raw);
		if (!source || !hasExactKeys(source, [
			'owner_canonical_id', 'duplicate_canonical_id', 'reason'
		])) return null;
		const ownerCanonicalId = boundedString(source.owner_canonical_id, CANONICAL_ID_LIMIT);
		const duplicateCanonicalId = boundedString(source.duplicate_canonical_id, CANONICAL_ID_LIMIT);
		const orderingKey = ownerCanonicalId && duplicateCanonicalId
			? `${ownerCanonicalId}\u0000${duplicateCanonicalId}`
			: null;
		if (
			!ownerCanonicalId ||
			!duplicateCanonicalId ||
			!orderingKey ||
			source.reason !== 'exact_source_identity' ||
			!ownerCanonicalId.startsWith('follow_up:') ||
			!duplicateCanonicalId.startsWith('worth_a_look:') ||
			ownerCanonicalId === duplicateCanonicalId ||
			duplicateIds.has(duplicateCanonicalId) ||
			(previousKey !== null && orderingKey <= previousKey)
		) return null;
		previousKey = orderingKey;
		duplicateIds.add(duplicateCanonicalId);
		aliases.push({
			owner_canonical_id: ownerCanonicalId,
			duplicate_canonical_id: duplicateCanonicalId,
			reason: 'exact_source_identity'
		});
	}
	const ownerIds = new Set(aliases.map((alias) => alias.owner_canonical_id));
	if (aliases.some((alias) => ownerIds.has(alias.duplicate_canonical_id))) return null;
	return aliases;
}

function parseCanonicalCrossLaneReconciliation(
	value: unknown,
	expectedScope?: CanonicalAttentionProjectionScope
): CanonicalCrossLaneReconciliation | 'unavailable' | null {
	const source = record(value);
	if (!source || !hasExactKeys(source, [
		'schema_version', 'status', 'reason', 'authoritative_lane', 'principal', 'workspace',
		'follow_up_source_total', 'worth_a_look_source_total', 'raw_source_total',
		'unique_source_total', 'duplicate_hidden_total', 'alias_record_total',
		'alias_records_returned', 'aliases_truncated', 'reconciliation_digest'
	])) return null;
	if (source.schema_version !== 1 || source.authoritative_lane !== 'follow_up') return null;
	const principal = boundedString(source.principal, 256);
	const workspace = boundedString(source.workspace, 256);
	if (!principal || !workspace ||
		(expectedScope && (principal !== expectedScope.principal || workspace !== expectedScope.workspace))) {
		return null;
	}
	if (source.status === 'unavailable') {
		const nullableCounts = [
			source.follow_up_source_total,
			source.worth_a_look_source_total,
			source.raw_source_total,
			source.unique_source_total,
			source.duplicate_hidden_total,
			source.alias_record_total,
			source.alias_records_returned
		];
		return CROSS_LANE_RECONCILIATION_REASONS.has(
			source.reason as CrossLaneReconciliationUnavailableReason
		) &&
			nullableCounts.every((value) => value === null || count(value) !== null) &&
			typeof source.aliases_truncated === 'boolean' &&
			(source.reconciliation_digest === null ||
				boundedString(source.reconciliation_digest, 512) !== null)
			? 'unavailable'
			: null;
	}
	if (source.status !== 'succeeded' || source.reason !== null) return null;
	const followUpSourceTotal = count(source.follow_up_source_total);
	const worthSourceTotal = count(source.worth_a_look_source_total);
	const rawSourceTotal = count(source.raw_source_total);
	const uniqueSourceTotal = count(source.unique_source_total);
	const duplicateHiddenTotal = count(source.duplicate_hidden_total);
	const aliasRecordTotal = count(source.alias_record_total);
	const aliasRecordsReturned = count(source.alias_records_returned);
	const digest = boundedString(source.reconciliation_digest, 512);
	if (
		followUpSourceTotal === null ||
		worthSourceTotal === null ||
		rawSourceTotal === null ||
		uniqueSourceTotal === null ||
		duplicateHiddenTotal === null ||
		aliasRecordTotal === null ||
		aliasRecordsReturned === null ||
		typeof source.aliases_truncated !== 'boolean' ||
		!digest ||
		rawSourceTotal !== followUpSourceTotal + worthSourceTotal ||
		uniqueSourceTotal + duplicateHiddenTotal !== rawSourceTotal ||
		aliasRecordTotal !== duplicateHiddenTotal ||
		aliasRecordsReturned > aliasRecordTotal ||
		aliasRecordsReturned > DUPLICATE_ALIAS_LIMIT ||
		source.aliases_truncated !== (aliasRecordsReturned < aliasRecordTotal)
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
		unique_source_total: uniqueSourceTotal,
		duplicate_hidden_total: duplicateHiddenTotal,
		alias_record_total: aliasRecordTotal,
		alias_records_returned: aliasRecordsReturned,
		aliases_truncated: source.aliases_truncated,
		reconciliation_digest: digest
	};
}

function parseGroup(value: unknown): CanonicalAttentionGroup | null {
	const source = record(value);
	if (!source || !hasExactKeys(source, [
		'cluster_id', 'representative_id', 'member_ids', 'member_count'
	]) || !Array.isArray(source.member_ids)) return null;
	const clusterId = string(source.cluster_id);
	const representativeId = string(source.representative_id);
	const memberCount = count(source.member_count);
	const memberIds = source.member_ids.map((value) => string(value));
	if (
		!clusterId ||
		!representativeId ||
		memberCount === null ||
		memberIds.some((value) => value === null) ||
		memberCount !== memberIds.length ||
		new Set(memberIds).size !== memberIds.length ||
		!memberIds.includes(representativeId)
	) return null;
	return {
		cluster_id: clusterId,
		representative_id: representativeId,
		member_ids: memberIds as string[],
		member_count: memberCount
	};
}

function parseActions(
	value: unknown,
	originLane: CanonicalAttentionOriginLane
): CanonicalAttentionOriginAction[] | null {
	if (!Array.isArray(value)) return null;
	const expectedKinds = originLane === 'follow_up' ? FOLLOW_UP_ACTION_KINDS : WORTH_ACTION_KINDS;
	const requiredKinds = originLane === 'follow_up'
		? (['open_source', 'approve', 'acknowledge', 'useful', 'dismiss', 'snooze'] as const)
		: (['open_source', 'useful', 'acknowledge', 'dismiss'] as const);
	if (value.length < requiredKinds.length) return null;
	const actions: CanonicalAttentionOriginAction[] = [];
	const ids = new Set<string>();
	const kinds = new Set<CanonicalAttentionActionKind>();
	for (const raw of value) {
		const source = record(raw);
		if (!source || !hasExactKeys(source, [
			'id', 'kind', 'label', 'method', 'href', 'requires_confirmation'
		])) return null;
		const id = string(source.id);
		const kind = source.kind;
		const label = string(source.label);
		const method = source.method;
		const href = string(source.href);
		if (
			!id ||
			!expectedKinds.includes(kind as CanonicalAttentionActionKind) ||
			!label ||
			(method !== 'get' && method !== 'post') ||
			!href ||
			typeof source.requires_confirmation !== 'boolean' ||
			id !== kind ||
			source.requires_confirmation !== ACTION_CONFIRMATION[kind as CanonicalAttentionActionKind] ||
			ids.has(id) ||
			kinds.has(kind as CanonicalAttentionActionKind) ||
			(originLane === 'worth_a_look' && (kind === 'approve' || kind === 'snooze')) ||
			(kind === 'open_source' ? method !== 'get' : method !== 'post')
		) return null;
		ids.add(id);
		kinds.add(kind as CanonicalAttentionActionKind);
		actions.push({
			id,
			kind: kind as CanonicalAttentionActionKind,
			label,
			method,
			href,
			requires_confirmation: source.requires_confirmation
		});
	}
	if (!requiredKinds.every((kind) => kinds.has(kind))) return null;
	return actions;
}

function parseItem(value: unknown, containingLane: CanonicalAttentionLane): CanonicalAttentionItem | null {
	const source = record(value);
	if (!source || !hasExactKeys(source, [
		'canonical_id', 'source_revision', 'origin_lane', 'served_lane', 'learned_lane',
		'route_reason', 'route_applied', 'origin', 'group', 'actions', 'payload'
	])) return null;
	const canonicalId = string(source.canonical_id);
	const sourceRevision = nullableString(source.source_revision);
	const originLane = source.origin_lane;
	const learnedLane = source.learned_lane;
	const routeReason = string(source.route_reason);
	const group = parseGroup(source.group);
	if (
		!canonicalId ||
		sourceRevision === undefined ||
		(originLane !== 'follow_up' && originLane !== 'worth_a_look') ||
		source.served_lane !== containingLane ||
		!isLane(learnedLane) ||
		routeReason === null ||
		typeof source.route_applied !== 'boolean' ||
		!group
	) return null;
	if (!group.member_ids.includes(canonicalId)) return null;
	const actions = parseActions(source.actions, originLane);
	const origin = record(source.origin);
	const payload = record(source.payload);
	if (!actions || !origin || !payload || origin.kind !== originLane || payload.kind !== originLane) {
		return null;
	}
	const base = {
		canonical_id: canonicalId,
		source_revision: sourceRevision,
		served_lane: containingLane,
		learned_lane: learnedLane,
		route_reason: routeReason,
		route_applied: source.route_applied,
		group,
		actions
	};
	if (originLane === 'follow_up') {
		if (!hasExactKeys(origin, [
			'kind', 'annotation_id', 'provider', 'account_alias', 'thread_id'
		]) || !hasExactKeys(payload, [
			'kind', 'annotation_id', 'subject', 'sender', 'summary', 'label', 'reason',
			'received_at', 'open_url'
		], ['due_text', 'due_at'])) return null;
		const annotationId = string(origin.annotation_id);
		const payloadAnnotationId = string(payload.annotation_id);
		const provider = string(origin.provider);
		const accountAlias = string(origin.account_alias);
		const threadId = string(origin.thread_id);
		const subject = nullableString(payload.subject);
		const sender = nullableString(payload.sender);
		const summary = nullableString(payload.summary);
		const label = nullableString(payload.label);
		const reason = nullableString(payload.reason);
		const receivedAt = nullableFiniteNumber(payload.received_at);
		const dueText = payload.due_text === undefined ? null : nullableString(payload.due_text);
		const dueAt = payload.due_at === undefined ? null : nullableFiniteNumber(payload.due_at);
		const openUrl = nullableString(payload.open_url);
		if (
			!annotationId ||
			payloadAnnotationId !== annotationId ||
			!provider ||
			!accountAlias ||
			!threadId ||
			subject === undefined ||
			sender === undefined ||
			summary === undefined ||
			label === undefined ||
			reason === undefined ||
			receivedAt === undefined ||
			dueText === undefined ||
			dueAt === undefined ||
			openUrl === undefined ||
			canonicalId !== `follow_up:${annotationId}`
		) return null;
		const actionBase = `/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}`;
		if (actions.some((action) =>
			action.kind === 'open_source'
				? action.href !== (openUrl ?? `${actionBase}/message`)
				: action.href !== `${actionBase}/${action.kind}`
		)) return null;
		return {
			...base,
			origin_lane: 'follow_up',
			origin: {
				kind: 'follow_up',
				annotation_id: annotationId,
				provider,
				account_alias: accountAlias,
				thread_id: threadId
			},
			payload: {
				kind: 'follow_up',
				annotation_id: payloadAnnotationId,
				subject,
				sender,
				summary,
				label,
				reason,
				received_at: receivedAt,
				due_text: dueText,
				due_at: dueAt,
				open_url: openUrl
			}
		};
	}
	if (!hasExactKeys(origin, ['kind', 'candidate_id', 'source_kind', 'source_ref']) ||
		!hasExactKeys(payload, [
			'kind', 'candidate_id', 'line', 'why_now', 'summary', 'source_title',
			'source_kind', 'source_ref', 'open_url', 'temporal_anchor_at', 'brief'
		])) return null;

	const candidateId = string(origin.candidate_id);
	const payloadCandidateId = string(payload.candidate_id);
	const sourceKind = string(origin.source_kind);
	const payloadSourceKind = string(payload.source_kind);
	const sourceRef = string(origin.source_ref);
	const payloadSourceRef = string(payload.source_ref);
	const line = string(payload.line, true);
	const whyNow = string(payload.why_now, true);
	const summary = string(payload.summary, true);
	const sourceTitle = string(payload.source_title, true);
	const openUrl = nullableString(payload.open_url);
	const temporalAnchorAt = nullableFiniteNumber(payload.temporal_anchor_at);
	const brief = payload.brief === null ? null : record(payload.brief);
	if (
		!candidateId ||
		payloadCandidateId !== candidateId ||
		!sourceKind ||
		payloadSourceKind !== sourceKind ||
		!sourceRef ||
		payloadSourceRef !== sourceRef ||
		line === null ||
		whyNow === null ||
		summary === null ||
		sourceTitle === null ||
		openUrl === undefined ||
		temporalAnchorAt === undefined ||
		(payload.brief !== null && !brief) ||
		canonicalId !== `worth_a_look:${candidateId}`
	) return null;
	const actionEndpoint =
		`/api/magician/v2/channel-assist/resurfacing/${encodeURIComponent(candidateId)}/action`;
	const detailEndpoint =
		`/api/magician/v2/channel-assist/resurfacing/${encodeURIComponent(candidateId)}/detail`;
	const sourceOpenUrl = /^https?:\/\//i.test(sourceRef) ? sourceRef : null;
	if (openUrl !== sourceOpenUrl) return null;
	if (actions.some((action) =>
		action.kind === 'open_source'
			? action.href !== (sourceOpenUrl ?? detailEndpoint)
			: action.href !== actionEndpoint
	)) return null;
	return {
		...base,
		origin_lane: 'worth_a_look',
		origin: {
			kind: 'worth_a_look',
			candidate_id: candidateId,
			source_kind: sourceKind,
			source_ref: sourceRef
		},
		payload: {
			kind: 'worth_a_look',
			candidate_id: payloadCandidateId,
			line,
			why_now: whyNow,
			summary,
			source_title: sourceTitle,
			source_kind: payloadSourceKind,
			source_ref: payloadSourceRef,
			open_url: openUrl,
			temporal_anchor_at: temporalAnchorAt,
			brief
		}
	};
}

/** Strict delivery-page seam. Delivery rows use the identical canonical item
 * contract and may not weaken it just because they arrive incrementally. */
export function parseCanonicalAttentionItem(
	value: unknown,
	containingLane: CanonicalAttentionLane
): CanonicalAttentionItem | null {
	return parseItem(value, containingLane);
}

function parseLane(value: unknown, lane: CanonicalAttentionLane): CanonicalAttentionItem[] | null {
	if (!Array.isArray(value)) return null;
	const items: CanonicalAttentionItem[] = [];
	for (const raw of value) {
		const item = parseItem(raw, lane);
		if (!item) return null;
		items.push(item);
	}
	return items;
}

function parseCanonicalAttentionDiagnostics(value: unknown): CanonicalAttentionDiagnostics | null {
	const input = record(value);
	if (!input) return null;
	const followUpHealth = record(input.follow_up_health);
	const diagnosticEnvelope = {
		actionability_mode: followUpHealth?.actionability_mode,
		actionability_snapshot_id: followUpHealth?.actionability_snapshot_id ?? null,
		semantic_extraction_coverage: followUpHealth?.semantic_extraction_coverage,
		actionability_scored_count: followUpHealth?.actionability_scored_count,
		actionability_fallback_count: followUpHealth?.actionability_fallback_count,
		grouping_mode: input.grouping_mode,
		grouping_snapshot_id: input.grouping_snapshot_id,
		grouping_generation: input.grouping_generation,
		grouping_scope: input.grouping_scope,
		grouping_health: input.grouping_health,
		decision: input.decision,
		impression_policy: input.impression_policy,
		routing_health: input.routing_health,
		bandit_health: input.bandit_health
	};
	const decisionItems = Array.isArray(input.decision_items)
		? input.decision_items.flatMap((raw) => {
				const item = parseAttentionDecisionItem(raw);
				return item ? [item] : [];
			})
		: [];
	return {
		health: parseChannelFollowUpLearningHealth(input.follow_up_health),
		actionability: parseAttentionActionabilityPage(diagnosticEnvelope),
		grouping: parseAttentionGroupingPage(diagnosticEnvelope),
		routing: parseAttentionRoutingPage(diagnosticEnvelope),
		bandit: parseAttentionBanditHealth(diagnosticEnvelope),
		decision_items: decisionItems
	};
}

export function canonicalItemDecisionItem(
	item: CanonicalAttentionItem,
	diagnostics: CanonicalAttentionDiagnostics | null | undefined
): AttentionDecisionItem | null {
	const items = diagnostics?.decision_items ?? [];
	if (items.length === 0) return null;
	const raw =
		item.origin.kind === 'follow_up' ? item.origin.annotation_id : item.origin.candidate_id;
	const aliases = new Set([
		item.canonical_id,
		raw,
		`follow_up:${raw}`,
		`worth_a_look:${raw}`
	]);
	return (
		items.find((candidate) => aliases.has(candidate.candidate_id) && candidate.selected) ??
		items.find((candidate) => aliases.has(candidate.candidate_id)) ??
		null
	);
}

/** Parse the projection object itself. No partial result is ever returned. */
export function parseCanonicalAttentionProjection(
	value: unknown,
	expectedScope?: CanonicalAttentionProjectionScope
): CanonicalAttentionProjectionParseResult {
	const source = record(value);
	if (!source) return { ok: false, reason: 'projection_missing' };
	if (!hasExactKeys(source, [
		'schema_version', 'status', 'projection_id', 'universe_digest', 'created_at',
		'policy', 'integrity', 'duplicate_aliases', 'cross_lane_reconciliation', 'lanes'
	// `diagnostics` carries the health the legacy lane endpoints read. It is
	// omitted when empty and is not consumed here, but it must be tolerated:
	// rejecting the envelope over it discards the whole canonical projection,
	// which silently falls the surface back to the pre-delivery lane.
	], ['diagnostics', 'source_generation_token'])) return { ok: false, reason: 'malformed_envelope' };
	if (source.schema_version !== 1) return { ok: false, reason: 'unsupported_schema' };
	if (source.status !== 'succeeded' && source.status !== 'baseline_fallback') {
		return { ok: false, reason: 'invalid_status' };
	}
	const projectionId = string(source.projection_id);
	const universeDigest = string(source.universe_digest);
	const sourceGenerationToken = source.source_generation_token === undefined ||
		source.source_generation_token === null
		? null
		: string(source.source_generation_token);
	const createdAt = finiteNumber(source.created_at);
	const policy = parsePolicy(source.policy);
	const integrity = parseIntegrity(source.integrity);
	const duplicateAliases = parseDuplicateAliases(source.duplicate_aliases);
	const reconciliation = parseCanonicalCrossLaneReconciliation(
		source.cross_lane_reconciliation,
		expectedScope
	);
	const lanes = record(source.lanes);
	if (reconciliation === 'unavailable') {
		return { ok: false, reason: 'cross_lane_reconciliation_unavailable' };
	}
	if (!projectionId || !universeDigest ||
		(source.source_generation_token !== undefined &&
			source.source_generation_token !== null &&
			!sourceGenerationToken) ||
		createdAt === null || !policy || !integrity ||
		!duplicateAliases || !reconciliation || !lanes) {
		return { ok: false, reason: 'malformed_envelope' };
	}
	if (!hasExactKeys(lanes, ['follow_up', 'worth_a_look', 'non_surfaced'])) {
		return { ok: false, reason: 'malformed_lane' };
	}
	const followUp = parseLane(lanes.follow_up, 'follow_up');
	const worth = parseLane(lanes.worth_a_look, 'worth_a_look');
	const nonSurfaced = parseLane(lanes.non_surfaced, 'non_surfaced');
	if (!followUp || !worth || !nonSurfaced) {
		return { ok: false, reason: 'malformed_lane' };
	}
	if (
		followUp.length !== integrity.follow_up_lane_total ||
		worth.length !== integrity.worth_a_look_lane_total ||
		nonSurfaced.length !== integrity.non_surfaced_total
	) return { ok: false, reason: 'lane_total_mismatch' };
	const all = [...followUp, ...worth, ...nonSurfaced];
	if (
		all.length !== integrity.materialized_total ||
		new Set(all.map((item) => item.canonical_id)).size !== all.length
	) return { ok: false, reason: 'exact_once_mismatch' };
	const allById = new Map(all.map((item) => [item.canonical_id, item]));
	const duplicateIds = new Set(duplicateAliases.map((alias) => alias.duplicate_canonical_id));
	if (
		reconciliation.follow_up_source_total !== integrity.follow_up_source_total ||
		reconciliation.worth_a_look_source_total !== integrity.worth_a_look_source_total ||
		reconciliation.raw_source_total !== integrity.source_total ||
		reconciliation.unique_source_total !== integrity.materialized_total ||
		reconciliation.unique_source_total !== integrity.grouped_member_total ||
		reconciliation.duplicate_hidden_total !== integrity.duplicate_hidden_total ||
		reconciliation.alias_records_returned !== duplicateAliases.length ||
		duplicateAliases.some((alias) => {
			const owner = allById.get(alias.owner_canonical_id);
			return !owner || owner.origin_lane !== 'follow_up' || allById.has(alias.duplicate_canonical_id);
		}) ||
		all.some((item) => item.group.member_ids.some((memberId) => duplicateIds.has(memberId)))
	) return { ok: false, reason: 'cross_lane_reconciliation_mismatch' };
	const clusterMembers = new Map<string, string[]>();
	const clusterRepresentatives = new Map<string, string>();
	for (const item of all) {
		const expectedMembers = clusterMembers.get(item.group.cluster_id);
		const expectedRepresentative = clusterRepresentatives.get(item.group.cluster_id);
		const members = item.group.member_ids;
		if (item.group.member_ids.some((memberId) => !allById.has(memberId))) {
			return { ok: false, reason: 'group_member_missing' };
		}
		if (expectedMembers && (
			expectedMembers.length !== members.length ||
			expectedMembers.some((memberId, index) => memberId !== members[index])
		)) return { ok: false, reason: 'group_membership_mismatch' };
		if (expectedRepresentative && expectedRepresentative !== item.group.representative_id) {
			return { ok: false, reason: 'group_representative_mismatch' };
		}
		clusterMembers.set(item.group.cluster_id, [...members]);
		clusterRepresentatives.set(item.group.cluster_id, item.group.representative_id);
		const representative = allById.get(item.group.representative_id);
		if (!representative || representative.group.cluster_id !== item.group.cluster_id) {
			return { ok: false, reason: 'group_representative_missing' };
		}
	}
	for (const [clusterId, members] of clusterMembers) {
		const materializedMembers = all.filter((item) => item.group.cluster_id === clusterId);
		if (materializedMembers.length !== members.length ||
			materializedMembers.some((item) => !members.includes(item.canonical_id))) {
			return { ok: false, reason: 'group_exact_once_mismatch' };
		}
	}
	const followUpOrigins = all.filter((item) => item.origin_lane === 'follow_up').length;
	const worthOrigins = all.length - followUpOrigins;
	if (followUpOrigins !== integrity.follow_up_source_total ||
		worthOrigins + integrity.duplicate_hidden_total !== integrity.worth_a_look_source_total) {
		return { ok: false, reason: 'origin_total_mismatch' };
	}
	const snapshotRoutingCannotApply = source.status === 'baseline_fallback' ||
		policy.mode === 'shadow' ||
		(policy.mode === 'canary' && policy.canary_fraction === 0);
	if ((source.status === 'succeeded') !== (integrity.fallback_reason === null) ||
		all.some((item) => {
			const knnApplied =
				item.route_reason === 'knn_demotion_applied' ||
				item.route_reason === 'knn_promotion_applied';
			if (item.route_applied) {
				return item.served_lane !== item.learned_lane ||
					item.learned_lane === item.origin_lane ||
					(!knnApplied && (snapshotRoutingCannotApply || policy.mode === 'baseline'));
			}
			return item.served_lane !== item.origin_lane;
		})) {
		return { ok: false, reason: 'routing_integrity_mismatch' };
	}
	return {
		ok: true,
		projection: {
			schema_version: 1,
			status: source.status,
			projection_id: projectionId,
			universe_digest: universeDigest,
			source_generation_token: sourceGenerationToken,
			created_at: createdAt,
			policy,
			integrity,
			duplicate_aliases: duplicateAliases,
			cross_lane_reconciliation: reconciliation,
			diagnostics: parseCanonicalAttentionDiagnostics(source.diagnostics),
			lanes: {
				follow_up: followUp,
				worth_a_look: worth,
				non_surfaced: nonSurfaced
			}
		}
	};
}

/** Extract the additive field from either legacy list response or the
 * standalone wrapper. Missing and malformed values intentionally collapse to
 * null so callers keep both legacy lanes together. */
export function canonicalAttentionProjectionFromResponse(
	value: unknown
): CanonicalAttentionProjection | null {
	const wrapper = record(value);
	if (!wrapper || !Object.prototype.hasOwnProperty.call(wrapper, 'canonical_attention_projection')) {
		return null;
	}
	const parsed = parseCanonicalAttentionProjection(wrapper.canonical_attention_projection);
	return parsed.ok ? parsed.projection : null;
}

function typedFallbackReason(value: unknown): string | null {
	const projection = record(value);
	const integrity = projection?.status === 'baseline_fallback' ? record(projection.integrity) : null;
	return integrity ? string(integrity.fallback_reason) : null;
}

export async function fetchCanonicalAttentionProjection(
	scope?: CanonicalAttentionProjectionScope,
	options: { signal?: AbortSignal } = {}
): Promise<CanonicalAttentionProjectionFetchResult> {
	try {
		const response = await fetch(CANONICAL_ATTENTION_PROJECTION_ENDPOINT, {
			headers: scopedRequestHeaders(),
			signal: options.signal
		});
		if (!response.ok) return { projection: null, fallback_reason: `HTTP ${response.status}` };
		const body: unknown = await response.json().catch(() => null);
		const wrapper = record(body);
		if (!wrapper || !Object.prototype.hasOwnProperty.call(wrapper, 'canonical_attention_projection')) {
			return { projection: null, fallback_reason: 'projection_missing' };
		}
		const parsed = parseCanonicalAttentionProjection(wrapper.canonical_attention_projection, scope);
		return parsed.ok
			? { projection: parsed.projection, fallback_reason: null }
			: {
					projection: null,
					fallback_reason:
						typedFallbackReason(wrapper.canonical_attention_projection) ?? parsed.reason
			  };
	} catch (error) {
		return {
			projection: null,
			fallback_reason: error instanceof Error ? error.message : String(error)
		};
	}
}

/** Ordered, identity-preserving lane selection. Deliberately no sorting,
 * filtering, grouping, or client-side rerouting. */
export function canonicalAttentionLaneItems(
	projection: CanonicalAttentionProjection,
	lane: CanonicalAttentionOriginLane
): readonly CanonicalAttentionItem[] {
	return projection.lanes[lane];
}

export function canonicalAttentionItemTimestamp(item: CanonicalAttentionItem): number {
	return item.origin_lane === 'follow_up'
		? item.payload.received_at ?? 0
		: item.payload.temporal_anchor_at ?? 0;
}
