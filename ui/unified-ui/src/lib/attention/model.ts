import { derived, type Readable } from 'svelte/store';

import type { FeedAttentionTotals, FeedItem } from '$lib/feed/types';
import {
	hitlRequestFromCanonicalEvent,
	hitlRequestFromFeedItem
} from '$lib/hitl/adapters';
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
			return ['requests', 'failed'];
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
	'service_health'
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
	const key = attentionRowDedupeKey(request.identifiers, entry.correlation_id);
	return {
		key,
		category: attentionSourceCategory(request.source),
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
		task_id: request.scope.task_id
	};
}

export function attentionFeedRow(
	item: FeedItem,
	category?: AttentionRowCategory
): AttentionDisplayRow {
	const metadata = metadataRecord(item);
	const adapted = hitlRequestFromFeedItem(item);
	const explicitSource = metadataString(metadata, 'source');
	const source =
		explicitSource && isKnownHitlSource(explicitSource)
			? explicitSource
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
	const executionId = metadataString(metadata, 'execution_id') ?? request?.scope.execution_id;
	const reviewHref = metadataString(metadata, 'review_href');
	return {
		key,
		category: item.status === 'failed' ? 'failed' : category ?? attentionSourceCategory(source),
		source,
		prompt: request?.prompt ?? item.title,
		hint: request?.hint ?? item.summary ?? undefined,
		scope:
			request?.scope ??
			({
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
		review_label: metadataString(metadata, 'review_label'),
		skillEvolution: parseSkillEvolutionDetails(metadata),
		rollbackRecommendation: parseRollbackRecommendationDetails(metadata),
		stale: request === null && !reviewHref,
		failed: item.status === 'failed',
		feed_item_id: item.id
	};
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
		merged.push(
			retainedAliasIds.length === (winner.alias_ids?.length ?? 0)
				? winner
				: { ...winner, alias_ids: retainedAliasIds }
		);
	}
	return merged.sort((a, b) => b.at - a.at);
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
		default:
			return 'requests';
	}
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
