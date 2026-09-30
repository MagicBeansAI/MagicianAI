<script lang="ts">
	/**
	 * CommentThread Component — GD-F03-D
	 *
	 * Recursive comment display with nesting support.
	 */

	interface Comment {
		id: string;
		author: string;
		avatar?: string;
		content: string;
		timestamp: number;
		replies?: Comment[];
	}

	export let comments: unknown = [];
	export let maxDepth: unknown = 3;

	function isComment(item: unknown): item is Comment {
		return (
			typeof item === 'object' &&
			item !== null &&
			typeof (item as Record<string, unknown>).id === 'string' &&
			typeof (item as Record<string, unknown>).author === 'string' &&
			typeof (item as Record<string, unknown>).content === 'string' &&
			typeof (item as Record<string, unknown>).timestamp === 'number'
		);
	}

	function normalizeComments(value: unknown): Comment[] {
		if (!Array.isArray(value)) return [];
		return value.filter(isComment).map((c) => ({
			...c,
			replies: c.replies ? normalizeComments(c.replies) : []
		}));
	}

	function toNumber(value: unknown, fallback: number): number {
		if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
			return Math.floor(value);
		}
		return fallback;
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	function formatRelativeTime(timestamp: number): string {
		const now = Date.now();
		const diffMs = Math.max(0, now - timestamp);
		const diffSec = Math.floor(diffMs / 1000);
		if (diffSec < 60) return 'just now';
		const diffMin = Math.floor(diffSec / 60);
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		if (diffHr < 24) return `${diffHr}h ago`;
		const diffDays = Math.floor(diffHr / 24);
		if (diffDays < 7) return `${diffDays}d ago`;
		return new Date(timestamp).toLocaleDateString();
	}

	function getInitials(name: string): string {
		if (!name || !name.trim()) return '??';
		return name.trim().slice(0, 2).toUpperCase();
	}

	$: safeComments = normalizeComments(comments);
	$: safeMaxDepth = toNumber(maxDepth, 3);
</script>

{#if safeComments.length > 0}
	<ul class="muij-comment-thread" role="feed" aria-label="Comments">
		{#each safeComments as comment}
			<li class="muij-comment">
				<div class="muij-comment-header">
					{#if comment.avatar}
						<img class="muij-comment-avatar" src={comment.avatar} alt={comment.author} />
					{:else}
						<div class="muij-comment-avatar-placeholder">
							{getInitials(comment.author)}
						</div>
					{/if}
					<span class="muij-comment-author">{comment.author}</span>
					<time class="muij-comment-time" datetime={new Date(comment.timestamp).toISOString()}>
						{formatRelativeTime(comment.timestamp)}
					</time>
				</div>
				<div class="muij-comment-content">
					{comment.content}
				</div>
				{#if comment.replies && comment.replies.length > 0 && safeMaxDepth > 1}
					<div class="muij-comment-replies">
						<svelte:self
							comments={comment.replies}
							maxDepth={safeMaxDepth - 1}
						/>
					</div>
				{/if}
			</li>
		{/each}
	</ul>
{:else}
	<div class="muij-comment-empty">
		<span class="muij-comment-empty-text">No comments yet</span>
	</div>
{/if}

<style>
	.muij-comment-thread {
		list-style: none;
		margin: 0;
		padding: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.muij-comment {
		padding: var(--space-sm) 0;
		border-bottom: 1px solid var(--border-soft);
	}

	.muij-comment:last-child {
		border-bottom: none;
	}

	.muij-comment-header {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		margin-bottom: var(--space-xs);
	}

	.muij-comment-avatar {
		width: 24px;
		height: 24px;
		border-radius: 50%;
		object-fit: cover;
	}

	.muij-comment-avatar-placeholder {
		width: 24px;
		height: 24px;
		border-radius: 50%;
		background: var(--bg-soft);
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 0.625rem;
		font-weight: 600;
		color: var(--text-muted);
	}

	.muij-comment-author {
		font-weight: 500;
		color: var(--text-primary);
	}

	.muij-comment-time {
		font-size: 0.6875rem;
		color: var(--text-muted);
	}

	.muij-comment-content {
		color: var(--text-body);
		line-height: 1.5;
		padding-left: calc(24px + var(--space-xs));
	}

	.muij-comment-replies {
		margin-top: var(--space-sm);
		margin-left: calc(24px + var(--space-sm));
		padding-left: var(--space-sm);
		border-left: 2px solid var(--border-soft);
	}

	.muij-comment-empty {
		padding: var(--space-md);
		text-align: center;
		color: var(--text-muted);
		font-size: 0.75rem;
	}
</style>
