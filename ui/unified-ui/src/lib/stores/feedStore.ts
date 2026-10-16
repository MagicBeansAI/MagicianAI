import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

import type { FeedCounts, FeedItem } from '$lib/feed/types';
import { getV2EventSequence, v2Events, type V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

const POLL_INTERVAL_MS = 15_000;
const REALTIME_DEBOUNCE_MS = 600;

export interface FeedStoreQuery {
	ui_thread_id?: string | null;
	limit?: number;
}

export interface FeedStoreState {
	isLoading: boolean;
	error: string | null;
	lastLoadedAt: number | null;
	query: FeedStoreQuery;
	items: FeedItem[];
	counts: FeedCounts;
}

function emptyCounts(): FeedCounts {
	return {
		total: 0,
		running: 0,
		needs_action: 0,
		failed: 0,
		done: 0,
		info: 0
	};
}

const defaultQuery: FeedStoreQuery = {
	ui_thread_id: null,
	limit: 60
};

function normalizeErrorMessage(error: unknown): string {
	if (error instanceof Error && error.message.trim().length > 0) {
		return normalizeApiErrorMessage(error.message);
	}
	return 'Failed to load feed';
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

function normalizeQuery(query: FeedStoreQuery): FeedStoreQuery {
	return {
		...defaultQuery,
		...query
	};
}

function queryKey(query: FeedStoreQuery): string {
	const normalized = normalizeQuery(query);
	return JSON.stringify({
		ui_thread_id: normalized.ui_thread_id ?? null,
		limit: typeof normalized.limit === 'number' && Number.isFinite(normalized.limit)
			? Math.floor(normalized.limit)
			: null
	});
}

function currentScopeKey(): string {
	const scope = get(scopeIdentityStore);
	return `${scope.principal}:${scope.workspace}`;
}

function buildQueryString(query: FeedStoreQuery): string {
	const params = new URLSearchParams();
	if (query.ui_thread_id && query.ui_thread_id.trim().length > 0) {
		params.set('ui_thread_id', query.ui_thread_id.trim());
	}
	if (typeof query.limit === 'number' && Number.isFinite(query.limit) && query.limit > 0) {
		params.set('limit', String(Math.floor(query.limit)));
	}
	return params.toString();
}

function isFeedDeltaEvent(event: V2WebSocketEvent): boolean {
	return (
		event.event_type === 'FeedItemCreated'
		|| event.event_type === 'FeedItemUpdated'
		|| event.event_type === 'FeedItemRemoved'
	);
}

function scopeMatches(principal: string, workspace: string): boolean {
	const scope = get(scopeIdentityStore);
	return scope.principal === principal && scope.workspace === workspace;
}

function matchesQuery(item: FeedItem, query: FeedStoreQuery): boolean {
	if (query.ui_thread_id && item.ui_thread_id !== query.ui_thread_id) {
		return false;
	}
	return true;
}

function sortFeedItems(items: FeedItem[]): FeedItem[] {
	return [...items].sort((left, right) => {
		if (right.updated_at !== left.updated_at) {
			return right.updated_at - left.updated_at;
		}
		return right.created_at - left.created_at;
	});
}

function clampFeedItems(items: FeedItem[], query: FeedStoreQuery): FeedItem[] {
	const sorted = sortFeedItems(items);
	if (typeof query.limit === 'number' && Number.isFinite(query.limit) && query.limit > 0) {
		return sorted.slice(0, Math.floor(query.limit));
	}
	return sorted;
}

function adjustCountsForItem(counts: FeedCounts, item: Pick<FeedItem, 'status'>, delta: 1 | -1): FeedCounts {
	const next = {
		...counts,
		total: Math.max(0, counts.total + delta)
	};
	switch (item.status) {
		case 'running':
			next.running = Math.max(0, next.running + delta);
			break;
		case 'needs_action':
			next.needs_action = Math.max(0, next.needs_action + delta);
			break;
		case 'failed':
			next.failed = Math.max(0, next.failed + delta);
			break;
		case 'done':
			next.done = Math.max(0, next.done + delta);
			break;
		case 'info':
			next.info = Math.max(0, next.info + delta);
			break;
	}
	return next;
}

function applyPatch(item: FeedItem, patch: Record<string, unknown>): FeedItem {
	return {
		...item,
		title: typeof patch.title === 'string' ? patch.title : item.title,
		summary: Object.prototype.hasOwnProperty.call(patch, 'summary')
			? (patch.summary as FeedItem['summary'])
			: item.summary,
		status: typeof patch.status === 'string' ? (patch.status as FeedItem['status']) : item.status,
		task_id: Object.prototype.hasOwnProperty.call(patch, 'task_id')
			? (patch.task_id as FeedItem['task_id'])
			: item.task_id,
		ui_thread_id: Object.prototype.hasOwnProperty.call(patch, 'ui_thread_id')
			? (patch.ui_thread_id as FeedItem['ui_thread_id'])
			: item.ui_thread_id,
		agent_id: Object.prototype.hasOwnProperty.call(patch, 'agent_id')
			? (patch.agent_id as FeedItem['agent_id'])
			: item.agent_id,
		updated_at: typeof patch.updated_at === 'number' ? patch.updated_at : item.updated_at,
		metadata: Object.prototype.hasOwnProperty.call(patch, 'metadata')
			? patch.metadata
			: item.metadata
	};
}

export function createFeedStore(initialQuery: FeedStoreQuery = {}) {
	const mergedQuery = normalizeQuery(initialQuery);

	const { subscribe, update } = writable<FeedStoreState>({
		isLoading: false,
		error: null,
		lastLoadedAt: null,
		query: mergedQuery,
		items: [],
		counts: emptyCounts()
	});

	let activeConsumers = 0;
	let pollHandle: ReturnType<typeof setInterval> | null = null;
	let realtimeUnsubscribe: (() => void) | null = null;
	let scopeUnsubscribe: (() => void) | null = null;
	let debounceHandle: ReturnType<typeof setTimeout> | null = null;
	let lastObservedEventSequence = 0;
	let inFlightRefresh: Promise<void> | null = null;
	let queuedRefresh = false;
	let currentQuery = mergedQuery;
	let lastScopeKey = '';

	async function fetchFeedState(query: FeedStoreQuery): Promise<{ items: FeedItem[]; counts: FeedCounts }> {
		const queryString = buildQueryString(query);
		const suffix = queryString ? `?${queryString}` : '';
		const [itemsResponse, countsResponse] = await Promise.all([
			fetch(`/api/magician/v2/feed${suffix}`),
			fetch(`/api/magician/v2/feed/counts${suffix}`)
		]);

		if (!itemsResponse.ok) {
			const body = await itemsResponse.text().catch(() => '');
			throw new Error(body || `Failed to load feed (${itemsResponse.status})`);
		}
		if (!countsResponse.ok) {
			const body = await countsResponse.text().catch(() => '');
			throw new Error(body || `Failed to load feed counts (${countsResponse.status})`);
		}

		const itemsPayload = (await itemsResponse.json()) as { items?: FeedItem[] };
		const countsPayload = (await countsResponse.json()) as { counts?: FeedCounts };
		return {
			items: itemsPayload.items || [],
			counts: countsPayload.counts || emptyCounts()
		};
	}

	async function runRefresh(): Promise<void> {
		if (!browser) return;
		if (inFlightRefresh) {
			queuedRefresh = true;
			return inFlightRefresh;
		}

		const refreshScopeKey = currentScopeKey();
		const refreshQuery = currentQuery;
		const refreshQueryKey = queryKey(refreshQuery);

		update((state) => ({
			...state,
			isLoading: state.lastLoadedAt === null,
			error: null
		}));

		inFlightRefresh = (async () => {
			try {
				const payload = await fetchFeedState(refreshQuery);
				if (refreshScopeKey !== currentScopeKey() || refreshQueryKey !== queryKey(currentQuery)) {
					return;
				}
				const firstItem = payload.items[0];
				if (firstItem) {
					scopeIdentityStore.observe(firstItem.principal, firstItem.workspace);
				}
				update((state) => ({
					...state,
					isLoading: false,
					error: null,
					lastLoadedAt: Date.now(),
					query: refreshQuery,
					items: payload.items,
					counts: payload.counts
				}));
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
					queuedRefresh = false;
					void runRefresh();
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
		if (pollHandle) {
			clearInterval(pollHandle);
			pollHandle = null;
		}
	}

	function startRealtimeBridge(): void {
		if (realtimeUnsubscribe || !browser) return;
		realtimeUnsubscribe = v2Events.subscribe((events) => {
			let nextSequence = lastObservedEventSequence;
			let requiresRefresh = false;

			for (const event of events) {
				const sequence = getV2EventSequence(event);
				if (sequence <= lastObservedEventSequence) continue;
				nextSequence = Math.max(nextSequence, sequence);
				if (!isFeedDeltaEvent(event)) continue;

				update((state) => {
					switch (event.event_type) {
						case 'FeedItemCreated': {
							const item = event.data.item;
							if (!scopeMatches(item.principal, item.workspace)) {
								return state;
							}
							let counts = state.counts;
							let items = state.items;
							if (matchesQuery(item, currentQuery)) {
								const existingIndex = items.findIndex((candidate) => candidate.id === item.id);
								if (existingIndex === -1) {
									counts = adjustCountsForItem(counts, item, 1);
								} else {
									const existing = items[existingIndex];
									if (existing.status !== item.status) {
										counts = adjustCountsForItem(counts, existing, -1);
										counts = adjustCountsForItem(counts, item, 1);
									}
								}
								const nextItems = existingIndex === -1
									? [...items, item]
									: items.map((candidate, index) => (index === existingIndex ? item : candidate));
								items = clampFeedItems(nextItems, currentQuery);
							} else if (!currentQuery.ui_thread_id) {
								counts = adjustCountsForItem(counts, item, 1);
							}
							return {
								...state,
								items,
								counts
							};
						}
						case 'FeedItemUpdated': {
							if (!scopeMatches(event.data.principal, event.data.workspace)) {
								return state;
							}
							const existingIndex = state.items.findIndex((candidate) => candidate.id === event.data.id);
							if (existingIndex === -1) {
								if (
									currentQuery.ui_thread_id
									&& event.data.ui_thread_id
									&& event.data.ui_thread_id !== currentQuery.ui_thread_id
								) {
									return state;
								}
								requiresRefresh = true;
								return state;
							}
							const current = state.items[existingIndex];
							const nextItem = applyPatch(current, event.data.patch as Record<string, unknown>);
							const stillMatchesQuery = matchesQuery(nextItem, currentQuery);
							let counts = state.counts;
							if (!stillMatchesQuery) {
								return {
									...state,
									items: state.items.filter((candidate) => candidate.id !== event.data.id),
									counts: adjustCountsForItem(state.counts, current, -1)
								};
							}
							if (current.status !== nextItem.status) {
								counts = adjustCountsForItem(counts, current, -1);
								counts = adjustCountsForItem(counts, nextItem, 1);
							}
							return {
								...state,
								items: clampFeedItems(
									state.items.map((candidate, index) => (index === existingIndex ? nextItem : candidate)),
									currentQuery
								),
								counts
							};
						}
						case 'FeedItemRemoved': {
							if (!scopeMatches(event.data.principal, event.data.workspace)) {
								return state;
							}
							const existingIndex = state.items.findIndex((candidate) => candidate.id === event.data.id);
							if (existingIndex === -1) {
								if (
									currentQuery.ui_thread_id
									&& event.data.ui_thread_id
									&& event.data.ui_thread_id !== currentQuery.ui_thread_id
								) {
									return state;
								}
								requiresRefresh = true;
								return state;
							}
							const current = state.items[existingIndex];
							return {
								...state,
								items: state.items.filter((candidate) => candidate.id !== event.data.id),
								counts: adjustCountsForItem(state.counts, current, -1)
							};
						}
						default:
							return state;
					}
				});
			}

			if (nextSequence <= lastObservedEventSequence) return;
			lastObservedEventSequence = nextSequence;
			if (requiresRefresh) {
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
			update((state) => ({
				...state,
				isLoading: false,
				error: null,
				lastLoadedAt: null,
				items: [],
				counts: emptyCounts()
			}));
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

	function setQuery(nextQuery: FeedStoreQuery): void {
		currentQuery = normalizeQuery(nextQuery);
		update((state) => ({
			...state,
			query: currentQuery
		}));
		if (activeConsumers > 0) {
			void runRefresh();
		}
	}

	async function deleteItem(itemId: string): Promise<boolean> {
		if (!browser) return false;
		const trimmed = itemId.trim();
		if (!trimmed) return false;
		const queryString = buildQueryString(currentQuery);
		const suffix = queryString ? `?${queryString}` : '';
		const response = await timedFetch(
			`/api/magician/v2/feed/items/${encodeURIComponent(trimmed)}${suffix}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to delete feed item (${response.status})`);
		}
		// Optimistic local removal — the realtime FeedItemRemoved event
		// may also arrive but the store's reducer treats removals as
		// idempotent.
		update((state) => {
			const removed = state.items.find((item) => item.id === trimmed);
			if (!removed) return state;
			return {
				...state,
				items: state.items.filter((item) => item.id !== trimmed),
				counts: adjustCountsForItem(state.counts, removed, -1)
			};
		});
		return true;
	}

	async function purgeOrphans(): Promise<number> {
		if (!browser) return 0;
		const response = await timedFetch('/api/magician/v2/feed/purge-orphans', {
			method: 'POST'
		});
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to purge orphans (${response.status})`);
		}
		const payload = (await response.json()) as { removed_count?: number };
		// Trust the realtime FeedItemRemoved bridge for live items, but
		// re-fetch counts so the buttons reflect the post-purge totals
		// even when the orphans were never in `state.items`.
		void runRefresh();
		return payload.removed_count ?? 0;
	}

	async function clearItems(): Promise<number> {
		if (!browser) return 0;
		const queryString = buildQueryString(currentQuery);
		const suffix = queryString ? `?${queryString}` : '';
		const response = await timedFetch(
			`/api/magician/v2/feed/items${suffix}`,
			{ method: 'DELETE' }
		);
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to clear feed (${response.status})`);
		}
		const payload = (await response.json()) as { removed_count?: number };
		// Optimistic local clear — defensive in case the realtime path lags.
		update((state) => ({
			...state,
			items: [],
			counts: emptyCounts()
		}));
		return payload.removed_count ?? 0;
	}

	async function postLearningCandidateAction(
		candidateId: string,
		action: 'confirm' | 'edit-confirm' | 'archive',
		extraBody: Record<string, unknown> = {}
	): Promise<void> {
		if (!browser) return;
		const trimmed = candidateId.trim();
		if (!trimmed) return;
		const response = await timedFetch(
			`/api/magician/v2/feed/learnings/${encodeURIComponent(trimmed)}/${action}`,
			{
				method: 'POST',
				headers: {
					'Content-Type': 'application/json'
				},
				body: JSON.stringify({
					...extraBody
				})
			}
		);
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to update learning (${response.status})`);
		}
		update((state) => {
			const feedId = `learning_candidate:${trimmed}`;
			const removed = state.items.find((item) => item.id === feedId);
			if (!removed) return state;
			return {
				...state,
				items: state.items.filter((item) => item.id !== feedId),
				counts: adjustCountsForItem(state.counts, removed, -1)
			};
		});
		void runRefresh();
	}

	async function confirmLearningCandidate(candidateId: string): Promise<void> {
		await postLearningCandidateAction(candidateId, 'confirm');
	}

	async function editConfirmLearningCandidate(candidateId: string, revisedValue: string): Promise<void> {
		await postLearningCandidateAction(candidateId, 'edit-confirm', {
			revised_value: revisedValue
		});
	}

	async function archiveLearningCandidate(candidateId: string, reason?: string): Promise<void> {
		await postLearningCandidateAction(candidateId, 'archive', reason ? { reason } : {});
	}

	async function postLearningInsightAction(
		insightId: string,
		action: 'archive' | 'save-to-memory' | 'create-follow-up',
		extraBody: Record<string, unknown> = {}
	): Promise<unknown> {
		if (!browser) return null;
		const trimmed = insightId.trim();
		if (!trimmed) return null;
		const response = await timedFetch(
			`/api/magician/v2/feed/insights/${encodeURIComponent(trimmed)}/${action}`,
			{
				method: 'POST',
				headers: {
					'Content-Type': 'application/json'
				},
				body: JSON.stringify({
					...extraBody
				})
			}
		);
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to update insight (${response.status})`);
		}
		const payload = await response.json().catch(() => null);
		update((state) => {
			const removed = state.items.find((item) => item.id === trimmed);
			if (!removed) return state;
			return {
				...state,
				items: state.items.filter((item) => item.id !== trimmed),
				counts: adjustCountsForItem(state.counts, removed, -1)
			};
		});
		void runRefresh();
		return payload;
	}

	async function archiveLearningInsight(insightId: string, reason?: string): Promise<void> {
		await postLearningInsightAction(insightId, 'archive', reason ? { reason } : {});
	}

	async function saveLearningInsightToMemory(
		insightId: string,
		memoryValue?: string
	): Promise<unknown> {
		return postLearningInsightAction(
			insightId,
			'save-to-memory',
			memoryValue ? { memory_value: memoryValue } : {}
		);
	}

	async function createLearningInsightFollowUp(
		insightId: string,
		taskTitle?: string,
		taskDescription?: string
	): Promise<unknown> {
		return postLearningInsightAction(insightId, 'create-follow-up', {
			...(taskTitle ? { task_title: taskTitle } : {}),
			...(taskDescription ? { task_description: taskDescription } : {})
		});
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
			stopPolling();
			stopRealtimeBridge();
		},
		refresh(): Promise<void> {
			return runRefresh();
		},
		deleteItem,
		confirmLearningCandidate,
		editConfirmLearningCandidate,
		archiveLearningCandidate,
		archiveLearningInsight,
		saveLearningInsightToMemory,
		createLearningInsightFollowUp,
		clearItems,
		purgeOrphans,
		setQuery,
		clearError(): void {
			update((state) => ({ ...state, error: null }));
		}
	};
}
