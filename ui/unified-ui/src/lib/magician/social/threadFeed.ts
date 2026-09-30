export interface ThreadableFeedPost {
	post_id: string;
	parent_id: string | null;
	created_at: string;
}

/** Refresh fetched rows without discarding pages the reader already opened. */
export function mergeFeedPosts<T extends { post_id: string }>(
	retained: readonly T[],
	incoming: readonly T[]
): T[] {
	const refreshed = new Map(incoming.map((post) => [post.post_id, post]));
	return [...refreshed.values(), ...retained.filter((post) => !refreshed.has(post.post_id))];
}

/**
 * Present complete reply trees as visible discussions while keeping threads
 * ordered by their latest activity. Replies whose parent has not arrived in
 * the current pagination window become temporary thread roots, so a recent
 * orphan never falls below older discussions or disappears.
 */
export function orderPostsByThread<T extends ThreadableFeedPost>(posts: readonly T[]): T[] {
	const postsById = new Map(posts.map((post) => [post.post_id, post]));
	const repliesByParent = new Map<string, T[]>();
	for (const post of posts) {
		if (!post.parent_id) continue;
		const replies = repliesByParent.get(post.parent_id) ?? [];
		replies.push(post);
		repliesByParent.set(post.parent_id, replies);
	}

	const comparePosts = (left: T, right: T): number => {
		const leftMillis = Date.parse(left.created_at);
		const rightMillis = Date.parse(right.created_at);
		if (Number.isFinite(leftMillis) && Number.isFinite(rightMillis) && leftMillis !== rightMillis) {
			return leftMillis - rightMillis;
		}
		const timestampOrder = left.created_at.localeCompare(right.created_at);
		return timestampOrder || left.post_id.localeCompare(right.post_id);
	};
	for (const replies of repliesByParent.values()) replies.sort(comparePosts);

	const visited = new Set<string>();
	const flattenThread = (root: T): { posts: T[]; latest: T } => {
		const ordered: T[] = [];
		let latest = root;
		const visit = (post: T) => {
			if (visited.has(post.post_id)) return;
			visited.add(post.post_id);
			ordered.push(post);
			if (comparePosts(latest, post) < 0) latest = post;
			for (const reply of repliesByParent.get(post.post_id) ?? []) visit(reply);
		};
		visit(root);
		return { posts: ordered, latest };
	};

	const rootsAndOrphans = posts.filter(
		(post) => !post.parent_id || !postsById.has(post.parent_id)
	);
	const threads = rootsAndOrphans.map(flattenThread);
	// Defensive fallback for malformed cycles: keep every post visible exactly
	// once even when no node in a component qualifies as a root.
	for (const post of posts) {
		if (!visited.has(post.post_id)) threads.push(flattenThread(post));
	}
	threads.sort((left, right) => comparePosts(right.latest, left.latest));

	return threads.flatMap((thread) => thread.posts);
}
