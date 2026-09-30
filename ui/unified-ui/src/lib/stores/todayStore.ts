import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

import { getV2EventSequence, v2Events, type V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { timedFetch } from '$lib/shared/fetch';
import { showError } from '$lib/shared/stores/notifications';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { readerLocalDate } from '$lib/stores/taskStore';
import type {
	TodayChangedDigest,
	TodayCounts,
	TodayFreshness,
	TodayItem,
	TodayResponse,
	TodaySectionId,
	TodaySections,
	TodayVisibilityListItem,
	TodayVisibilityListResponse,
	TodayVisibilitySnapshot
} from '$lib/today/types';

const POLL_INTERVAL_MS = 30_000;
const REALTIME_DEBOUNCE_MS = 750;
const DEFAULT_PER_SECTION = 8;
const DEFAULT_SECTION_LIMIT = 8;
const DEFAULT_DIGEST_LIMIT = 7;
const MAX_SECTION_LIMIT = 50;
const MAX_DIGEST_LIMIT = 50;
const MAX_DIGEST_OFFSET = 1_000;

export interface TodayStoreQuery {
	per_section?: number;
	section?: TodaySectionId;
	limit?: number;
	cursor?: string | null;
	digest_limit?: number;
	digest_offset?: number;
}

export type TodayVisibilityAction = 'mark_seen' | 'dismiss' | 'snooze' | 'restore';

export type SnoozeOption = 'tonight' | 'tomorrow_morning' | 'next_week';

/**
 * Pure snooze-duration computation for the Today snooze menu.
 *
 * - `tonight`: until 18:00 today, or now + 3h when 18:00 has already passed.
 * - `tomorrow_morning`: until the next 08:00 (today's 08:00 when still ahead).
 * - `next_week`: until the next Monday 08:00 (a full week out when today is
 *   Monday).
 *
 * `now` is injected — no `Date.now()` inside — so callers and tests control
 * the clock. Returns whole minutes (minimum 1).
 */
export function snoozeMinutesFor(option: SnoozeOption, now: Date): number {
	const target = new Date(now.getTime());
	switch (option) {
		case 'tonight': {
			target.setHours(18, 0, 0, 0);
			if (target.getTime() <= now.getTime()) {
				return 3 * 60;
			}
			break;
		}
		case 'tomorrow_morning': {
			target.setHours(8, 0, 0, 0);
			if (target.getTime() <= now.getTime()) {
				target.setDate(target.getDate() + 1);
			}
			break;
		}
		case 'next_week': {
			// getDay(): 0 = Sunday .. 6 = Saturday; Monday = 1.
			const daysUntilMonday = (8 - now.getDay()) % 7 || 7;
			target.setDate(target.getDate() + daysUntilMonday);
			target.setHours(8, 0, 0, 0);
			break;
		}
	}
	return Math.max(1, Math.round((target.getTime() - now.getTime()) / 60_000));
}

const TODAY_SECTION_IDS: TodaySectionId[] = [
	'needs_you',
	'delivered',
	'changed',
	'active_work',
	'followups'
];

interface RemovedTodayItem {
	section: TodaySectionId;
	index: number;
	item: TodayItem;
}

/**
 * Removes optimistically-hidden rows from a refetched payload's sections
 * AND decrements the payload counts by exactly what was stripped (clamped
 * at 0), so the applied counts never transiently overcount the visible
 * rows while a hide POST is still in flight.
 */
function stripHiddenRows(
	sections: TodaySections,
	counts: TodayCounts,
	hiddenIds: ReadonlySet<string>
): { sections: TodaySections; counts: TodayCounts } {
	const nextSections: TodaySections = { ...sections };
	const nextCounts: TodayCounts = { ...counts };
	let removedTotal = 0;
	for (const sectionId of TODAY_SECTION_IDS) {
		const items = sections[sectionId];
		const kept = items.filter((item) => !hiddenIds.has(item.id));
		if (kept.length === items.length) continue;
		const removed = items.length - kept.length;
		nextSections[sectionId] = kept;
		nextCounts[sectionId] = Math.max(0, nextCounts[sectionId] - removed);
		removedTotal += removed;
	}
	nextCounts.total = Math.max(0, nextCounts.total - removedTotal);
	return { sections: nextSections, counts: nextCounts };
}

interface TodayFetchOptions {
	digestRefresh?: boolean;
}

export interface TodayStoreState {
	isLoading: boolean;
	error: string | null;
	lastLoadedAt: number | null;
	query: TodayStoreQuery;
	loadedSectionPageKeys: Record<TodaySectionId, string | null>;
	loadedDigestPageKey: string | null;
	sectionPage: TodayResponse['section_page'] | null;
	principal: string | null;
	workspace: string | null;
	generated_at: number | null;
	freshness: TodayFreshness | null;
	headline: string;
	digest: TodayChangedDigest;
	sections: TodaySections;
	counts: TodayCounts;
	hiddenItems: TodayVisibilityListItem[];
}

function emptySections(): TodaySections {
	return {
		needs_you: [],
		delivered: [],
		changed: [],
		active_work: [],
		followups: []
	};
}

function emptyCounts(): TodayCounts {
	return {
		needs_you: 0,
		delivered: 0,
		changed: 0,
		active_work: 0,
		followups: 0,
		total: 0
	};
}

function emptyLoadedSectionPageKeys(): Record<TodaySectionId, string | null> {
	return {
		needs_you: null,
		delivered: null,
		changed: null,
		active_work: null,
		followups: null
	};
}

function emptyDigest(): TodayChangedDigest {
	return {
		generated_at: 0,
		since: null,
		total: 0,
		limit: DEFAULT_DIGEST_LIMIT,
		offset: 0,
		bullets: []
	};
}

function normalizeStringArray(value: unknown): string[] {
	if (!Array.isArray(value)) return [];
	return value
		.filter((item): item is string => typeof item === 'string')
		.map((item) => item.trim())
		.filter(Boolean);
}

function normalizeTodayItem(item: TodayItem): TodayItem {
	const raw = item as TodayItem & Record<string, unknown>;
	return {
		...item,
		space_ids: normalizeStringArray(raw.space_ids),
		actions: Array.isArray(raw.actions) ? item.actions : [],
		evidence_refs: Array.isArray(raw.evidence_refs) ? item.evidence_refs : [],
		metadata: raw.metadata ?? null
	};
}

function normalizeTodaySections(sections: TodayResponse['sections'] | null | undefined): TodaySections {
	return {
		needs_you: Array.isArray(sections?.needs_you)
			? sections.needs_you.map(normalizeTodayItem)
			: [],
		delivered: Array.isArray(sections?.delivered)
			? sections.delivered.map(normalizeTodayItem)
			: [],
		changed: Array.isArray(sections?.changed)
			? sections.changed.map(normalizeTodayItem)
			: [],
		active_work: Array.isArray(sections?.active_work)
			? sections.active_work.map(normalizeTodayItem)
			: [],
		followups: Array.isArray(sections?.followups)
			? sections.followups.map(normalizeTodayItem)
			: []
	};
}

function normalizeTodayDigest(
	digest: TodayResponse['digest'] | null | undefined,
	generatedAt: number
): TodayChangedDigest {
	const raw = digest as (TodayChangedDigest & Record<string, unknown>) | null | undefined;
	return {
		generated_at:
			typeof raw?.generated_at === 'number' && Number.isFinite(raw.generated_at)
				? raw.generated_at
				: generatedAt,
		since: typeof raw?.since === 'number' && Number.isFinite(raw.since) ? raw.since : null,
		total:
			typeof raw?.total === 'number' && Number.isFinite(raw.total)
				? Math.max(0, Math.floor(raw.total))
				: Array.isArray(raw?.bullets)
					? raw.bullets.length
					: 0,
		limit:
			typeof raw?.limit === 'number' && Number.isFinite(raw.limit)
				? Math.max(1, Math.floor(raw.limit))
				: DEFAULT_DIGEST_LIMIT,
		offset:
			typeof raw?.offset === 'number' && Number.isFinite(raw.offset)
				? Math.max(0, Math.floor(raw.offset))
				: 0,
		bullets: Array.isArray(raw?.bullets)
			? raw.bullets.map((bullet, index) => {
					const item = bullet as TodayChangedDigest['bullets'][number] & Record<string, unknown>;
					return {
						id: typeof item.id === 'string' && item.id.trim() ? item.id : `today:digest:${index}`,
						text: typeof item.text === 'string' ? item.text : '',
						source_kind: typeof item.source_kind === 'string' ? item.source_kind : 'unknown',
						source_id: typeof item.source_id === 'string' ? item.source_id : '',
						source_url: typeof item.source_url === 'string' ? item.source_url : null,
						space_ids: normalizeStringArray(item.space_ids),
						updated_at:
							typeof item.updated_at === 'number' && Number.isFinite(item.updated_at)
								? item.updated_at
								: generatedAt
					};
				})
			: []
	};
}

function isTodaySectionId(value: unknown): value is TodaySectionId {
	return typeof value === 'string' && TODAY_SECTION_IDS.includes(value as TodaySectionId);
}

function defaultState(query: TodayStoreQuery): TodayStoreState {
	return {
		isLoading: false,
		error: null,
		lastLoadedAt: null,
		query,
		loadedSectionPageKeys: emptyLoadedSectionPageKeys(),
		loadedDigestPageKey: null,
		sectionPage: null,
		principal: null,
		workspace: null,
		generated_at: null,
		freshness: null,
		headline: 'Nothing needs you right now.',
		digest: emptyDigest(),
		sections: emptySections(),
		counts: emptyCounts(),
		hiddenItems: []
	};
}

function sectionPageKey(query: TodayStoreQuery): string {
	const normalized = normalizeQuery(query);
	const limit = normalized.limit ?? normalized.per_section ?? DEFAULT_SECTION_LIMIT;
	const cursor = normalized.cursor ?? '';
	return `${limit}:${cursor}`;
}

function digestPageKey(query: TodayStoreQuery): string {
	const normalized = normalizeQuery(query);
	const limit = normalized.digest_limit ?? DEFAULT_DIGEST_LIMIT;
	const offset = normalized.digest_offset ?? 0;
	return `${limit}:${offset}`;
}

function normalizeQuery(query: TodayStoreQuery = {}): TodayStoreQuery {
	const perSection =
		typeof query.per_section === 'number' && Number.isFinite(query.per_section)
			? Math.floor(query.per_section)
			: DEFAULT_PER_SECTION;
	const limit =
		typeof query.limit === 'number' && Number.isFinite(query.limit)
			? Math.floor(query.limit)
			: DEFAULT_SECTION_LIMIT;
	const cursor = typeof query.cursor === 'string' && query.cursor.trim() ? query.cursor : null;
	const digestLimit =
		typeof query.digest_limit === 'number' && Number.isFinite(query.digest_limit)
			? Math.floor(query.digest_limit)
			: DEFAULT_DIGEST_LIMIT;
	const digestOffset =
		typeof query.digest_offset === 'number' && Number.isFinite(query.digest_offset)
			? Math.floor(query.digest_offset)
			: 0;
	return {
		per_section: Math.max(1, Math.min(20, perSection)),
		digest_limit: Math.max(1, Math.min(MAX_DIGEST_LIMIT, digestLimit)),
		digest_offset: Math.max(0, Math.min(MAX_DIGEST_OFFSET, digestOffset)),
		...(isTodaySectionId(query.section)
			? {
					section: query.section,
					limit: Math.max(1, Math.min(MAX_SECTION_LIMIT, limit)),
					cursor
				}
			: {})
	};
}

function queryKey(query: TodayStoreQuery): string {
	const normalized = normalizeQuery(query);
	return JSON.stringify({
		per_section: normalized.per_section ?? DEFAULT_PER_SECTION,
		section: normalized.section ?? null,
		limit: normalized.limit ?? null,
		cursor: normalized.cursor ?? null,
		digest_limit: normalized.digest_limit ?? DEFAULT_DIGEST_LIMIT,
		digest_offset: normalized.digest_offset ?? 0
	});
}

function currentScopeKey(): string {
	const scope = get(scopeIdentityStore);
	return `${scope.principal}:${scope.workspace}`;
}

function buildQueryString(query: TodayStoreQuery, options: TodayFetchOptions = {}): string {
	const params = new URLSearchParams();
	const scope = get(scopeIdentityStore);
	if (scope.principal.trim().length > 0) {
	}
	if (scope.workspace.trim().length > 0) {
	}
	// The date the reader is actually looking at. Every Today predicate — due
	// today, changed since, overdue — is a date comparison, and the server has
	// no idea where the reader is: left to its own clock it answers from the UTC
	// date, which names the wrong day for part of every day anywhere east of
	// Greenwich. Shared with the tasks list rather than derived again here;
	// `readerLocalDate()` reads the local calendar fields precisely because
	// rendering a local instant through `toISOString()` is the bug it replaced.
	params.set('today', readerLocalDate());
	if (typeof query.per_section === 'number' && Number.isFinite(query.per_section)) {
		params.set('per_section', String(Math.floor(query.per_section)));
	}
	if (isTodaySectionId(query.section)) {
		params.set('section', query.section);
		if (typeof query.limit === 'number' && Number.isFinite(query.limit)) {
			params.set('limit', String(Math.floor(query.limit)));
		}
		if (typeof query.cursor === 'string' && query.cursor.trim().length > 0) {
			params.set('cursor', query.cursor);
		}
	}
	if (typeof query.digest_limit === 'number' && Number.isFinite(query.digest_limit)) {
		params.set('digest_limit', String(Math.floor(query.digest_limit)));
	}
	if (typeof query.digest_offset === 'number' && Number.isFinite(query.digest_offset)) {
		params.set('digest_offset', String(Math.floor(query.digest_offset)));
	}
	if (options.digestRefresh) {
		params.set('digest_refresh', 'true');
	}
	return params.toString();
}

function buildScopeQueryString(): string {
	const params = new URLSearchParams();
	const scope = get(scopeIdentityStore);
	if (scope.principal.trim().length > 0) {
	}
	if (scope.workspace.trim().length > 0) {
	}
	return params.toString();
}

function normalizeErrorMessage(error: unknown): string {
	if (error instanceof Error && error.message.trim().length > 0) {
		return normalizeApiErrorMessage(error.message);
	}
	return 'Failed to load Today';
}

function normalizeApiErrorMessage(message: string): string {
	const trimmed = message.trim();
	if (!trimmed.startsWith('{')) return trimmed;
	try {
		const payload = JSON.parse(trimmed) as { message?: unknown; error?: unknown };
		if (typeof payload.message === 'string' && payload.message.trim().length > 0) {
			return payload.message.trim();
		}
		if (typeof payload.error === 'string' && payload.error.trim().length > 0) {
			return payload.error.trim();
		}
	} catch {
		// Fall through to the original text when the backend response is not JSON.
	}
	return trimmed;
}

function isFeedDeltaEvent(event: V2WebSocketEvent): boolean {
	return (
		event.event_type === 'FeedItemCreated'
		|| event.event_type === 'FeedItemUpdated'
		|| event.event_type === 'FeedItemRemoved'
	);
}

function isHitlCanonicalEvent(event: V2WebSocketEvent): boolean {
	const eventType = String(event.event_type);
	return eventType === 'HitlRequested' || eventType === 'HitlResolved';
}

function feedEventMatchesCurrentScope(event: V2WebSocketEvent): boolean {
	const scope = get(scopeIdentityStore);
	switch (event.event_type) {
		case 'FeedItemCreated':
			return (
				event.data.item.principal === scope.principal
				&& event.data.item.workspace === scope.workspace
			);
		case 'FeedItemUpdated':
		case 'FeedItemRemoved':
			return (
				event.data.principal === scope.principal
				&& event.data.workspace === scope.workspace
			);
		default:
			return false;
	}
}

function applyTodayPayload(
	state: TodayStoreState,
	payload: TodayResponse,
	hiddenItems?: TodayVisibilityListItem[] | null
): TodayStoreState {
	const nextSections = normalizeTodaySections(payload.sections);
	const loadedSectionPageKeys = { ...state.loadedSectionPageKeys };
	const sections = isTodaySectionId(state.query.section)
		? {
				...state.sections,
				[state.query.section]: nextSections[state.query.section]
			}
		: nextSections;
	if (isTodaySectionId(state.query.section)) {
		loadedSectionPageKeys[state.query.section] = sectionPageKey(state.query);
	} else {
		const key = sectionPageKey(state.query);
		for (const sectionId of TODAY_SECTION_IDS) {
			loadedSectionPageKeys[sectionId] = key;
		}
	}
	return {
		...state,
		isLoading: false,
		error: null,
		lastLoadedAt: Date.now(),
		loadedSectionPageKeys,
		loadedDigestPageKey: digestPageKey(state.query),
		sectionPage: payload.section_page ?? null,
		principal: payload.principal,
		workspace: payload.workspace,
		generated_at: payload.generated_at,
		freshness: payload.freshness,
		headline: payload.headline,
		digest: normalizeTodayDigest(payload.digest, payload.generated_at),
		sections,
		counts: payload.counts,
		hiddenItems: hiddenItems ?? state.hiddenItems
	};
}

export function createTodayStore(initialQuery: TodayStoreQuery = {}) {
	const normalizedQuery = normalizeQuery(initialQuery);
	const { subscribe, update } = writable<TodayStoreState>(defaultState(normalizedQuery));

	let activeConsumers = 0;
	let pollHandle: ReturnType<typeof setInterval> | null = null;
	let realtimeUnsubscribe: (() => void) | null = null;
	let scopeUnsubscribe: (() => void) | null = null;
	let debounceHandle: ReturnType<typeof setTimeout> | null = null;
	let lastObservedEventSequence = 0;
	let inFlightRefresh: Promise<void> | null = null;
	let queuedRefresh = false;
	let queuedDigestRefresh = false;
	let currentQuery = normalizedQuery;
	let lastScopeKey = '';
	let hiddenItemsRefreshToken = 0;
	// 1-deep undo buffer for the most recent optimistic dismiss/snooze.
	// `completion` never rejects: it resolves true when the hide POST landed,
	// false when it failed (the failure path already restored the row).
	let lastHidden: { itemId: string; completion: Promise<boolean> } | null = null;
	// Item ids optimistically removed whose hide POST hasn't settled yet — a
	// concurrent poll/WS refetch that raced the POST must not re-add them.
	const pendingHiddenItemIds = new Set<string>();

	async function fetchToday(
		query: TodayStoreQuery,
		options: TodayFetchOptions = {}
	): Promise<TodayResponse> {
		const queryString = buildQueryString(query, options);
		const suffix = queryString ? `?${queryString}` : '';
		const response = await timedFetch(`/api/magician/v2/today${suffix}`);
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to load Today (${response.status})`);
		}
		return (await response.json()) as TodayResponse;
	}

	async function fetchTodayVisibility(): Promise<TodayVisibilityListResponse> {
		const queryString = buildScopeQueryString();
		const suffix = queryString ? `?${queryString}` : '';
		const response = await timedFetch(`/api/magician/v2/today/visibility${suffix}`, {
			timeoutMs: 8_000
		});
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to load hidden Today items (${response.status})`);
		}
		return (await response.json()) as TodayVisibilityListResponse;
	}

	async function refreshHiddenItems(expectedScopeKey: string, expectedQueryKey: string): Promise<void> {
		const token = ++hiddenItemsRefreshToken;
		try {
			const visibilityPayload = await fetchTodayVisibility();
			if (
				token !== hiddenItemsRefreshToken
				|| expectedScopeKey !== currentScopeKey()
				|| expectedQueryKey !== queryKey(currentQuery)
			) {
				return;
			}
			update((state) => ({
				...state,
				hiddenItems: visibilityPayload.items
			}));
		} catch {
			// Hidden-item restore state is secondary. A slow or failed
			// visibility read must never keep the main Today sections in
			// their initial loading state.
		}
	}

	async function postVisibility(
		itemId: string,
		action: TodayVisibilityAction,
		options: {
			snoozeMinutes?: number;
			snoozeUntil?: number;
			snapshot?: TodayVisibilitySnapshot | null;
		} = {}
	): Promise<void> {
		const queryString = buildScopeQueryString();
		const suffix = queryString ? `?${queryString}` : '';
		const body: Record<string, unknown> = { action };
		if (typeof options.snoozeMinutes === 'number' && Number.isFinite(options.snoozeMinutes)) {
			body.snooze_minutes = Math.floor(options.snoozeMinutes);
		}
		if (typeof options.snoozeUntil === 'number' && Number.isFinite(options.snoozeUntil)) {
			body.snooze_until = Math.floor(options.snoozeUntil);
		}
		if (options.snapshot) {
			body.snapshot = options.snapshot;
		}
		const response = await timedFetch(
			`/api/magician/v2/today/items/${encodeURIComponent(itemId)}/visibility${suffix}`,
			{
				method: 'POST',
				headers: {
					'content-type': 'application/json'
				},
				body: JSON.stringify(body)
			}
		);
		if (!response.ok) {
			const text = await response.text().catch(() => '');
			throw new Error(text || `Failed to update Today item (${response.status})`);
		}
	}

	async function updateVisibility(
		itemId: string,
		action: TodayVisibilityAction,
		options: {
			snoozeMinutes?: number;
			snoozeUntil?: number;
			snapshot?: TodayVisibilitySnapshot | null;
		} = {}
	): Promise<void> {
		if (!browser) return;
		if (action !== 'dismiss' && action !== 'snooze') {
			// `mark_seen` and `restore` keep the original POST-then-refetch
			// path: `mark_seen` doesn't remove rows, and the Hidden-panel
			// restore relies on the immediate refetch to bring the row back.
			await postVisibility(itemId, action, options);
			await runRefresh();
			return;
		}

		// Optimistic dismiss/snooze: remove the row locally right now, then
		// reconcile the active server-side page once the POST lands. That keeps
		// the cursor-backed server page aligned after rows shift forward.
		const actionScopeKey = currentScopeKey();
		const actionQueryKey = queryKey(currentQuery);
		let removed: RemovedTodayItem | null = null;
		update((state) => {
			for (const sectionId of TODAY_SECTION_IDS) {
				const items = state.sections[sectionId];
				const index = items.findIndex((entry) => entry.id === itemId);
				if (index === -1) continue;
				removed = { section: sectionId, index, item: items[index] };
				const sections: TodaySections = { ...state.sections };
				sections[sectionId] = [...items.slice(0, index), ...items.slice(index + 1)];
				const counts: TodayCounts = { ...state.counts };
				counts[sectionId] = Math.max(0, counts[sectionId] - 1);
				counts.total = Math.max(0, counts.total - 1);
				return { ...state, sections, counts };
			}
			return state;
		});
		pendingHiddenItemIds.add(itemId);

		const completion = (async () => {
			try {
				await postVisibility(itemId, action, options);
				pendingHiddenItemIds.delete(itemId);
				if (actionScopeKey === currentScopeKey() && actionQueryKey === queryKey(currentQuery)) {
					await runRefresh();
				}
				return true;
			} catch (error) {
				pendingHiddenItemIds.delete(itemId);
				if (lastHidden?.itemId === itemId) {
					lastHidden = null;
				}
				// Scope guard: a late failure must never restore the row into
				// a different principal/workspace's (or query's) state — the
				// scope bridge already reset it.
				const stillCurrent =
					actionScopeKey === currentScopeKey() && actionQueryKey === queryKey(currentQuery);
				// TS control-flow analysis can't see the assignment inside the
				// update() callback above and keeps `removed` narrowed to its
				// initial null (which then collapses to `never` after the truthy
				// check) — reassert the real type.
				const restore = removed as RemovedTodayItem | null;
				if (restore && stillCurrent) {
					update((state) => {
						const items = state.sections[restore.section];
						if (items.some((entry) => entry.id === restore.item.id)) return state;
						const index = Math.min(restore.index, items.length);
						const sections: TodaySections = { ...state.sections };
						sections[restore.section] = [
							...items.slice(0, index),
							restore.item,
							...items.slice(index)
						];
						const counts: TodayCounts = { ...state.counts };
						counts[restore.section] = counts[restore.section] + 1;
						counts.total = counts.total + 1;
						return { ...state, sections, counts };
					});
				}
				if (stillCurrent) {
					showError(
						action === 'dismiss'
							? 'Failed to dismiss Today item'
							: 'Failed to snooze Today item',
						normalizeErrorMessage(error)
					);
				}
				return false;
			}
		})();
		lastHidden = { itemId, completion };
	}

	async function undoLastHidden(): Promise<boolean> {
		const entry = lastHidden;
		if (!entry) return false;
		lastHidden = null;
		const undoScopeKey = currentScopeKey();
		// Ordering: the hide POST may still be in flight. `completion`
		// settles (never rejects) once the background POST — and any
		// failure-restore — is done, so awaiting it first guarantees the
		// restore POST reaches the server after the hide it undoes.
		const hidden = await entry.completion;
		if (!hidden) return false;
		// Scope guard: the principal/workspace may have switched while we
		// awaited the hide POST — a restore now would target the wrong
		// scope's item, so bail without POSTing.
		if (undoScopeKey !== currentScopeKey()) return false;
		// Reuse the existing restore mechanism (POST + refetch), same as the
		// Hidden-panel restore.
		await updateVisibility(entry.itemId, 'restore');
		return true;
	}

	async function runRefresh(options: TodayFetchOptions = {}): Promise<void> {
		if (!browser) return;
		if (inFlightRefresh) {
			queuedRefresh = true;
			queuedDigestRefresh = queuedDigestRefresh || options.digestRefresh === true;
			return inFlightRefresh;
		}

		const refreshScopeKey = currentScopeKey();
		const refreshQuery = currentQuery;
		const refreshQueryKey = queryKey(refreshQuery);

		update((state) => ({
			...state,
			isLoading: state.lastLoadedAt === null || options.digestRefresh === true,
			error: null
		}));

		inFlightRefresh = (async () => {
			try {
				const payload = await fetchToday(refreshQuery, options);
				if (refreshScopeKey !== currentScopeKey() || refreshQueryKey !== queryKey(currentQuery)) {
					return;
				}
				scopeIdentityStore.observe(payload.principal, payload.workspace);
				update((state) => {
					const next = applyTodayPayload(state, payload);
					// Keep optimistically-hidden rows out of a refetch that
					// raced their still-in-flight hide POST — and keep the
					// applied counts in step with the stripped rows.
					if (pendingHiddenItemIds.size === 0) return next;
					const stripped = stripHiddenRows(next.sections, next.counts, pendingHiddenItemIds);
					return { ...next, sections: stripped.sections, counts: stripped.counts };
				});
				if (refreshScopeKey === currentScopeKey() && refreshQueryKey === queryKey(currentQuery)) {
					void refreshHiddenItems(refreshScopeKey, refreshQueryKey);
				}
			} catch (error) {
				if (refreshScopeKey !== currentScopeKey() || refreshQueryKey !== queryKey(currentQuery)) {
					return;
				}
				update((state) => ({
					...state,
					isLoading: false,
					error: normalizeErrorMessage(error)
				}));
			} finally {
				inFlightRefresh = null;
				if (queuedRefresh) {
					const nextDigestRefresh = queuedDigestRefresh;
					queuedRefresh = false;
					queuedDigestRefresh = false;
					void runRefresh({ digestRefresh: nextDigestRefresh });
				}
			}
		})();

		return inFlightRefresh;
	}

	function scheduleRefresh(): void {
		if (!browser) return;
		if (debounceHandle) {
			clearTimeout(debounceHandle);
		}
		debounceHandle = setTimeout(() => {
			debounceHandle = null;
			void runRefresh();
		}, REALTIME_DEBOUNCE_MS);
	}

	function startPolling(): void {
		if (pollHandle || !browser) return;
		pollHandle = setInterval(() => {
			void runRefresh();
		}, POLL_INTERVAL_MS);
	}

	function stopPolling(): void {
		if (!pollHandle) return;
		clearInterval(pollHandle);
		pollHandle = null;
	}

	function startRealtimeBridge(): void {
		if (realtimeUnsubscribe || !browser) return;
		realtimeUnsubscribe = v2Events.subscribe((events) => {
			let nextSequence = lastObservedEventSequence;
			let shouldRefresh = false;

			for (const event of events) {
				const sequence = getV2EventSequence(event);
				if (sequence <= lastObservedEventSequence) continue;
				nextSequence = Math.max(nextSequence, sequence);
				if (isFeedDeltaEvent(event) && feedEventMatchesCurrentScope(event)) {
					shouldRefresh = true;
					continue;
				}
				if (isHitlCanonicalEvent(event)) {
					shouldRefresh = true;
				}
			}

			if (nextSequence <= lastObservedEventSequence) return;
			lastObservedEventSequence = nextSequence;
			if (shouldRefresh) {
				scheduleRefresh();
			}
		});
	}

	function startScopeBridge(): void {
		if (scopeUnsubscribe || !browser) return;
		lastScopeKey = currentScopeKey();
		scopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
			const scopeKey = `${scope.principal}:${scope.workspace}`;
			if (scopeKey === lastScopeKey) return;
			lastScopeKey = scopeKey;
			lastObservedEventSequence = 0;
			hiddenItemsRefreshToken += 1;
			// Undo/optimistic bookkeeping belongs to the previous scope —
			// never carry it across a principal/workspace switch.
			lastHidden = null;
			pendingHiddenItemIds.clear();
			update(() => defaultState(currentQuery));
			if (activeConsumers > 0) {
				void runRefresh();
			}
		});
	}

	function stopRealtimeBridge(): void {
		if (realtimeUnsubscribe) {
			realtimeUnsubscribe();
			realtimeUnsubscribe = null;
		}
		if (scopeUnsubscribe) {
			scopeUnsubscribe();
			scopeUnsubscribe = null;
		}
		if (debounceHandle) {
			clearTimeout(debounceHandle);
			debounceHandle = null;
		}
	}

	function setQuery(query: TodayStoreQuery): void {
		currentQuery = normalizeQuery(query);
		update((state) => ({
			...state,
			query: currentQuery
		}));
		if (activeConsumers > 0) {
			void runRefresh();
		}
	}

	return {
		subscribe,
		start(): void {
			activeConsumers += 1;
			if (activeConsumers !== 1) return;
			if (v2Events.getConnectionState() === 'CLOSED') {
				v2Events.connectGlobal();
			}
			startPolling();
			startRealtimeBridge();
			startScopeBridge();
			void runRefresh();
		},
		stop(): void {
			activeConsumers = Math.max(0, activeConsumers - 1);
			if (activeConsumers !== 0) return;
			hiddenItemsRefreshToken += 1;
			stopPolling();
			stopRealtimeBridge();
		},
		refresh(): Promise<void> {
			return runRefresh();
		},
		refreshDigest(): Promise<void> {
			return runRefresh({ digestRefresh: true });
		},
		updateVisibility,
		undoLastHidden,
		setQuery,
		clearError(): void {
			update((state) => ({ ...state, error: null }));
		}
	};
}

export const todayStore = createTodayStore();
