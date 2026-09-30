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
