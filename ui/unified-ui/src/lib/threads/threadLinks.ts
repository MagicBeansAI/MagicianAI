/**
 * Store-aware thread link and label helpers: the pure rules live in
 * `threadKeys.ts`; these read the loaded threads from `threadStore`.
 *
 * - `threadHref(id, rest)` — link a thread by its slug (id fallback). Read
 *   once, so use it where the link is built on click.
 * - `$threadHrefs(id, rest)` / `$threadLabels(id)` — reactive versions for
 *   markup, updating as threads load or are renamed.
 * - `threadLabel(id)` — the name behind a `#name` label (id fallback).
 */
import { derived, get, type Readable } from 'svelte/store';

import { threadStore } from '$lib/stores/threadStore';
import { findThreadByKey, threadHrefFor, threadLabelFrom } from './threadKeys';
import type { UiThreadRecord } from './types';

function loadedThreads(state: { threads?: UiThreadRecord[] } | null | undefined): UiThreadRecord[] {
	return state?.threads ?? [];
}

export function threadHref(thread: string | UiThreadRecord, rest = ''): string {
	return threadHrefFor(loadedThreads(get(threadStore)), thread, rest);
}

/**
 * The canonical id for a thread key (slug, former slug or id) among loaded
 * threads; the key itself when no loaded thread answers to it. Synchronous,
 * so it suits redirects; `threadStore.resolveThreadKey` asks the server.
 */
export function threadIdForKey(key: string): string {
	return findThreadByKey(loadedThreads(get(threadStore)), key)?.id ?? key;
}

export function threadLabel(threadId: string | null | undefined): string {
	return threadLabelFrom(loadedThreads(get(threadStore)), threadId);
}

export const threadLabels: Readable<(threadId: string | null | undefined) => string> = derived(
	threadStore,
	($threadStore) => {
		const threads = loadedThreads($threadStore);
		return (threadId: string | null | undefined) => threadLabelFrom(threads, threadId);
	}
);

export const threadHrefs: Readable<(thread: string | UiThreadRecord, rest?: string) => string> =
	derived(threadStore, ($threadStore) => {
		const threads = loadedThreads($threadStore);
		return (thread: string | UiThreadRecord, rest = '') => threadHrefFor(threads, thread, rest);
	});
