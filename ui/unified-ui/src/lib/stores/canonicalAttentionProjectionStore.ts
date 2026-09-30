import { writable } from 'svelte/store';

import {
	fetchCanonicalAttentionProjection,
	type CanonicalAttentionProjection
} from '$lib/attention/canonicalAttentionProjection';
import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';

export interface CanonicalAttentionProjectionState {
	projection: CanonicalAttentionProjection | null;
	isLoading: boolean;
	fallbackReason: string | null;
	loadedScopeKey: string | null;
}

const initialState: CanonicalAttentionProjectionState = {
	projection: null,
	isLoading: false,
	fallbackReason: null,
	loadedScopeKey: null
};

function createCanonicalAttentionProjectionStore() {
	const { subscribe, set, update } = writable(initialState);
	let generation = 0;
	let activeRequest: {
		scopeKey: string;
		controller: AbortController;
		promise: Promise<void>;
	} | null = null;

	async function runRefresh(
		scopeKey: string,
		requestGeneration: number,
		controller: AbortController
	): Promise<void> {
		const requestedScope = getCurrentScopeIdentity();
		const requestedScopeKey = `${requestedScope.principal}:${requestedScope.workspace}`;
		if (requestedScopeKey !== scopeKey) {
			if (requestGeneration === generation) {
				set({
					projection: null,
					isLoading: false,
					fallbackReason: 'scope_mismatch',
					loadedScopeKey: scopeKey
				});
			}
			return;
		}
		let retainedProjection: CanonicalAttentionProjection | null = null;
		update((state) => {
			retainedProjection = state.loadedScopeKey === scopeKey ? state.projection : null;
			return {
				...state,
				projection: retainedProjection,
				isLoading: true,
				fallbackReason: null,
				loadedScopeKey: scopeKey
			};
		});
		const result = await fetchCanonicalAttentionProjection(requestedScope, {
			signal: controller.signal
		});
		if (requestGeneration !== generation || controller.signal.aborted) return;
		const currentScope = getCurrentScopeIdentity();
		const currentScopeKey = `${currentScope.principal}:${currentScope.workspace}`;
		if (currentScopeKey !== scopeKey) {
			set({
				projection: null,
				isLoading: false,
				fallbackReason: 'scope_changed_during_projection_load',
				loadedScopeKey: currentScopeKey
			});
			return;
		}
		set({
			projection: result.projection ?? retainedProjection,
			isLoading: false,
			fallbackReason: result.fallback_reason,
			loadedScopeKey: scopeKey
		});
	}

	return {
		subscribe,

		/** One complete-universe read owns both destination lanes. A failed or
		 * malformed same-scope read retains the last verified union atomically;
		 * switching to unverified legacy halves could reintroduce a hidden alias. */
		refresh(scopeKey: string): Promise<void> {
			if (activeRequest?.scopeKey === scopeKey) return activeRequest.promise;
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
			update((state) => ({ ...state, isLoading: false }));
		}
	};
}

export const canonicalAttentionProjectionStore = createCanonicalAttentionProjectionStore();
