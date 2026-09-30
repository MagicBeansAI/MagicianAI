import { writable } from 'svelte/store';

import {
	parseAttentionSemanticExtractionHealth,
	type AttentionSemanticExtractionHealth
} from '$lib/attention/attentionSemanticExtraction';
import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';

const SEMANTIC_HEALTH_TTL_MS = 60_000;

export interface AttentionSemanticExtractionState {
	health: AttentionSemanticExtractionHealth | null;
	loadedScopeKey: string | null;
	loadedAt: number | null;
	isLoading: boolean;
	error: string | null;
}

const initialState: AttentionSemanticExtractionState = {
	health: null,
	loadedScopeKey: null,
	loadedAt: null,
	isLoading: false,
	error: null
};

function createAttentionSemanticExtractionStore() {
	const { subscribe, set, update } = writable(initialState);
	let state = initialState;
	let generation = 0;
	let activeRequest: {
		scopeKey: string;
		controller: AbortController;
		promise: Promise<void>;
	} | null = null;
	const unsubscribe = subscribe((next) => (state = next));

	async function runRefresh(
		scopeKey: string,
		requestGeneration: number,
		controller: AbortController
	): Promise<void> {
		const scope = getCurrentScopeIdentity();
		if (`${scope.principal}:${scope.workspace}` !== scopeKey) return;
		const retained = state.loadedScopeKey === scopeKey ? state.health : null;
		update((current) => ({
			...current,
			health: retained,
			loadedScopeKey: scopeKey,
			isLoading: true,
			error: null
		}));
		try {
			const response = await fetch(
				'/api/magician/v2/channel-assist/attention-learning/semantic-extraction/status',
				{
					headers: {
					},
					signal: controller.signal
				}
			);
			if (!response.ok) throw new Error(`HTTP ${response.status}`);
			const body: unknown = await response.json().catch(() => null);
			const health = parseAttentionSemanticExtractionHealth(body);
			if (!health) throw new Error('semantic_extraction_health_malformed');
			if (requestGeneration !== generation || controller.signal.aborted) return;
			const currentScope = getCurrentScopeIdentity();
			if (`${currentScope.principal}:${currentScope.workspace}` !== scopeKey) return;
			set({
				health,
				loadedScopeKey: scopeKey,
				loadedAt: Date.now(),
				isLoading: false,
				error: null
			});
		} catch (error) {
			if (requestGeneration !== generation || controller.signal.aborted) return;
			update((current) => ({
				...current,
				isLoading: false,
				error: error instanceof Error ? error.message : String(error)
			}));
		}
	}

	return {
		subscribe,

		/** Diagnostics are independent from lane delivery: their complete-universe
		 * scan may finish later, but can never delay or overwrite the cards. */
		refresh(scopeKey: string, force = false): Promise<void> {
			if (activeRequest?.scopeKey === scopeKey) return activeRequest.promise;
			if (
				!force &&
				state.loadedScopeKey === scopeKey &&
				state.loadedAt !== null &&
				Date.now() - state.loadedAt < SEMANTIC_HEALTH_TTL_MS
			) return Promise.resolve();
			activeRequest?.controller.abort();
			const controller = new AbortController();
			const requestGeneration = ++generation;
			const promise = runRefresh(scopeKey, requestGeneration, controller).finally(() => {
				if (activeRequest?.controller === controller) activeRequest = null;
			});
			activeRequest = { scopeKey, controller, promise };
			return promise;
		},

		clear(): void {
			activeRequest?.controller.abort();
			activeRequest = null;
			generation += 1;
			set(initialState);
		},

		cancel(scopeKey: string): void {
			if (activeRequest?.scopeKey !== scopeKey) return;
			activeRequest.controller.abort();
			activeRequest = null;
			generation += 1;
			update((current) => ({ ...current, isLoading: false }));
		},

		destroy(): void {
			unsubscribe();
		}
	};
}

export const attentionSemanticExtractionStore = createAttentionSemanticExtractionStore();
