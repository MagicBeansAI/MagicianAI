import { get } from 'svelte/store';
import { vibeDevProjectStore } from '$lib/stores/vibeDevProjectStore';

/**
 * Normalize a thread identifier from a route param / task `uiThreadId`.
 *
 * Thread ids are lowercase slugs; an empty/blank value falls back to the
 * always-present `'general'` thread. Shared by the thread `+layout` (route
 * param → active thread) and the thread Tasks route (filtering each task's
 * `uiThreadId` to the active thread).
 */
export function normalizeThreadId(value: string | null | undefined): string {
	const normalized = (value || '').trim().toLowerCase();
	return normalized.length > 0 ? normalized : 'general';
}

/**
 * Check if a thread ID represents the VibeDev thread.
 */
export function isVibeDevThread(threadId: string | null | undefined): boolean {
	const normalized = (threadId || '').trim().toLowerCase();
	return normalized === 'vibedev';
}

/**
 * Check if a session belongs to VibeDev.
 * A session belongs to VibeDev if its `ui_thread_id` is 'vibedev' or if it matches
 * any known VibeDev project's `chat_session_id`.
 */
export function isVibeDevSession(
	session: { id?: string; ui_thread_id?: string | null; title?: string | null } | null | undefined
): boolean {
	if (!session) return false;
	if (isVibeDevThread(session.ui_thread_id)) return true;
	if (session.id) {
		try {
			const projects = get(vibeDevProjectStore).projects;
			if (projects.some((p) => p.chat_session_id === session.id)) return true;
		} catch {
			// Ignore in environments where vibeDevProjectStore is unavailable
		}
	}
	return false;
}
