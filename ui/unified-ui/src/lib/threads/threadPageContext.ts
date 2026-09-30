import { getContext, setContext } from 'svelte';
import type { Readable } from 'svelte/store';
import type { UiThreadDetail } from './types';

/**
 * Shell state shared by the thread `+layout.svelte` with its child routes
 * (`chat` / `tasks` / `settings`). The layout owns the thread lifecycle
 * (store start/stop, `loadThread`, scope/param reactivity) and publishes the
 * resolved thread identity + load state here so each child route doesn't have
 * to re-run that work. Task-specific derivations (the thread's task list,
 * filter state, MUIJ surface) stay local to the Tasks route.
 */
export interface ThreadPageState {
	/** Normalized active thread id (route `[name]`). */
	threadName: string;
	/** Display title — thread record `name`, falling back to the id. */
	threadDisplayName: string;
	/** Full thread record for the active thread, or null until loaded. */
	threadDetail: UiThreadDetail | null;
	/** True while the layout's `loadThread` is in flight. */
	loadingThread: boolean;
	/** Load error for the active thread, or null. */
	threadError: string | null;
}

const THREAD_PAGE_KEY = Symbol('thread-page');

export function setThreadPageContext(store: Readable<ThreadPageState>): void {
	setContext(THREAD_PAGE_KEY, store);
}

export function getThreadPageContext(): Readable<ThreadPageState> {
	const store = getContext<Readable<ThreadPageState> | undefined>(THREAD_PAGE_KEY);
	if (!store) {
		throw new Error('getThreadPageContext() must be called within the thread layout.');
	}
	return store;
}
