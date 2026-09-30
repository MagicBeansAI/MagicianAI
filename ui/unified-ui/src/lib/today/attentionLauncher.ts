import type { TodayItem } from './types';
import { parseAttentionRouteIntent } from '$lib/attention/routeLauncher';
import {
	hitlOpenTargetFromFeedItem,
	type HitlOpenTarget
} from '$lib/attention/openHitlPrompt';
import type { FeedItem, FeedItemType } from '$lib/feed/types';

const ATTENTION_METADATA_ID_KEYS = [
	'pause_state_id',
	'approval_id',
	'correlation_id',
	'request_id',
	'feed_item_id',
	'dedupe_key',
	'dedupe_id',
	'dedupe'
] as const;

const KNOWN_ATTENTION_SOURCE_PREFIXES = [
	'v3:attention:',
	'skill_evolution_approval:',
	'skill_evolution_rollback:',
	'skill_evolution_post_promotion:'
] as const;

function nonEmptyString(value: unknown): string | null {
	return typeof value === 'string' && value.trim().length > 0 ? value.trim() : null;
}

function metadataRecord(item: TodayItem): Record<string, unknown> | null {
	return item.metadata && typeof item.metadata === 'object' && !Array.isArray(item.metadata)
		? (item.metadata as Record<string, unknown>)
		: null;
}

function projectedFeedItemId(item: TodayItem): string | null {
	const prefix = `today:${item.section}:`;
	return item.id.startsWith(prefix) ? nonEmptyString(item.id.slice(prefix.length)) : null;
}

function knownResolvableSourceId(item: TodayItem, projectedId: string | null): string | null {
	const sourceId = nonEmptyString(item.source_id);
	if (!sourceId) return null;
	if (sourceId === projectedId || sourceId === nonEmptyString(item.id)) return sourceId;
	return KNOWN_ATTENTION_SOURCE_PREFIXES.some((prefix) => sourceId.startsWith(prefix))
		? sourceId
		: null;
}

export function todayAttentionItemId(item: TodayItem): string | null {
	const routeItemId = parseAttentionRouteIntent(item.source_url)?.itemId;
	if (routeItemId) return routeItemId;

	const metadata = metadataRecord(item);
	for (const key of ATTENTION_METADATA_ID_KEYS) {
		const value = nonEmptyString(metadata?.[key]);
		if (value) return value;
	}

	const projectedId = projectedFeedItemId(item);
	return projectedId ?? knownResolvableSourceId(item, projectedId);
}

const FEED_ITEM_TYPES = new Set<FeedItemType>([
	'task',
	'approval',
	'agent_message',
	'data_delivery',
	'routine_result',
	'escalation',
	'learning_candidate',
	'learning_insight',
	'agent_learning'
]);

/** Recover the complete HITL payload retained by the Today projection. */
export function todayHitlOpenTarget(item: TodayItem): HitlOpenTarget | null {
	const sourceKind = item.source_kind as FeedItemType;
	const feedItem: FeedItem = {
		id: projectedFeedItemId(item) ?? item.source_id ?? item.id,
		principal: item.principal,
		workspace: item.workspace,
		item_type: FEED_ITEM_TYPES.has(sourceKind) ? sourceKind : 'task',
		task_id: item.task_id,
		ui_thread_id: item.thread_id,
		agent_id: item.agent_id,
		title: item.title,
		summary: item.summary,
		status: item.status,
		created_at: item.created_at,
		updated_at: item.updated_at,
		actions: item.actions,
		metadata: item.metadata
	};
	return hitlOpenTargetFromFeedItem(feedItem);
}
