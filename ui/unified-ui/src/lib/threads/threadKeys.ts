/**
 * Thread keys and URLs.
 *
 * A thread has two names in a URL's first segment (`/t/<key>/…`):
 * - `id`: permanent. Sessions, tasks and memory reference it; it never changes.
 * - `slug`: follows the thread's name. A rename moves it and the server keeps
 *   the old slug as an alias, so an old link still opens the thread.
 *
 * The server resolves a key as id, then current slug, then alias
 * (`GET /api/magician/v2/ui-threads/{key}`). Links the UI builds use the slug
 * when it is known and fall back to the id, which always resolves.
 *
 * `new` is reserved: `/t/new` is the "name a new thread" page, never a thread.
 */
import type { UiThreadDetail, UiThreadRecord } from './types';

export const NEW_THREAD_KEY = 'new';
export const RESERVED_THREAD_KEYS: readonly string[] = [NEW_THREAD_KEY];

type ThreadLike = Pick<UiThreadRecord, 'id'> & { slug?: string | null; name?: string | null };

const THREAD_ROUTE = /^\/t\/([^/]+)(\/.*)?$/;

export function isReservedThreadKey(key: string | null | undefined): boolean {
	return RESERVED_THREAD_KEYS.includes((key ?? '').trim().toLowerCase());
}

/** The URL key for a thread: its slug, else its id. */
export function threadSlugOf(thread: ThreadLike): string {
	const slug = (thread.slug ?? '').trim();
	return slug.length > 0 ? slug : thread.id;
}

/** A known thread a route key names: by id first, then by current slug. */
export function findThreadByKey<T extends ThreadLike>(
	threads: readonly T[],
	key: string | null | undefined
): T | undefined {
	const normalized = (key ?? '').trim().toLowerCase();
	if (!normalized) return undefined;
	return (
		threads.find((thread) => thread.id === normalized) ??
		threads.find((thread) => threadSlugOf(thread) === normalized)
	);
}

/**
 * `/t/<slug><rest>` for a thread id or record. An id whose record is not in
 * `threads` links by id. `rest` is appended as given (`/chat`, `?session=…`).
 */
export function threadHrefFor(
	threads: readonly ThreadLike[],
	thread: string | ThreadLike,
	rest = ''
): string {
	const record =
		typeof thread === 'string' ? threads.find((candidate) => candidate.id === thread) : thread;
	const key = record ? threadSlugOf(record) : (thread as string);
	return `/t/${encodeURIComponent(key)}${rest}`;
}

/** A thread's display name for `#name` labels; the id when it is unknown. */
export function threadLabelFrom(
	threads: readonly ThreadLike[],
	threadId: string | null | undefined
): string {
	const id = (threadId ?? '').trim();
	if (!id) return '';
	const name = threads.find((thread) => thread.id === id)?.name?.trim();
	return name && name.length > 0 ? name : id;
}

/** The key in a `/t/<key>/…` pathname, or `null` off the thread route. */
export function threadRouteKey(pathname: string): string | null {
	const match = pathname.match(THREAD_ROUTE);
	if (!match) return null;
	try {
		return decodeURIComponent(match[1]);
	} catch {
		return match[1];
	}
}

/**
 * The canonical URL for the thread route `url` shows: the same sub-path,
 * query and hash with the first segment replaced by the thread's slug.
 * `null` when the URL already uses the slug or is not a thread route.
 */
export function canonicalThreadUrl(
	url: Pick<URL, 'pathname' | 'search' | 'hash'>,
	thread: Pick<UiThreadDetail, 'id'> & { slug?: string | null }
): string | null {
	const match = url.pathname.match(THREAD_ROUTE);
	const current = threadRouteKey(url.pathname);
	if (!match || current === null) return null;
	const slug = threadSlugOf(thread);
	if (current === slug) return null;
	return `/t/${encodeURIComponent(slug)}${match[2] ?? ''}${url.search}${url.hash}`;
}
