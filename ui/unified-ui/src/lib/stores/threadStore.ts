import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

import type { UiThreadDetail, UiThreadRecord } from '$lib/threads/types';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

interface ThreadStoreState {
	isLoading: boolean;
	error: string | null;
	threads: UiThreadRecord[];
	activeThread: UiThreadDetail | null;
}

const defaultState: ThreadStoreState = {
	isLoading: false,
	error: null,
	threads: [],
	activeThread: null
};

function normalizeError(error: unknown): string {
	if (error instanceof Error && error.message.trim().length > 0) {
		return error.message;
	}
	return 'Failed to load threads';
}

function getCurrentScope(): { principal: string; workspace: string } {
	const current = get(scopeIdentityStore);
	return { principal: current.principal, workspace: current.workspace };
}

function currentThreadScopeKey(): string {
	const scope = getCurrentScope();
	return `${scope.principal}:${scope.workspace}`;
}

async function requestJson<T>(input: string, init?: RequestInit): Promise<T> {
	const response = await timedFetch(input, init);
	if (!response.ok) {
		const text = await response.text().catch(() => '');
		throw new Error(text || `Request failed (${response.status})`);
	}
	return response.json() as Promise<T>;
}

function createThreadStore() {
	const { subscribe, update } = writable<ThreadStoreState>(defaultState);
	let activeConsumers = 0;
	let scopeUnsubscribe: (() => void) | null = null;
	let lastScopeKey = '';
	let loadGeneration = 0;

	function nextThreadLoadToken(): { generation: number; scopeKey: string } {
		return {
			generation: loadGeneration,
			scopeKey: currentThreadScopeKey()
		};
	}

	function isStaleThreadLoad(generation: number, scopeKey: string): boolean {
		return generation !== loadGeneration || currentThreadScopeKey() !== scopeKey;
	}

	function clearThreadsForScopeChange(): void {
		update((state) => ({
			...state,
			isLoading: false,
			error: null,
			threads: [],
			activeThread: null
		}));
	}

	async function refresh(): Promise<void> {
		if (!browser) return;
		const token = nextThreadLoadToken();
		update((state) => ({ ...state, isLoading: true, error: null }));
		try {
			const payload = await requestJson<{ threads: UiThreadRecord[] }>(
				'/api/magician/v2/ui-threads'
			);
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return;
			}
			const first = payload.threads[0];
			if (first) {
				scopeIdentityStore.observe(first.principal, first.workspace);
			}
			update((state) => ({
				...state,
				isLoading: false,
				error: null,
				threads: payload.threads
			}));
		} catch (error) {
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return;
			}
			update((state) => ({
				...state,
				isLoading: false,
				error: normalizeError(error)
			}));
		}
	}

	async function loadThread(id: string, ensure = false): Promise<UiThreadDetail | null> {
		if (!browser) return null;
		const token = nextThreadLoadToken();
		try {
			const payload = await requestJson<{ thread: UiThreadDetail }>(
				`/api/magician/v2/ui-threads/${encodeURIComponent(id)}`
			);
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return null;
			}
			scopeIdentityStore.observe(payload.thread.principal, payload.thread.workspace);
			update((state) => ({
				...state,
				activeThread: payload.thread,
				threads: mergeThreadRecord(state.threads, payload.thread)
			}));
			return payload.thread;
		} catch (error) {
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return null;
			}
			if (!ensure) {
				update((state) => ({ ...state, error: normalizeError(error) }));
				return null;
			}
			const created = await createThread(id, id);
			if (!created) return null;
			return loadThread(created.id, false);
		}
	}

	async function createThread(name: string, explicitId?: string): Promise<UiThreadRecord | null> {
		if (!browser) return null;
		const token = nextThreadLoadToken();
		try {
			const thread = await requestJson<UiThreadRecord>(
				'/api/magician/v2/ui-threads',
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({
						id: explicitId ?? null,
						name
					})
				}
			);
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return null;
			}
			scopeIdentityStore.observe(thread.principal, thread.workspace);
			update((state) => ({
				...state,
				error: null,
				threads: mergeThreadRecord(state.threads, thread)
			}));
			await refresh();
			return thread;
		} catch (error) {
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return null;
			}
			update((state) => ({ ...state, error: normalizeError(error) }));
			return null;
		}
	}

	async function updateThread(
		id: string,
		patch: {
			name?: string;
			archived?: boolean;
			memory_text?: string | null;
			/** "chat" | "dev" — see Developer Mode plan (2026-05-13). */
			display_mode?: 'chat' | 'dev';
			/** Plan-mode gate for Developer Mode (Phase 5). */
			plan_mode?: boolean;
		}
	): Promise<UiThreadDetail | null> {
		if (!browser) return null;
		const token = nextThreadLoadToken();
		try {
			const payload = await requestJson<{ thread: UiThreadDetail }>(
				`/api/magician/v2/ui-threads/${encodeURIComponent(id)}`,
				{
					method: 'PATCH',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify(patch)
				}
			);
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return null;
			}
			scopeIdentityStore.observe(payload.thread.principal, payload.thread.workspace);
			update((state) => ({
				...state,
				error: null,
				activeThread: state.activeThread?.id === payload.thread.id ? payload.thread : state.activeThread,
				threads: mergeThreadRecord(state.threads, payload.thread)
			}));
			await refresh();
			return payload.thread;
		} catch (error) {
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return null;
			}
			update((state) => ({ ...state, error: normalizeError(error) }));
			return null;
		}
	}

	async function deleteThread(id: string): Promise<boolean> {
		if (!browser || id === 'general') return false;
		const token = nextThreadLoadToken();
		try {
			const response = await timedFetch(
				`/api/magician/v2/ui-threads/${encodeURIComponent(id)}`,
				{ method: 'DELETE' }
			);
			if (!response.ok) {
				const text = await response.text().catch(() => '');
				throw new Error(text || `Request failed (${response.status})`);
			}
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return false;
			}
			update((state) => ({
				...state,
				error: null,
				activeThread: state.activeThread?.id === id ? null : state.activeThread,
				threads: state.threads.filter((thread) => thread.id !== id)
			}));
			await refresh();
			return true;
		} catch (error) {
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return false;
			}
			update((state) => ({ ...state, error: normalizeError(error) }));
			return false;
		}
	}

	async function reorderThreads(orderedIds: string[]): Promise<void> {
		if (!browser) return;
		const token = nextThreadLoadToken();
		try {
			const payload = await requestJson<{ threads: UiThreadRecord[] }>(
				'/api/magician/v2/ui-threads/reorder',
				{
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ ordered_ids: orderedIds })
				}
			);
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return;
			}
			update((state) => ({
				...state,
				error: null,
				threads: payload.threads
			}));
		} catch (error) {
			if (isStaleThreadLoad(token.generation, token.scopeKey)) {
				return;
			}
			update((state) => ({ ...state, error: normalizeError(error) }));
		}
	}

	return {
		subscribe,
		start(): void {
			activeConsumers += 1;
			if (activeConsumers === 1) {
				lastScopeKey = currentThreadScopeKey();
				scopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
					const scopeKey = `${scope.principal}:${scope.workspace}`;
					if (scopeKey === lastScopeKey) return;
					lastScopeKey = scopeKey;
					loadGeneration += 1;
					clearThreadsForScopeChange();
					void refresh();
				});
				void refresh();
			}
		},
		stop(): void {
			activeConsumers = Math.max(0, activeConsumers - 1);
			if (activeConsumers === 0 && scopeUnsubscribe) {
				scopeUnsubscribe();
				scopeUnsubscribe = null;
				lastScopeKey = '';
			}
		},
		refresh,
		loadThread,
		ensureThread(id: string): Promise<UiThreadDetail | null> {
			return loadThread(id, true);
		},
		createThread,
		updateThread,
		deleteThread,
		reorderThreads,
		clearActiveThread(): void {
			update((state) => ({ ...state, activeThread: null }));
		}
	};
}

function mergeThreadRecord(
	threads: UiThreadRecord[],
	record: UiThreadRecord | UiThreadDetail
): UiThreadRecord[] {
	const next = threads.filter((thread) => thread.id !== record.id);
	next.push({
		principal: record.principal,
		workspace: record.workspace,
		id: record.id,
		name: record.name,
		archived: record.archived,
		sort_order: record.sort_order,
		memory_summary: record.memory_summary,
		memory_updated_at: record.memory_updated_at,
		created_at: record.created_at,
		updated_at: record.updated_at,
		history_lane: record.history_lane
	});
	return next.sort((left, right) => {
		if (left.id === 'general') return -1;
		if (right.id === 'general') return 1;
		if (left.archived !== right.archived) return left.archived ? 1 : -1;
		return left.sort_order - right.sort_order || right.updated_at - left.updated_at || left.id.localeCompare(right.id);
	});
}

export const threadStore = createThreadStore();
