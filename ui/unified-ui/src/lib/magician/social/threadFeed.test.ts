import { describe, expect, it } from 'vitest';
import { mergeFeedPosts, orderPostsByThread } from './threadFeed';

interface TestPost {
	post_id: string;
	parent_id: string | null;
	created_at: string;
}

const post = (post_id: string, created_at: string, parent_id: string | null = null): TestPost => ({
	post_id,
	parent_id,
	created_at
});

describe('Town Square thread ordering', () => {
	it('keeps loaded history when a head refresh returns only fifty posts', () => {
		const history = Array.from({ length: 102 }, (_, index) => ({ post_id: String(index), body: 'original' }));
		const refreshed = mergeFeedPosts(history, history.slice(0, 50));
		expect(refreshed).toHaveLength(102);
		expect(refreshed.map((post) => post.post_id)).toEqual(history.map((post) => post.post_id));
		expect(mergeFeedPosts(refreshed, history.slice(0, 50))).toEqual(refreshed);
	});

	it('adds new posts and updates fetched rows without duplicating an older page', () => {
		const history = [{ post_id: 'recent', body: 'old' }, { post_id: 'older', body: 'kept' }];
		const refreshed = mergeFeedPosts(history, [
			{ post_id: 'new', body: 'new post' },
			{ post_id: 'recent', body: 'updated' }
		]);
		expect(refreshed).toEqual([
			{ post_id: 'new', body: 'new post' },
			{ post_id: 'recent', body: 'updated' },
			{ post_id: 'older', body: 'kept' }
		]);
		expect(history[0].body).toBe('old');
		expect(mergeFeedPosts(refreshed, [{ post_id: 'older', body: 'fresh older row' }])).toHaveLength(3);
	});
	it('groups replies under their root and orders active threads first', () => {
		const ordered = orderPostsByThread([
			post('reply-a2', '2026-08-26T00:00:05Z', 'root-a'),
			post('root-b', '2026-08-26T00:00:04Z'),
			post('reply-a1', '2026-08-26T00:00:03Z', 'root-a'),
			post('root-a', '2026-08-26T00:00:01Z')
		]);

		expect(ordered.map(({ post_id }) => post_id)).toEqual([
			'root-a',
			'reply-a1',
			'reply-a2',
			'root-b'
		]);
	});

	it('keeps a recent paginated reply in activity order until its root arrives', () => {
		const ordered = orderPostsByThread([
			post('orphan-reply', '2026-08-26T00:00:05Z', 'older-root'),
			post('visible-root', '2026-08-26T00:00:04Z')
		]);

		expect(ordered.map(({ post_id }) => post_id)).toEqual(['orphan-reply', 'visible-root']);
	});

	it('keeps nested operator and mention replies inside their discussion tree', () => {
		const ordered = orderPostsByThread([
			post('other-root', '2026-08-26T00:00:04Z'),
			post('nested-reply', '2026-08-26T00:00:06Z', 'direct-reply'),
			post('direct-reply', '2026-08-26T00:00:05Z', 'root'),
			post('root', '2026-08-26T00:00:01Z')
		]);

		expect(ordered.map(({ post_id }) => post_id)).toEqual([
			'root',
			'direct-reply',
			'nested-reply',
			'other-root'
		]);
	});
});
