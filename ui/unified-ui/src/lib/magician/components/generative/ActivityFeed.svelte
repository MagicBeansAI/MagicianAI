<script lang="ts">
	/**
	 * ActivityFeed Component — GD-F03-C
	 *
	 * Timeline-style activity feed.
	 */

	interface ActivityItem {
		id: string;
		type: string;
		actor: string;
		avatar?: string;
		action: string;
		target?: string;
		timestamp: number;
	}

	export let items: unknown = [];
	export let maxItems: unknown = undefined;

	function isActivityItem(item: unknown): item is ActivityItem {
		return (
			typeof item === 'object' &&
			item !== null &&
			typeof (item as Record<string, unknown>).id === 'string' &&
			typeof (item as Record<string, unknown>).type === 'string' &&
			typeof (item as Record<string, unknown>).actor === 'string' &&
			typeof (item as Record<string, unknown>).action === 'string' &&
			typeof (item as Record<string, unknown>).timestamp === 'number'
		);
	}

	function normalizeItems(value: unknown): ActivityItem[] {
		if (!Array.isArray(value)) return [];
		return value.filter(isActivityItem);
	}

	function toNumber(value: unknown): number | undefined {
		if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
			return Math.floor(value);
		}
		return undefined;
	}

	function getActivityIcon(type: string): string {
		switch (type) {
			case 'comment':
				return '💬';
			case 'update':
				return '✏️';
			case 'complete':
				return '✅';
			case 'create':
				return '➕';
			case 'delete':
				return '🗑️';
			case 'approve':
				return '👍';
			case 'reject':
				return '👎';
			default:
				return '📌';
		}
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

	$: safeItems = normalizeItems(items);
	$: safeMaxItems = toNumber(maxItems);
	$: visibleItems = safeMaxItems ? safeItems.slice(0, safeMaxItems) : safeItems;
</script>

{#if visibleItems.length > 0}
	<ul class="muij-activity-feed" role="feed" aria-label="Activity feed">
		{#each visibleItems as item}
			<li class="muij-activity-item">
				<span class="muij-activity-icon" aria-hidden="true">
					{getActivityIcon(item.type)}
				</span>
				<div class="muij-activity-content">
					<div class="muij-activity-header">
						{#if item.avatar}
							<img class="muij-activity-avatar" src={item.avatar} alt={item.actor} />
						{/if}
						<span class="muij-activity-actor">{item.actor}</span>
						<span class="muij-activity-action">{item.action}</span>
						{#if item.target}
							<span class="muij-activity-target">{item.target}</span>
						{/if}
					</div>
					<time class="muij-activity-time" datetime={new Date(item.timestamp).toISOString()}>
						{formatRelativeTime(item.timestamp)}
					</time>
				</div>
			</li>
		{/each}
	</ul>
{:else}
	<div class="muij-activity-empty">
		<span class="muij-activity-empty-text">No recent activity</span>
	</div>
{/if}

<style>
	.muij-activity-feed {
		list-style: none;
		margin: 0;
		padding: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.muij-activity-item {
		display: flex;
		gap: var(--space-sm);
		padding: var(--space-sm) 0;
		border-bottom: 1px solid var(--border-soft);
	}

	.muij-activity-item:last-child {
		border-bottom: none;
	}

	.muij-activity-icon {
		flex-shrink: 0;
		font-size: 1rem;
		width: 1.5rem;
		text-align: center;
	}

	.muij-activity-content {
		flex: 1;
		min-width: 0;
	}

	.muij-activity-header {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		flex-wrap: wrap;
	}

	.muij-activity-avatar {
		width: 20px;
		height: 20px;
		border-radius: 50%;
		object-fit: cover;
	}

	.muij-activity-actor {
		font-weight: 500;
		color: var(--text-primary);
	}

	.muij-activity-action {
		color: var(--text-secondary);
	}

	.muij-activity-target {
		color: var(--accent-primary);
		font-weight: 500;
	}

	.muij-activity-time {
		display: block;
		margin-top: 2px;
		font-size: 0.6875rem;
		color: var(--text-muted);
	}

	.muij-activity-empty {
		padding: var(--space-md);
		text-align: center;
		color: var(--text-muted);
		font-size: 0.75rem;
	}
</style>
