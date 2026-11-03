import { describe, expect, it } from 'vitest';

import {
	canonicalThreadUrl,
	findThreadByKey,
	isReservedThreadKey,
	threadHrefFor,
	threadLabelFrom,
	threadRouteKey,
	threadSlugOf
} from './threadKeys';

const threads = [
	{ id: 'general', slug: 'general', name: 'general' },
	{ id: 'research', slug: 'trip-planning', name: 'Trip planning' },
	{ id: 'legacy', name: 'Legacy (no slug field)' }
];

describe('thread keys', () => {
	it('reserves the new-thread route word', () => {
		expect(isReservedThreadKey('new')).toBe(true);
		expect(isReservedThreadKey(' NEW ')).toBe(true);
		expect(isReservedThreadKey('news')).toBe(false);
		expect(isReservedThreadKey(null)).toBe(false);
	});

	it('uses the slug as the URL key and falls back to the id', () => {
		expect(threadSlugOf(threads[1])).toBe('trip-planning');
		expect(threadSlugOf(threads[2])).toBe('legacy');
		expect(threadSlugOf({ id: 'x', slug: '  ' })).toBe('x');
	});

	it('finds a known thread by id first, then by slug', () => {
		expect(findThreadByKey(threads, 'research')?.id).toBe('research');
		expect(findThreadByKey(threads, 'Trip-Planning')?.id).toBe('research');
		expect(findThreadByKey(threads, 'unknown')).toBeUndefined();
		expect(findThreadByKey(threads, '')).toBeUndefined();
	});

	it('links by slug when the thread is known and by id otherwise', () => {
		expect(threadHrefFor(threads, 'research', '/chat?session=s1')).toBe('/t/trip-planning/chat?session=s1');
		expect(threadHrefFor(threads, threads[1])).toBe('/t/trip-planning');
		expect(threadHrefFor(threads, 'not-loaded', '/tasks')).toBe('/t/not-loaded/tasks');
	});

	it('labels a thread by its name, falling back to the id', () => {
		expect(threadLabelFrom(threads, 'research')).toBe('Trip planning');
		expect(threadLabelFrom(threads, 'not-loaded')).toBe('not-loaded');
		expect(threadLabelFrom(threads, null)).toBe('');
	});

	it('reads the key from a thread route', () => {
		expect(threadRouteKey('/t/trip-planning/chat')).toBe('trip-planning');
		expect(threadRouteKey('/t/research')).toBe('research');
		expect(threadRouteKey('/chat')).toBeNull();
	});

	it('rewrites a thread URL to the canonical slug, keeping sub-path, query and hash', () => {
		const record = { id: 'research', slug: 'trip-planning' };
		expect(
			canonicalThreadUrl({ pathname: '/t/research/chat', search: '?session=s1', hash: '#m2' }, record)
		).toBe('/t/trip-planning/chat?session=s1#m2');
		expect(
			canonicalThreadUrl({ pathname: '/t/old-name/tasks', search: '?selected=t1', hash: '' }, record)
		).toBe('/t/trip-planning/tasks?selected=t1');
		expect(canonicalThreadUrl({ pathname: '/t/trip-planning/settings', search: '', hash: '' }, record)).toBeNull();
		expect(canonicalThreadUrl({ pathname: '/chat', search: '', hash: '' }, record)).toBeNull();
	});
});
