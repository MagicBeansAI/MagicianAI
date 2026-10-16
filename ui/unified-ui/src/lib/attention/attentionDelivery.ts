import {
	parseCanonicalAttentionItem,
	type CanonicalAttentionItem,
	type CanonicalAttentionOriginLane,
	type CanonicalAttentionProjection,
	type CanonicalAttentionProjectionScope,
	type CanonicalAttentionProjectionStatus
} from './canonicalAttentionProjection';
import {
	ATTENTION_MIN_VISIBLE_MS_MAX,
	ATTENTION_VISIBLE_MS_MAX,
	ATTENTION_CANDIDATE_ID_MAX_CHARS,
	ATTENTION_CLIENT_TYPE_MAX_CHARS,
	ATTENTION_CLIENT_VERSION_MAX_CHARS,
	ATTENTION_DECISION_ID_MAX_CHARS,
	ATTENTION_EVENT_ID_MAX_CHARS,
	ATTENTION_SOURCE_REVISION_MAX_CHARS,
	ATTENTION_VIEWPORT_CLASS_MAX_CHARS,
	ATTENTION_VISIBILITY_RULE_MAX_CHARS,
	parseAttentionImpressionReceipt,
	type AttentionImpressionPolicy,
	type AttentionImpressionReceipt,
	type AttentionImpressionRequest
} from './attentionRouting';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

export const ATTENTION_DELIVERY_ENDPOINT =
	'/api/magician/v2/channel-assist/attention-learning/canonical-deliveries';

export type AttentionDeliveryRefreshReason =
	| 'expired'
	| 'projection_drift'
	| 'revision_drift'
	| 'scope_mismatch'
	| 'binding_mismatch'
	| 'cursor_not_found';

export type AttentionDeliveryFallbackReason =
	| 'bandit_disabled'
	| 'canonical_projection_baseline_fallback'
	| 'snapshot_missing_or_invalid'
	| 'feature_contract_mismatch'
	| 'posterior_unavailable'
	| 'policy_scope_not_canary'
	| 'policy_shadow_only';

export interface AttentionDeliveryRootDecision {
	decision_id: string;
	lane: CanonicalAttentionOriginLane;
	projection_id: string;
	universe_digest: string;
	policy_snapshot_id: string | null;
	policy_model_version: string | null;
	posterior_version: number;
	seed_identity: string;
	universe_size: number;
	created_at: number;
	expires_at: number;
}

export interface AttentionDeliveryPage {
	delivery_id: string;
	page_index: number;
	page_start: number;
	page_size: number;
	cursor: string | null;
	next_cursor: string | null;
	has_more: boolean;
	expires_at: number;
}

export interface AttentionDeliveredItem {
	position: number;
	candidate_id: string;
	source_revision: string | null;
	root_policy_propensity: number;
	conditional_delivery_propensity: 1;
	exposure_token: string;
	item: CanonicalAttentionItem;
}

export interface AttentionDeliveryHealth {
	bandit_mode: 'disabled' | 'shadow' | 'canary';
	canary_assigned: boolean;
	applied: boolean;
	baseline_preserved: boolean;
	complete_universe_recorded: boolean;
	propensity_coverage: number;
	degradation_reason: string | null;
	root_sample_count: number;
	delivered_count: number;
	remaining_count: number;
	exact_revision_match: boolean;
	replay: boolean;
}

export interface AttentionDeliveryPageResponse {
	schema_version: 1;
	status: CanonicalAttentionProjectionStatus;
	fallback_reason: AttentionDeliveryFallbackReason | null;
	root_decision: AttentionDeliveryRootDecision;
	page: AttentionDeliveryPage;
	items: AttentionDeliveredItem[];
	impression_policy: AttentionImpressionPolicy;
	health: AttentionDeliveryHealth;
}

export interface AttentionDeliveryRefreshRequired {
	schema_version: 1;
	status: 'refresh_required';
	error: 'attention_delivery_refresh_required';
	reason: AttentionDeliveryRefreshReason;
	lane: CanonicalAttentionOriginLane;
	refresh_href: string;
}

export interface AttentionDeliveryImpressionRequest extends AttentionImpressionRequest {
	delivery_id: string;
	page_index: number;
	position: number;
	exposure_token: string;
}

export interface AttentionDeliveryImpressionReceipt extends AttentionImpressionReceipt {
	delivery_id: string;
	page_index: number;
	position: number;
	exposure_token: string;
	root_policy_propensity: number;
	conditional_delivery_propensity: 1;
}

export type AttentionDeliveryImpressionResult =
	| { ok: true; receipt: AttentionDeliveryImpressionReceipt }
	| { ok: false; error: string; code: string | null; retryable: boolean };

export type AttentionDeliveryFetchResult =
	| { kind: 'page'; response: AttentionDeliveryPageResponse }
	| { kind: 'refresh_required'; refresh: AttentionDeliveryRefreshRequired }
	| { kind: 'fallback'; reason: string };

function record(value: unknown): Record<string, unknown> | null {
	return value !== null && typeof value === 'object' && !Array.isArray(value)
		? value as Record<string, unknown>
		: null;
}

function exact(source: Record<string, unknown>, keys: readonly string[]): boolean {
	const actual = Object.keys(source);
	return actual.length === keys.length && keys.every((key) =>
		Object.prototype.hasOwnProperty.call(source, key));
}

function structurallyEqual(left: unknown, right: unknown): boolean {
	if (Object.is(left, right)) return true;
	if (Array.isArray(left) || Array.isArray(right)) {
		return Array.isArray(left) && Array.isArray(right) &&
			left.length === right.length &&
			left.every((value, index) => structurallyEqual(value, right[index]));
	}
	const leftRecord = record(left);
	const rightRecord = record(right);
	if (!leftRecord || !rightRecord) return false;
	const leftKeys = Object.keys(leftRecord);
	const rightKeys = Object.keys(rightRecord);
	return leftKeys.length === rightKeys.length && leftKeys.every((key) =>
		Object.prototype.hasOwnProperty.call(rightRecord, key) &&
		structurallyEqual(leftRecord[key], rightRecord[key]));
}

function text(value: unknown, maxCharacters = 2_000): string | null {
	return typeof value === 'string' && value.trim().length > 0 && [...value].length <= maxCharacters &&
		![...value].some((character) => /\p{Cc}/u.test(character))
		? value
		: null;
}

function nullableText(value: unknown): string | null | undefined {
	return value === null ? null : text(value) ?? undefined;
}

function integer(value: unknown): number | null {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function probability(value: unknown, allowZero = true): number | null {
	return typeof value === 'number' && Number.isFinite(value) &&
		value <= 1 && (allowZero ? value >= 0 : value > 0) ? value : null;
}

function parseRoot(value: unknown, lane: CanonicalAttentionOriginLane): AttentionDeliveryRootDecision | null {
	const source = record(value);
	if (!source || !exact(source, [
		'decision_id', 'lane', 'projection_id', 'universe_digest', 'policy_snapshot_id',
		'policy_model_version', 'posterior_version', 'seed_identity', 'universe_size',
		'created_at', 'expires_at'
	])) return null;
	const decisionId = text(source.decision_id);
	const projectionId = text(source.projection_id);
	const digest = text(source.universe_digest);
	const snapshot = nullableText(source.policy_snapshot_id);
	const model = nullableText(source.policy_model_version);
	const posterior = integer(source.posterior_version);
	const seed = text(source.seed_identity);
	const size = integer(source.universe_size);
	const created = integer(source.created_at);
	const expires = integer(source.expires_at);
	if (!decisionId || [...decisionId].length > ATTENTION_DECISION_ID_MAX_CHARS ||
		source.lane !== lane || !projectionId || !digest ||
		snapshot === undefined || model === undefined || (model !== null && snapshot === null) ||
		posterior === null || !seed || size === null || created === null || expires === null ||
		created < 1 || expires <= created) return null;
	return {
		decision_id: decisionId,
		lane,
		projection_id: projectionId,
		universe_digest: digest,
		policy_snapshot_id: snapshot,
		policy_model_version: model,
		posterior_version: posterior,
		seed_identity: seed,
		universe_size: size,
		created_at: created,
		expires_at: expires
	};
}

function parsePage(value: unknown, root: AttentionDeliveryRootDecision): AttentionDeliveryPage | null {
	const source = record(value);
	if (!source || !exact(source, [
		'delivery_id', 'page_index', 'page_start', 'page_size', 'cursor', 'next_cursor',
		'has_more', 'expires_at'
	])) return null;
	const id = text(source.delivery_id);
	const index = integer(source.page_index);
	const start = integer(source.page_start);
	const size = integer(source.page_size);
	const cursor = nullableText(source.cursor);
	const next = nullableText(source.next_cursor);
	const expires = integer(source.expires_at);
	if (!id || [...id].length > ATTENTION_DECISION_ID_MAX_CHARS ||
		index === null || start === null || size === null || size < 1 ||
		cursor === undefined || next === undefined || typeof source.has_more !== 'boolean' ||
		expires !== root.expires_at || source.has_more !== (next !== null) ||
		start !== index * size ||
		(index === 0 ? cursor !== null || start !== 0 : cursor === null)) return null;
	return {
		delivery_id: id,
		page_index: index,
		page_start: start,
		page_size: size,
		cursor,
		next_cursor: next,
		has_more: source.has_more,
		expires_at: expires
	};
}

function parseHealth(value: unknown): AttentionDeliveryHealth | null {
	const source = record(value);
	if (!source || !exact(source, [
		'bandit_mode', 'canary_assigned', 'applied', 'baseline_preserved',
		'complete_universe_recorded', 'propensity_coverage', 'degradation_reason',
		'root_sample_count', 'delivered_count', 'remaining_count',
		'exact_revision_match', 'replay'
	])) return null;
	const coverage = probability(source.propensity_coverage);
	const degradation = nullableText(source.degradation_reason);
	const rootSampleCount = integer(source.root_sample_count);
	const deliveredCount = integer(source.delivered_count);
	const remainingCount = integer(source.remaining_count);
	if ((source.bandit_mode !== 'disabled' && source.bandit_mode !== 'shadow' && source.bandit_mode !== 'canary') ||
		typeof source.canary_assigned !== 'boolean' || typeof source.applied !== 'boolean' ||
		typeof source.baseline_preserved !== 'boolean' ||
		source.complete_universe_recorded !== true || coverage === null || degradation === undefined ||
		rootSampleCount === null || deliveredCount === null || remainingCount === null ||
		typeof source.exact_revision_match !== 'boolean' || typeof source.replay !== 'boolean') return null;
	return {
		bandit_mode: source.bandit_mode,
		canary_assigned: source.canary_assigned,
		applied: source.applied,
		baseline_preserved: source.baseline_preserved,
		complete_universe_recorded: true,
		propensity_coverage: coverage,
		degradation_reason: degradation,
		root_sample_count: rootSampleCount,
		delivered_count: deliveredCount,
		remaining_count: remainingCount,
		exact_revision_match: source.exact_revision_match,
		replay: source.replay
	};
}

function parseImpressionPolicy(value: unknown): AttentionImpressionPolicy | null {
	const source = record(value);
	if (!source || !exact(source, ['min_visible_ms', 'visibility_rule_version'])) return null;
	const minVisibleMs = integer(source.min_visible_ms);
	const rule = text(source.visibility_rule_version);
	if (minVisibleMs === null || minVisibleMs < 1 || minVisibleMs > ATTENTION_MIN_VISIBLE_MS_MAX || !rule) {
		return null;
	}
	return { min_visible_ms: minVisibleMs, visibility_rule_version: rule };
}

function healthMatchesFallback(
	reason: AttentionDeliveryFallbackReason,
	health: AttentionDeliveryHealth
): boolean {
	switch (reason) {
		case 'bandit_disabled':
		case 'canonical_projection_baseline_fallback':
			return health.bandit_mode === 'disabled' && health.root_sample_count === 0 &&
				!health.canary_assigned;
		case 'snapshot_missing_or_invalid':
		case 'feature_contract_mismatch':
		case 'posterior_unavailable':
			return health.bandit_mode !== 'disabled' && health.root_sample_count === 0 &&
				!health.canary_assigned;
		case 'policy_scope_not_canary':
			return health.bandit_mode === 'canary' && health.root_sample_count === 1 &&
				!health.canary_assigned;
		case 'policy_shadow_only':
			return health.bandit_mode === 'shadow' && health.root_sample_count === 1;
	}
}

export function parseAttentionDeliveryPageResponse(
	value: unknown,
	lane: CanonicalAttentionOriginLane,
	expectedProjection: CanonicalAttentionProjection
): AttentionDeliveryPageResponse | null {
	const source = record(value);
	if (!source || !exact(source, [
		'schema_version', 'status', 'fallback_reason', 'root_decision', 'page', 'items',
		'impression_policy', 'health'
	]) || source.schema_version !== 1 ||
		(source.status !== 'succeeded' && source.status !== 'baseline_fallback') ||
		!Array.isArray(source.items)) {
		return null;
	}
	const fallbackReason = nullableText(source.fallback_reason);
	const allowedFallbackReasons = new Set<AttentionDeliveryFallbackReason>([
		'bandit_disabled', 'canonical_projection_baseline_fallback', 'snapshot_missing_or_invalid',
		'feature_contract_mismatch', 'posterior_unavailable', 'policy_scope_not_canary',
		'policy_shadow_only'
	]);
	const root = parseRoot(source.root_decision, lane);
	if (!root) return null;
	const page = parsePage(source.page, root);
	const impressionPolicy = parseImpressionPolicy(source.impression_policy);
	const health = parseHealth(source.health);
	if (!page || !impressionPolicy || !health || fallbackReason === undefined ||
		(health.root_sample_count !== 0 && health.root_sample_count !== 1) ||
		(source.status === 'succeeded'
			? fallbackReason !== null || health.bandit_mode !== 'canary' || !health.canary_assigned ||
				!health.applied || health.baseline_preserved || health.degradation_reason !== null ||
				health.root_sample_count !== 1
			: fallbackReason === null || !allowedFallbackReasons.has(fallbackReason as AttentionDeliveryFallbackReason) ||
				health.applied || !health.baseline_preserved || health.degradation_reason !== fallbackReason ||
				!healthMatchesFallback(fallbackReason as AttentionDeliveryFallbackReason, health)) ||
		(expectedProjection.status === 'baseline_fallback' &&
			(source.status !== 'baseline_fallback' || health.bandit_mode !== 'disabled')) ||
		(health.bandit_mode === 'disabled' &&
			(health.canary_assigned || root.policy_snapshot_id !== null || root.policy_model_version !== null ||
				root.posterior_version !== 0 || root.seed_identity !== 'baseline' || health.root_sample_count !== 0)) ||
		(health.root_sample_count === 1 &&
			(root.policy_snapshot_id === null || root.policy_model_version === null)) ||
		root.projection_id !== expectedProjection.projection_id ||
		root.universe_digest !== expectedProjection.universe_digest ||
		root.universe_size !== expectedProjection.lanes[lane].length ||
		page.page_start > root.universe_size || source.items.length > page.page_size) return null;

	const items: AttentionDeliveredItem[] = [];
	const exposureTokens = new Set<string>();
	const candidateIds = new Set<string>();
	const expectedItems = new Map(expectedProjection.lanes[lane].map((item) => [item.canonical_id, item]));
	for (let index = 0; index < source.items.length; index += 1) {
		const raw = record(source.items[index]);
		if (!raw || !exact(raw, [
			'position', 'candidate_id', 'source_revision', 'root_policy_propensity',
			'conditional_delivery_propensity', 'exposure_token', 'item'
		])) return null;
		const position = integer(raw.position);
		const candidateId = text(raw.candidate_id, ATTENTION_CANDIDATE_ID_MAX_CHARS);
		const revision = raw.source_revision === null
			? null
			: text(raw.source_revision, ATTENTION_SOURCE_REVISION_MAX_CHARS) ?? undefined;
		const rootPropensity = probability(raw.root_policy_propensity, false);
		const conditional = probability(raw.conditional_delivery_propensity, false);
		const token = text(raw.exposure_token, 128);
		const item = parseCanonicalAttentionItem(raw.item, lane);
		const expectedItem = candidateId ? expectedItems.get(candidateId) : undefined;
		if (position !== page.page_start + index + 1 || !candidateId || revision === undefined ||
			rootPropensity === null || conditional !== 1 || !token || exposureTokens.has(token) ||
			candidateIds.has(candidateId) || !item || !expectedItem ||
			candidateId !== item.canonical_id || revision !== item.source_revision ||
			!structurallyEqual(item, expectedItem) ||
			(health.baseline_preserved &&
				expectedProjection.lanes[lane][position - 1]?.canonical_id !== candidateId)) return null;
		exposureTokens.add(token);
		candidateIds.add(candidateId);
		items.push({
			position,
			candidate_id: candidateId,
			source_revision: revision,
			root_policy_propensity: rootPropensity,
			conditional_delivery_propensity: 1,
			exposure_token: token,
			item
		});
	}
	const expectedItemCount = Math.min(page.page_size, root.universe_size - page.page_start);
	if (items.length !== expectedItemCount ||
		page.has_more !== (page.page_start + items.length < root.universe_size) ||
		(health.root_sample_count === 0 && items.some((item) => item.root_policy_propensity !== 1)) ||
		health.propensity_coverage !== (root.universe_size === 0 ? 0 : 1) ||
		health.delivered_count !== page.page_start + items.length ||
		health.remaining_count !== root.universe_size - health.delivered_count ||
		health.exact_revision_match !== true || health.replay !== (page.page_index > 0)) return null;
	return {
		schema_version: 1,
		status: source.status,
		fallback_reason: fallbackReason as AttentionDeliveryFallbackReason | null,
		root_decision: root,
		page,
		items,
		impression_policy: impressionPolicy,
		health
	};
}

export function parseAttentionDeliveryRefreshRequired(
	value: unknown,
	lane: CanonicalAttentionOriginLane
): AttentionDeliveryRefreshRequired | null {
	const source = record(value);
	if (!source || !exact(source, [
		'schema_version', 'status', 'error', 'reason', 'lane', 'refresh_href'
	]) || source.schema_version !== 1 || source.status !== 'refresh_required' ||
		source.error !== 'attention_delivery_refresh_required' || source.lane !== lane ||
		!['expired', 'projection_drift', 'revision_drift', 'scope_mismatch',
			'binding_mismatch', 'cursor_not_found'].includes(String(source.reason))) return null;
	const href = text(source.refresh_href);
	if (!href || !href.startsWith(`${ATTENTION_DELIVERY_ENDPOINT}/${lane}`)) return null;
	return {
		schema_version: 1,
		status: 'refresh_required',
		error: 'attention_delivery_refresh_required',
		reason: source.reason as AttentionDeliveryRefreshReason,
		lane,
		refresh_href: href
	};
}

export function parseAttentionDeliveryImpressionReceipt(
	value: unknown
): AttentionDeliveryImpressionReceipt | null {
	const source = record(value);
	const base = parseAttentionImpressionReceipt(value);
	if (!source || !base || !exact(source, [
		'impression_id', 'event_id', 'decision_id', 'candidate_id', 'source_revision',
		'surface', 'accumulated_visible_ms', 'min_visible_ms', 'visibility_rule_version',
		'verified', 'deduplicated', 'delivery_id', 'page_index', 'position', 'exposure_token',
		'root_policy_propensity', 'conditional_delivery_propensity'
	])) return null;
	const deliveryId = text(source.delivery_id);
	const pageIndex = integer(source.page_index);
	const position = integer(source.position);
	const token = text(source.exposure_token);
	const rootPropensity = probability(source.root_policy_propensity, false);
	const conditional = probability(source.conditional_delivery_propensity, false);
	if (!deliveryId || pageIndex === null || position === null || position < 1 || !token ||
		rootPropensity === null || conditional !== 1) return null;
	return {
		...base,
		delivery_id: deliveryId,
		page_index: pageIndex,
		position,
		exposure_token: token,
		root_policy_propensity: rootPropensity,
		conditional_delivery_propensity: 1
	};
}

export async function postAttentionDeliveryImpression(
	request: AttentionDeliveryImpressionRequest,
	scope: CanonicalAttentionProjectionScope,
	expectedRootPolicyPropensity: number
): Promise<AttentionDeliveryImpressionResult> {
	if (!text(request.event_id, ATTENTION_EVENT_ID_MAX_CHARS) ||
		!text(request.decision_id, ATTENTION_DECISION_ID_MAX_CHARS) ||
		!text(request.candidate_id, ATTENTION_CANDIDATE_ID_MAX_CHARS) ||
		(request.source_revision !== null &&
			!text(request.source_revision, ATTENTION_SOURCE_REVISION_MAX_CHARS)) ||
		(request.surface !== 'follow_up' && request.surface !== 'worth_a_look') ||
		integer(request.visible_ms) === null || request.visible_ms < 1 ||
		request.visible_ms > ATTENTION_VISIBLE_MS_MAX ||
		!text(request.visibility_rule_version, ATTENTION_VISIBILITY_RULE_MAX_CHARS) ||
		request.client_type !== 'web' ||
		!text(request.client_type, ATTENTION_CLIENT_TYPE_MAX_CHARS) ||
		!text(request.client_version, ATTENTION_CLIENT_VERSION_MAX_CHARS) ||
		!text(request.viewport_class, ATTENTION_VIEWPORT_CLASS_MAX_CHARS) ||
		!text(request.delivery_id, ATTENTION_DECISION_ID_MAX_CHARS) ||
		integer(request.page_index) === null || integer(request.position) === null || request.position < 1 ||
		!text(request.exposure_token, 128) || !text(scope.principal) || !text(scope.workspace) ||
		probability(expectedRootPolicyPropensity, false) === null) {
		return { ok: false, error: 'Invalid delivery impression contract', code: 'invalid_impression_request', retryable: false };
	}
	try {
		const response = await fetch('/api/magician/v2/channel-assist/attention-learning/impressions', {
			method: 'POST',
			headers: scopedRequestHeaders({ 'Content-Type': 'application/json' }),
			body: JSON.stringify(request)
		});
		const body: unknown = await response.json().catch(() => null);
		if (!response.ok) {
			const envelope = record(body);
			const error = record(envelope?.error);
			return {
				ok: false,
				error: text(error?.message) ?? `HTTP ${response.status}`,
				code: nullableText(error?.code) ?? null,
				retryable: response.status >= 500
			};
		}
		const receipt = parseAttentionDeliveryImpressionReceipt(body);
		if (!receipt || receipt.event_id !== request.event_id ||
			receipt.decision_id !== request.decision_id || receipt.candidate_id !== request.candidate_id ||
			receipt.source_revision !== request.source_revision || receipt.surface !== request.surface ||
			receipt.delivery_id !== request.delivery_id || receipt.page_index !== request.page_index ||
			receipt.position !== request.position || receipt.exposure_token !== request.exposure_token ||
			receipt.root_policy_propensity !== expectedRootPolicyPropensity ||
			receipt.visibility_rule_version !== request.visibility_rule_version || !receipt.verified ||
			receipt.accumulated_visible_ms < request.visible_ms || receipt.min_visible_ms > request.visible_ms) {
			return { ok: false, error: 'Malformed delivery impression receipt', code: null, retryable: false };
		}
		return { ok: true, receipt };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error), code: null, retryable: true };
	}
}

export async function fetchAttentionDeliveryPage(args: {
	lane: CanonicalAttentionOriginLane;
	scope: CanonicalAttentionProjectionScope;
	projection: CanonicalAttentionProjection;
	pageSize?: number;
	cursor?: string | null;
}): Promise<AttentionDeliveryFetchResult> {
	const params = new URLSearchParams();
	if (args.cursor) params.set('cursor', args.cursor);
	else if (args.pageSize !== undefined) params.set('page_size', String(Math.max(1, Math.floor(args.pageSize))));
	try {
		const response = await fetch(`${ATTENTION_DELIVERY_ENDPOINT}/${args.lane}?${params.toString()}`, {
			headers: scopedRequestHeaders()
		});
		const body: unknown = await response.json().catch(() => null);
		if (response.status === 409) {
			const refresh = parseAttentionDeliveryRefreshRequired(body, args.lane);
			return refresh ? { kind: 'refresh_required', refresh } : { kind: 'fallback', reason: 'malformed_refresh_required' };
		}
		if (!response.ok) return { kind: 'fallback', reason: `HTTP ${response.status}` };
		const parsed = parseAttentionDeliveryPageResponse(body, args.lane, args.projection);
		return parsed ? { kind: 'page', response: parsed } : { kind: 'fallback', reason: 'malformed_delivery_page' };
	} catch (error) {
		return { kind: 'fallback', reason: error instanceof Error ? error.message : String(error) };
	}
}
