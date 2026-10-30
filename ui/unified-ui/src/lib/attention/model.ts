import { derived, type Readable } from 'svelte/store';

import type { FeedAttentionTotals, FeedItem } from '$lib/feed/types';
import { isDetailedHint } from '$lib/hitl/hitlDetails';
import {
	hitlDelegationFromMetadata,
	hitlRequestFromCanonicalEvent,
	hitlRequestFromFeedItem
} from '$lib/hitl/adapters';
import {
	hitlOpenTargetFromFeedItem,
	hitlRequestFromOpenTarget
} from '$lib/attention/openHitlPrompt';
import { isFreeTextChoiceOption } from '$lib/hitl/freeTextChoice';
import type { HitlRequest, HitlSource } from '$lib/hitl/types';
import {
	pendingHitlEntries,
	type HitlPendingEntry
} from '$lib/stores/pendingHitlStore';
import {
	attentionStore,
	type AttentionStoreState
} from '$lib/stores/attentionStore';

export type AttentionSourceFilter = HitlSource | 'failed' | 'all';
export type AttentionCategory =
	| 'all'
	| 'requests'
	| 'approvals'
	| 'escalations'
	| 'failed';
export type AttentionRowCategory = Exclude<AttentionCategory, 'all'>;
export type AttentionFeedLane = 'requests' | 'approvals' | 'escalations' | 'failed';
export type AttentionViewMode = 'pending' | 'all';
export type AttentionRowOrigin = 'bus' | 'feed';
export type SkillEvolutionGate = 'proposal_review' | 'apply_gate' | 'promotion_gate';
export type SkillEvolutionGateAction = 'approve' | 'reject' | 'dry_run' | 'apply' | 'promote';
export type SkillEvolutionTargetSurface = 'scoped_skill' | 'source_skill';
export type SkillEvolutionRollbackDecision = 'dismissed' | 'superseded';

export interface AttentionInboxFeedback {
	kind?: 'notice' | 'error';
	message: string;
	action?: { label: string; href: string };
}

export interface SkillEvolutionGateDetails {
	gate: SkillEvolutionGate;
	action: SkillEvolutionGateAction;
	actionEnabled: boolean;
	candidateId: string;
	proposalId?: string;
	validationId?: string;
	implementationId?: string;
	applicationId?: string;
	targetSurface?: SkillEvolutionTargetSurface;
}

export interface SkillEvolutionRollbackDetails {
	recommendationId: string;
	candidateId: string;
	applicationId?: string;
}

/** Canonical row consumed by every full or compact Attention inbox. */
export interface AttentionDisplayRow {
	key: string;
	/** Operational inbox lane used for server-backed category paging. */
	category?: AttentionRowCategory;
	source: HitlSource;
	prompt: string;
	hint?: string;
	scope: HitlRequest['scope'];
	at: number;
	correlation_id: string;
	/** Alternate stream identifiers that resolve to the same item. */
	alias_ids?: string[];
	origin: AttentionRowOrigin;
	request: HitlRequest | null;
	pause_state_id?: string;
	execution_id?: string;
	task_id?: string;
	stale?: boolean;
	review_href?: string;
	review_label?: string;
	skillEvolution?: SkillEvolutionGateDetails;
	rollbackRecommendation?: SkillEvolutionRollbackDetails;
	failed?: boolean;
	/** Raw FeedItem.id used for durable failed-item dismissal. */
	feed_item_id?: string;
	/**
	 * The delegated agent whose child run raised this ask on the parent task
	 * (C2), rendered as "from <agent>". `execution_id` is then the child's.
	 */
	delegated_agent_id?: string;
	/**
	 * Set when the run is waiting on the reader but no question reached them
	 * (C3, `metadata.attention_reason = "waiting_without_question"`). There is
	 * nothing to answer, so the row offers Cancel and Open task instead.
	 */
	stalledWait?: AttentionStalledWait;
}

/** A run stuck waiting on the user with no deliverable question (C3). */
export interface AttentionStalledWait {
	executionId: string | null;
	taskId: string | null;
	/** `metadata.actions` as published; the row renders only these. */
	actions: readonly AttentionStalledWaitAction[];
}

export type AttentionStalledWaitAction = 'cancel' | 'open_task';

export const STALLED_WAIT_SUMMARY = 'Waiting on you, but no question was delivered';

/** Where "Open task" goes. `/tasks` selects the task on arrival. */
export function attentionTaskHref(taskId: string): string {
	return `/work?filter=all&selected=${encodeURIComponent(taskId)}`;
}

export const ATTENTION_SOURCE_LABELS: Record<HitlSource, string> = {
	approval: 'Approval',
	clarification: 'Clarification',
	plan_approval: 'Plan approval',
	user_request: 'User request',
	agentic: 'Agentic pause',
	escalation: 'Escalation',
	diff_approval: 'Diff approval',
	service_health: 'Service health',
	mcp_oauth: 'Sign-in',
	bot_auth: 'Bot auth'
};

export const ATTENTION_SOURCE_COLORS: Record<HitlSource, string> = {
	approval: 'var(--color-warning, #d28b1a)',
	clarification: 'var(--color-info, #4d9de0)',
	plan_approval: 'var(--accent-primary, #ff6b6b)',
	user_request: 'var(--accent-secondary, #4ecdc4)',
	agentic: 'var(--color-info, #4d9de0)',
	escalation: 'var(--color-error, #d23a3a)',
	diff_approval: 'var(--color-success, #2f8f5b)',
	service_health: 'var(--color-warning, #d28b1a)',
	mcp_oauth: 'var(--accent-primary, #ff6b6b)',
	bot_auth: 'var(--accent-primary, #ff6b6b)'
};

export const ATTENTION_FAILED_LABEL = 'Failed';
export const ATTENTION_FAILED_COLOR = 'var(--color-error, #d23a3a)';
export const ATTENTION_FILTER_LABELS: Record<Exclude<AttentionSourceFilter, 'all'>, string> = {
	...ATTENTION_SOURCE_LABELS,
	failed: ATTENTION_FAILED_LABEL
};

export const ATTENTION_CATEGORY_ORDER: readonly AttentionCategory[] = [
	'all',
	'requests',
	'approvals',
	'escalations',
	'failed'
];

export const ATTENTION_CATEGORY_LABELS: Record<AttentionCategory, string> = {
	all: 'All',
	requests: 'Requests',
	approvals: 'Approvals',
	escalations: 'Escalations',
	failed: 'Failed'
};

const ATTENTION_FEED_LANES: readonly AttentionFeedLane[] = [
	'requests',
	'approvals',
	'escalations',
	'failed'
];

export function attentionFeedLanesForCategory(
	category: AttentionCategory
): readonly AttentionFeedLane[] {
	switch (category) {
		case 'requests':
			return ['requests'];
		case 'approvals':
			return ['approvals'];
		case 'escalations':
			return ['escalations'];
		case 'failed':
			return ['failed'];
		default:
			return ATTENTION_FEED_LANES;
	}
}

/**
 * Tab counts from the store's per-lane totals. `all` means "every HITL
 * category" and matches exactly what the badge counts
 * (`needs_action + failed`, see `attentionBadgeCount.ts`): the Attention
 * surface is for live just-in-time notifications (approvals, requests,
 * escalations, failures). Channel follow-ups are patient, not urgent — they
 * live in Today and the Messages-free lanes there.
 */
export function attentionCategoryCountsFromTotals(
	totals: FeedAttentionTotals
): Record<AttentionCategory, number> {
	const requests = totals.requests;
	const approvals = totals.approvals;
	const escalations = totals.escalations;
	const failed = totals.failed;
	return {
		// Matches the badge (`needs_action + failed`) and what the `all` list
		// actually renders.
		all: requests + approvals + escalations + failed,
		requests,
		approvals,
		escalations,
		failed
	};
}

const KNOWN_HITL_SOURCES: readonly HitlSource[] = [
	'approval',
	'clarification',
	'plan_approval',
	'user_request',
	'agentic',
	'escalation',
	'diff_approval',
	'bot_auth',
	'service_health',
	'mcp_oauth'
];

function metadataRecord(item: FeedItem): Record<string, unknown> {
	return item.metadata !== null && typeof item.metadata === 'object'
		? (item.metadata as Record<string, unknown>)
		: {};
}

function metadataString(metadata: Record<string, unknown>, key: string): string | undefined {
	const value = metadata[key];
	return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
}

function metadataBool(metadata: Record<string, unknown>, key: string): boolean | undefined {
	const value = metadata[key];
	return typeof value === 'boolean' ? value : undefined;
}

export function isKnownHitlSource(source: string): source is HitlSource {
	return (KNOWN_HITL_SOURCES as readonly string[]).includes(source);
}

export function attentionKindToSource(kind: string): HitlSource {
	switch (kind) {
		case 'user_request.pending':
			return 'user_request';
		case 'max_iterations_reached':
			return 'escalation';
		case 'diff_approval':
			return 'diff_approval';
		case 'input.requested':
		case 'waiting_for_confirmation':
		case 'hitl.requested':
		default:
			return 'agentic';
	}
}

function parseSkillEvolutionGate(value: string | undefined): SkillEvolutionGate | null {
	return value === 'proposal_review' || value === 'apply_gate' || value === 'promotion_gate'
		? value
		: null;
}

function parseSkillEvolutionGateAction(
	value: string | undefined,
	gate: SkillEvolutionGate
): SkillEvolutionGateAction {
	if (
		value === 'approve' ||
		value === 'reject' ||
		value === 'dry_run' ||
		value === 'apply' ||
		value === 'promote'
	) {
		return value;
	}
	if (gate === 'proposal_review') return 'approve';
	if (gate === 'apply_gate') return 'dry_run';
	return 'promote';
}

function parseSkillEvolutionTargetSurface(
	value: string | undefined
): SkillEvolutionTargetSurface | undefined {
	return value === 'scoped_skill' || value === 'source_skill' ? value : undefined;
}

export function parseSkillEvolutionDetails(
	metadata: Record<string, unknown>
): SkillEvolutionGateDetails | undefined {
	const gate = parseSkillEvolutionGate(metadataString(metadata, 'skill_evolution_gate'));
	const candidateId = metadataString(metadata, 'candidate_id');
	if (!gate || !candidateId) return undefined;
	return {
		gate,
		action: parseSkillEvolutionGateAction(
			metadataString(metadata, 'skill_evolution_gate_action'),
			gate
		),
		actionEnabled: metadataBool(metadata, 'skill_evolution_gate_action_enabled') ?? true,
		candidateId,
		proposalId: metadataString(metadata, 'proposal_id'),
		validationId: metadataString(metadata, 'validation_id'),
		implementationId: metadataString(metadata, 'implementation_id'),
		applicationId: metadataString(metadata, 'application_id'),
		targetSurface: parseSkillEvolutionTargetSurface(metadataString(metadata, 'target_surface'))
	};
}

export function parseRollbackRecommendationDetails(
	metadata: Record<string, unknown>
): SkillEvolutionRollbackDetails | undefined {
	if (metadataString(metadata, 'attention_target') !== 'skill_evolution_rollback_recommendation') {
		return undefined;
	}
	const recommendationId = metadataString(metadata, 'recommendation_id');
	const candidateId = metadataString(metadata, 'candidate_id');
	if (!recommendationId || !candidateId) return undefined;
	return {
		recommendationId,
		candidateId,
		applicationId: metadataString(metadata, 'application_id')
	};
}

function addAlias(aliases: Set<string>, value: unknown): void {
	if (typeof value === 'string' && value.length > 0) aliases.add(value);
}

/** Every identifier that may refer to the same underlying Attention item. */
export function attentionRowAliases(
	row: AttentionDisplayRow,
	request: HitlRequest | null = row.request
): Set<string> {
	const aliases = new Set<string>();
	addAlias(aliases, row.key);
	addAlias(aliases, row.correlation_id);
	addAlias(aliases, row.pause_state_id);
	addAlias(aliases, row.feed_item_id);
	for (const alias of row.alias_ids ?? []) addAlias(aliases, alias);
	if (request) {
		addAlias(aliases, request.id);
		addAlias(aliases, request.identifiers.pause_state_id);
		addAlias(aliases, request.identifiers.approval_id);
		addAlias(aliases, request.identifiers.correlation_id);
		addAlias(aliases, request.identifiers.request_id);
	}
	return aliases;
}

/** Canonical primary key; alias sets handle streams that choose different primaries. */
export function attentionRowDedupeKey(
	identifiers: HitlRequest['identifiers'],
	fallback: string
): string {
	return (
		identifiers.pause_state_id ??
		identifiers.approval_id ??
		identifiers.correlation_id ??
		identifiers.request_id ??
		fallback
	);
}

export function attentionBusRow(entry: HitlPendingEntry): AttentionDisplayRow | null {
	const request = entry.raw ? hitlRequestFromCanonicalEvent(entry.raw) : null;
	if (!request) return null;
	const review = requestReviewNavigation(request);
	const key = attentionRowDedupeKey(request.identifiers, entry.correlation_id);
	return {
		key,
		category: attentionRequestCategory(request),
		source: request.source,
		prompt: request.prompt,
		hint: request.hint,
		scope: request.scope,
		at: entry.at,
		correlation_id: entry.correlation_id,
		alias_ids: [entry.pause_state_id, entry.approval_id, request.id].filter(
			(value): value is string => typeof value === 'string' && value.length > 0
		),
		origin: 'bus',
		request,
		pause_state_id: request.identifiers.pause_state_id ?? entry.pause_state_id,
		execution_id: request.scope.execution_id,
		task_id: request.scope.task_id,
		...review,
		delegated_agent_id: request.delegated_agent_id
	};
}

/** Review navigation is a producer-supplied local route, never an answer or approval. */
function requestReviewNavigation(request: HitlRequest | null): {
	review_href?: string;
	review_label?: string;
} {
	const schema = request?.raw?.input_schema;
	if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return {};
	const context = (schema as Record<string, unknown>).context;
	if (!context || typeof context !== 'object' || Array.isArray(context)) return {};
	const metadata = context as Record<string, unknown>;
	const href = metadataString(metadata, 'review_href');
	// Reject protocol-relative URLs, backslash normalization and control characters.
	if (!href || !href.startsWith('/') || href.startsWith('//') || /[\\\u0000-\u0020\u007f]/.test(href)) return {};
	return { review_href: href, review_label: metadataString(metadata, 'review_label') };
}

/**
 * The C3 stalled-wait stamp on an attention row, or `undefined`.
 *
 * Only the published actions this client can perform are kept; when the
 * backend names none, both are offered, since cancelling and opening the
 * task are the only two things a reader can do about a run that is waiting
 * on a question that never arrived.
 */
export function parseStalledWait(
	metadata: Record<string, unknown>,
	item: FeedItem
): AttentionStalledWait | undefined {
	if (metadataString(metadata, 'attention_reason') !== 'waiting_without_question') return undefined;
	const published = Array.isArray(metadata.actions)
		? metadata.actions.filter(
				(action): action is AttentionStalledWaitAction =>
					action === 'cancel' || action === 'open_task'
			)
		: [];
	return {
		executionId: metadataString(metadata, 'execution_id') ?? null,
		taskId: item.task_id ?? metadataString(metadata, 'task_id') ?? null,
		actions: published.length > 0 ? published : ['cancel', 'open_task']
	};
}

export function attentionFeedRow(
	item: FeedItem,
	category?: AttentionRowCategory
): AttentionDisplayRow {
	const metadata = metadataRecord(item);
	const stalledWait = parseStalledWait(metadata, item);
	// A stalled wait has no ask to adapt; a row that carries only the embedded
	// `hitl_request` (a delegated child's ask, C2) is adapted from that target.
	const adapted = stalledWait
		? null
		: (hitlRequestFromFeedItem(item) ?? embeddedRequestOf(item));
	const explicitSource = metadataString(metadata, 'source');
	const source =
		explicitSource && isKnownHitlSource(explicitSource)
			? explicitSource
			: stalledWait
				? 'escalation'
				: adapted?.source ??
				(item.item_type === 'approval'
					? 'approval'
					: attentionKindToSource(metadataString(metadata, 'attention_kind') ?? ''));
	// The request drives canonical POST routing, so preserve an explicit source
	// there as well as on the visual row.
	const request = adapted ? { ...adapted, source } : null;
	const pauseStateId = metadataString(metadata, 'pause_state_id');
	const identifiers: HitlRequest['identifiers'] = {
		pause_state_id: pauseStateId ?? request?.identifiers.pause_state_id,
		approval_id: metadataString(metadata, 'approval_id') ?? request?.identifiers.approval_id,
		correlation_id:
			metadataString(metadata, 'correlation_id') ?? request?.identifiers.correlation_id,
		request_id: metadataString(metadata, 'request_id') ?? request?.identifiers.request_id
	};
	const key = attentionRowDedupeKey(identifiers, request?.id ?? item.id);
	const delegation = hitlDelegationFromMetadata(metadata);
	const executionId =
		delegation?.executionId ??
		metadataString(metadata, 'execution_id') ??
		request?.scope.execution_id;
	// A stalled wait has nothing to answer; activating it opens its task.
	const requestReview = requestReviewNavigation(request);
	const reviewHref =
		metadataString(metadata, 'review_href') ??
		requestReview.review_href ??
		(stalledWait?.taskId && stalledWait.actions.includes('open_task')
			? attentionTaskHref(stalledWait.taskId)
			: undefined);
	return {
		key,
		category: item.status === 'failed' ? 'failed' : category ?? attentionSourceCategory(source),
		source,
		prompt: stalledWait ? STALLED_WAIT_SUMMARY : (request?.prompt ?? item.title),
		hint: stalledWait
			? (item.title && item.title !== STALLED_WAIT_SUMMARY ? item.title : undefined)
			: (request?.hint ?? item.summary ?? undefined),
		scope:
			request?.scope ??
			({
				principal: item.principal,
				workspace: item.workspace,
				task_id: item.task_id ?? undefined,
				execution_id: executionId,
				agent_id: item.agent_id ?? undefined,
				thread_id: item.ui_thread_id ?? undefined
			} satisfies HitlRequest['scope']),
		at: item.updated_at,
		correlation_id: identifiers.correlation_id ?? key,
		alias_ids: [
			item.id,
			identifiers.pause_state_id,
			identifiers.approval_id,
			identifiers.correlation_id,
			identifiers.request_id,
			request?.id
		].filter((value): value is string => typeof value === 'string' && value.length > 0),
		origin: 'feed',
		request,
		pause_state_id: identifiers.pause_state_id,
		execution_id: executionId,
		task_id: item.task_id ?? undefined,
		review_href: reviewHref,
		review_label:
			metadataString(metadata, 'review_label') ?? requestReview.review_label ?? (stalledWait ? 'Open task' : undefined),
		skillEvolution: parseSkillEvolutionDetails(metadata),
		rollbackRecommendation: parseRollbackRecommendationDetails(metadata),
		stale: request === null && !reviewHref,
		failed: item.status === 'failed',
		feed_item_id: item.id,
		delegated_agent_id: request?.delegated_agent_id ?? delegation?.agentId ?? undefined,
		stalledWait
	};
}

/** The row's embedded `metadata.hitl_request`, when the flat metadata could not be adapted. */
function embeddedRequestOf(item: FeedItem): HitlRequest | null {
	const metadata = metadataRecord(item);
	if (!metadata.hitl_request) return null;
	const target = hitlOpenTargetFromFeedItem(item);
	return target ? hitlRequestFromOpenTarget(target) : null;
}

export function collectAttentionFeedItems(state: AttentionStoreState): FeedItem[] {
	const seen = new Set<string>();
	const items: FeedItem[] = [];
	for (const bucket of [state.requests, state.approvals, state.escalations, state.failed]) {
		for (const item of bucket) {
			if (seen.has(item.id)) continue;
			seen.add(item.id);
			items.push(item);
		}
	}
	return items;
}

/** Merge normalized rows with bus priority and alias-aware deduplication. */
export function mergeAttentionRows(
	busRows: AttentionDisplayRow[],
	feedRows: AttentionDisplayRow[]
): AttentionDisplayRow[] {
	const rows = [...busRows, ...feedRows];
	const parents = rows.map((_, index) => index);
	const aliasOwners = new Map<string, number>();

	function root(index: number): number {
		while (parents[index] !== index) {
			parents[index] = parents[parents[index]];
			index = parents[index];
		}
		return index;
	}

	function union(left: number, right: number): void {
		const leftRoot = root(left);
		const rightRoot = root(right);
		if (leftRoot === rightRoot) return;
		// Inputs are bus-first, so retaining the earlier root preserves the
		// richer bus row when a feed row bridges multiple alias groups.
		parents[Math.max(leftRoot, rightRoot)] = Math.min(leftRoot, rightRoot);
	}

	rows.forEach((row, index) => {
		for (const alias of attentionRowAliases(row)) {
			const owner = aliasOwners.get(alias);
			if (owner === undefined) {
				aliasOwners.set(alias, index);
			} else {
				union(index, owner);
			}
		}
	});

	const merged: AttentionDisplayRow[] = [];
	for (let index = 0; index < rows.length; index += 1) {
		if (root(index) !== index) continue;
		const winner = rows[index];
		const winnerAliases = attentionRowAliases(winner);
		const retainedAliasIds = [...(winner.alias_ids ?? [])];
		const retainedAliasSet = new Set(retainedAliasIds);
		for (let candidate = 0; candidate < rows.length; candidate += 1) {
			if (root(candidate) !== index) continue;
			for (const alias of attentionRowAliases(rows[candidate])) {
				if (winnerAliases.has(alias) || retainedAliasSet.has(alias)) continue;
				retainedAliasSet.add(alias);
				retainedAliasIds.push(alias);
			}
		}
		// The bus row wins, but only the feed row knows a delegated child raised
		// the ask (C2) — `HitlRequested` carries no delegation stamp — so the
		// label and the child execution are carried over from whichever member
		// has them.
		let delegatedAgentId = winner.delegated_agent_id;
		if (!delegatedAgentId) {
			for (let candidate = 0; candidate < rows.length; candidate += 1) {
				if (root(candidate) !== index) continue;
				if (rows[candidate].delegated_agent_id) {
					delegatedAgentId = rows[candidate].delegated_agent_id;
					break;
				}
			}
		}
		// The bus row's content wins; the lane is the server's when a feed row
		// carries the same ask, so the tab and its count agree.
		let category = winner.category;
		for (let candidate = busRows.length; candidate < rows.length; candidate += 1) {
			if (root(candidate) === index && rows[candidate].category) {
				category = rows[candidate].category;
				break;
			}
		}
		const withLane = category === winner.category ? winner : { ...winner, category };
		const withAliases =
			retainedAliasIds.length === (withLane.alias_ids?.length ?? 0)
				? withLane
				: { ...withLane, alias_ids: retainedAliasIds };
		merged.push(
			delegatedAgentId && !winner.delegated_agent_id
				? {
						...withAliases,
						delegated_agent_id: delegatedAgentId,
						request: withAliases.request
							? { ...withAliases.request, delegated_agent_id: delegatedAgentId }
							: withAliases.request
					}
				: withAliases
		);
	}
	// Canonical requests can arrive before the cached task projection catches
	// up. A delivered ask contradicts a question-less warning for that exact
	// scoped execution; other executions on the same task remain visible.
	const executionKey = (row: AttentionDisplayRow): string | null => {
		const execution = row.execution_id ?? row.scope.execution_id;
		return execution
			? JSON.stringify([row.scope.principal ?? '', row.scope.workspace ?? '', execution])
			: null;
	};
	const askedExecutions = new Set(
		merged.filter((row) => row.request && !row.failed && !row.stale)
			.map(executionKey).filter((key): key is string => key !== null)
	);
	return merged.filter((row) => !row.stalledWait || !askedExecutions.has(executionKey(row) ?? ''))
		.sort((a, b) => b.at - a.at);
}

export function buildAttentionRows(
	entries: HitlPendingEntry[],
	feedItems: FeedItem[]
): AttentionDisplayRow[] {
	const busRows = entries
		.map(attentionBusRow)
		.filter((row): row is AttentionDisplayRow => row !== null)
		.sort((a, b) => b.at - a.at);
	return mergeAttentionRows(busRows, feedItems.map((item) => attentionFeedRow(item)));
}

export function createAttentionRowsStore(
	entries: Readable<HitlPendingEntry[]> = pendingHitlEntries,
	feed: Readable<AttentionStoreState> = attentionStore
): Readable<AttentionDisplayRow[]> {
	return derived([entries, feed], ([$entries, $feed]) => {
		const busRows = $entries
			.map(attentionBusRow)
			.filter((row): row is AttentionDisplayRow => row !== null)
			.sort((a, b) => b.at - a.at);
		const feedRows = [
			...$feed.requests.map((item) => attentionFeedRow(item, 'requests')),
			...$feed.approvals.map((item) => attentionFeedRow(item, 'approvals')),
			...$feed.escalations.map((item) => attentionFeedRow(item, 'escalations')),
			...$feed.failed.map((item) => attentionFeedRow(item, 'failed'))
		];
		return mergeAttentionRows(busRows, feedRows);
	});
}

/** Shared derived inbox used by /attention and shell-level consumers. */
export const attentionRows = createAttentionRowsStore();

export function attentionSourceCategory(source: HitlSource): AttentionRowCategory {
	switch (source) {
		case 'approval':
		case 'plan_approval':
		case 'diff_approval':
			return 'approvals';
		case 'escalation':
			return 'escalations';
		case 'service_health':
			return 'failed';
		default:
			return 'requests';
	}
}

/**
 * The lane of a live ask, mirroring the server's `attention_lane_for_ask`
 * (`feed/store.rs`): a budget or iteration-limit pause is an escalation, a
 * confirmation naming its action is an approval, otherwise its source decides.
 */
export function attentionRequestCategory(request: HitlRequest): AttentionRowCategory {
	const schema = (request.schema ?? {}) as Record<string, unknown>;
	if (schema.pause_kind === 'budget' || schema.pause_kind === 'max_iterations') return 'escalations';
	if (request.input_type === 'diff_approval') return 'approvals';
	if (request.input_type === 'confirmation' && typeof schema.action_type === 'string' && schema.action_type.trim())
		return 'approvals';
	return attentionSourceCategory(request.source);
}

export function attentionRowOperationalCategory(
	row: AttentionDisplayRow
): AttentionRowCategory {
	if (row.failed) return 'failed';
	return row.category ?? attentionSourceCategory(row.source);
}

export function filterAttentionRowsByCategory(
	rows: AttentionDisplayRow[],
	category: AttentionCategory
): AttentionDisplayRow[] {
	return category === 'all'
		? rows
		: rows.filter((row) => attentionRowOperationalCategory(row) === category);
}

export function attentionRowCategory(row: AttentionDisplayRow): AttentionSourceFilter {
	return row.failed ? 'failed' : row.source;
}

export function filterAttentionRows(
	rows: AttentionDisplayRow[],
	filter: AttentionSourceFilter,
	search: string
): AttentionDisplayRow[] {
	const needle = search.trim().toLowerCase();
	return rows.filter((row) => {
		// NOTE: `'all'` here is a "no category filtering" SENTINEL, not the
		// user-facing All category. Both page call sites pass it literally to run
		// a search-only pass over rows already categorised by
		// `filterAttentionRowsByCategory`, and the page hardcodes
		// `sourceFilter="all"` into AttentionInboxSurface for the same reason.
		// Category semantics live in `filterAttentionRowsByCategory` ALONE.
		if (filter !== 'all' && attentionRowCategory(row) !== filter) return false;
		if (!needle) return true;
		return [
			row.prompt,
			row.hint ?? '',
			row.scope.task_id ?? '',
			row.scope.thread_id ?? '',
			row.scope.agent_id ?? '',
			row.correlation_id
		]
			.join(' ')
			.toLowerCase()
			.includes(needle);
	});
}

export function countAttentionRowsBySource(
	rows: AttentionDisplayRow[]
): Record<string, number> {
	return rows.reduce<Record<string, number>>((counts, row) => {
		const category = attentionRowCategory(row);
		counts[category] = (counts[category] ?? 0) + 1;
		return counts;
	}, {});
}

export function attentionScopeLabel(scope: HitlRequest['scope']): string {
	const parts: string[] = [];
	if (scope.thread_id) parts.push(`thread:${scope.thread_id}`);
	if (scope.task_id) parts.push(`task:${scope.task_id.slice(0, 8)}`);
	if (scope.agent_id) parts.push(scope.agent_id);
	return parts.join(' · ');
}

export function attentionRelativeTime(ms: number, now = Date.now()): string {
	const minutes = Math.round((now - ms) / 60_000);
	if (minutes < 1) return 'just now';
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.round(minutes / 60);
	if (hours < 48) return `${hours}h ago`;
	return `${Math.round(hours / 24)}d ago`;
}

export function skillEvolutionActionLabel(action: SkillEvolutionGateAction): string {
	if (action === 'approve') return 'Approve';
	if (action === 'reject') return 'Reject';
	if (action === 'dry_run') return 'Dry-run';
	if (action === 'apply') return 'Apply';
	return 'Promote';
}

/** One answer an Attention row offers as its own button. */
export interface AttentionQuickChoice {
	/** The id the prompt would report for this answer (`confirm`/`deny` for a confirmation). */
	id: string;
	label: string;
	/** A change/other answer: its words are typed, so the button opens the prompt. */
	opensPrompt: boolean;
}

/**
 * The answers a pending agentic ask offers as one-click buttons on its row,
 * or none.
 *
 * Only `source: "agentic"` — the `need_user_input` pause. `escalation`
 * (max-iterations, cannot-proceed, loop-detected) spends budget and has its
 * own continuation endpoint, so it keeps the prompt. A `choice` offers its
 * options, one per id; a confirmation offers Approve / Deny only on an
 * allow-list: the producer's explicit `destructive: false`, no tool
 * `action_type` (an agentic action approval), not a protected app, no preview
 * card, not a captured send's review. Everything else — other input types,
 * secret asks, preference holds, rows that open a review — keeps the row click
 * and its prompt, because that is where the rest of the ask is shown.
 */
export function attentionQuickChoices(row: AttentionDisplayRow): AttentionQuickChoice[] {
	const request = row.request;
	if (!request || row.failed || row.review_href || row.stalledWait) return [];
	if (row.skillEvolution || row.rollbackRecommendation) return [];
	if (request.source !== 'agentic') return [];
	// An ask that carries what it is about (a proposal under "Details") is
	// answered from its prompt, where the details show; the row cannot show them.
	if (isDetailedHint(row.hint)) return [];
	const schema = request.schema;
	if (schema.sensitive || schema.alignment || schema.protected === true) return [];
	if (request.input_type === 'choice') {
		const seen = new Set<string>();
		const choices: AttentionQuickChoice[] = [];
		for (const option of schema.options ?? []) {
			// A repeated id is one answer; the keyed row list must not see it twice.
			if (seen.has(option.id)) continue;
			seen.add(option.id);
			choices.push({
				id: option.id,
				label: option.label,
				opensPrompt: isFreeTextChoiceOption(option)
			});
		}
		return choices;
	}
	if (
		request.input_type === 'confirmation' &&
		schema.destructive === false &&
		!schema.action_type &&
		!schema.approval_preview &&
		// A captured send's review has three answers, one of them a standing
		// grant; a row's Approve / Deny pair cannot express it.
		schema.escalation_trigger !== 'outward_review'
	) {
		return [
			{ id: 'confirm', label: schema.confirm_label ?? 'Approve', opensPrompt: false },
			{ id: 'deny', label: schema.deny_label ?? 'Deny', opensPrompt: false }
		];
	}
	return [];
}
