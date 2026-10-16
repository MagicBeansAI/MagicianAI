// Global application state store
import { writable } from 'svelte/store';

export interface AppState {
	quotaExceeded: boolean;
	quotaMessage: string | null;
	quotaUsed: number;
	quotaLimit: number;
	retryAfterSec: number;
	queued: boolean;
	userUsed: number;
	userLimit: number;
	queuedRequestId: string | null;
}

const initialState: AppState = {
	quotaExceeded: false,
	quotaMessage: null,
	quotaUsed: 0,
	quotaLimit: 0,
	retryAfterSec: 0,
	queued: false,
	userUsed: 0,
	userLimit: 0,
	queuedRequestId: null
};

export const appState = writable<AppState>(initialState);

/**
 * Update quota exceeded state
 */
export function setQuotaExceeded(
	exceeded: boolean,
	message: string | null = null,
	retryAfter: number = 0
) {
	appState.update((state) => ({
		...state,
		quotaExceeded: exceeded,
		quotaMessage: message,
		retryAfterSec: retryAfter
	}));
}

/**
 * Update quota usage
 */
export function setQuotaUsage(used: number, limit: number) {
	appState.update((state) => ({
		...state,
		quotaUsed: used,
		quotaLimit: limit
	}));
}

/**
 * Update user quota usage
 */
export function setUserQuotaUsage(used: number, limit: number) {
	appState.update((state) => ({
		...state,
		userUsed: used,
		userLimit: limit
	}));
}

/**
 * Set queued status
 */
export function setQueuedStatus(queued: boolean, requestId: string | null = null) {
	appState.update((state) => ({
		...state,
		queued,
		queuedRequestId: requestId
	}));
}

/**
 * Clear quota exceeded state
 */
export function clearQuotaExceeded() {
	appState.update((state) => ({
		...state,
		quotaExceeded: false,
		quotaMessage: null,
		retryAfterSec: 0
	}));
}

/**
 * Reset app state to initial values
 */
export function resetAppState() {
	appState.set(initialState);
}
