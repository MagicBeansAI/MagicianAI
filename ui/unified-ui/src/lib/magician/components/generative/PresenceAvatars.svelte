<script lang="ts">
	/**
	 * PresenceAvatars Component — GD-F03-B
	 *
	 * Stacked avatar display showing who's present.
	 */

	interface PresenceUser {
		id: string;
		name: string;
		avatar?: string;
		status?: 'online' | 'away' | 'offline';
	}

	export let users: unknown = [];
	export let maxVisible: unknown = 4;
	export let size: unknown = 'md';

	function isUser(item: unknown): item is PresenceUser {
		return (
			typeof item === 'object' &&
			item !== null &&
			typeof (item as Record<string, unknown>).id === 'string' &&
			typeof (item as Record<string, unknown>).name === 'string'
		);
	}

	function normalizeUsers(value: unknown): PresenceUser[] {
		if (!Array.isArray(value)) return [];
		return value.filter(isUser);
	}

	function toNumber(value: unknown, fallback: number): number {
		if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
			return Math.floor(value);
		}
		return fallback;
	}

	function toSize(value: unknown): 'sm' | 'md' | 'lg' {
		if (value === 'sm' || value === 'md' || value === 'lg') return value;
		return 'md';
	}

	function getInitials(name: string): string {
		if (!name || !name.trim()) return '??';
		const trimmed = name.trim();
		const parts = trimmed.split(/\s+/).filter((p) => p.length > 0);
		if (parts.length === 0) return '??';
		return parts
			.map((word) => word[0])
			.join('')
			.slice(0, 2)
			.toUpperCase();
	}

	function getStatusColor(status?: string): string {
		switch (status) {
			case 'online':
				return '#22c55e';
			case 'away':
							return '#f59e0b';
			default:
				return '#9ca3af';
		}
	}

	$: safeUsers = normalizeUsers(users);
	$: safeMaxVisible = toNumber(maxVisible, 4);
	$: safeSize = toSize(size);

	$: visibleUsers = safeUsers.slice(0, safeMaxVisible);
	$: overflowCount = safeUsers.length - safeMaxVisible;

	$: sizeClass = `muij-avatar-${safeSize}`;

	function handleAvatarError(event: Event): void {
		const target = event.target as HTMLImageElement;
		target.style.display = 'none';
	}
</script>

{#if safeUsers.length > 0}
	<div class="muij-presence" role="group" aria-label="{safeUsers.length} users present">
		<div class="muij-presence-stack">
			{#each visibleUsers as user, i}
				<div
					class="muij-avatar {sizeClass}"
					style:z-index={visibleUsers.length - i}
					title={user.name}
				>
					{#if user.avatar}
						<img
							class="muij-avatar-img"
							src={user.avatar}
							alt={user.name}
							on:error={handleAvatarError}
						/>
						<span class="muij-avatar-initials muij-avatar-fallback">{getInitials(user.name)}</span>
					{:else}
						<span class="muij-avatar-initials">{getInitials(user.name)}</span>
					{/if}
					<span
						class="muij-avatar-status"
						style:background-color={getStatusColor(user.status)}
						aria-label={user.status || 'offline'}
					></span>
				</div>
			{/each}
		</div>
		{#if overflowCount > 0}
			<span class="muij-presence-overflow">+{overflowCount}</span>
		{/if}
	</div>
{:else}
	<div class="muij-presence-empty">
		<span class="muij-presence-empty-text">No one present</span>
	</div>
{/if}

<style>
	.muij-presence {
		display: inline-flex;
		align-items: center;
		gap: var(--space-xs);
		font-family: var(--font-primary);
	}

	.muij-presence-stack {
		display: flex;
		flex-direction: row-reverse;
		justify-content: flex-end;
	}

	.muij-avatar {
		position: relative;
		flex-shrink: 0;
		border-radius: 50%;
		background: var(--bg-soft);
		border: 2px solid var(--bg-card);
		margin-left: -8px;
		overflow: hidden;
	}

	.muij-avatar:first-child {
		margin-left: 0;
	}

	.muij-avatar-sm {
		width: 24px;
		height: 24px;
	}

	.muij-avatar-md {
		width: 32px;
		height: 32px;
	}

	.muij-avatar-lg {
		width: 40px;
		height: 40px;
	}

	.muij-avatar-img {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.muij-avatar-initials {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 100%;
		height: 100%;
		font-size: 0.625rem;
		font-weight: 600;
		color: var(--text-secondary);
	}

	.muij-avatar-fallback {
		position: absolute;
		top: 0;
		left: 0;
		opacity: 0;
		transition: opacity 0.15s ease;
	}

	.muij-avatar-img[src] + .muij-avatar-fallback {
		opacity: 0;
	}

	.muij-avatar:has(.muij-avatar-img:not([src])) .muij-avatar-fallback,
	.muij-avatar:has(.muij-avatar-img:error) .muij-avatar-fallback {
		opacity: 1;
	}

	.muij-avatar-lg .muij-avatar-initials {
		font-size: 0.75rem;
	}

	.muij-avatar-status {
		position: absolute;
		bottom: 0;
		right: 0;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		border: 2px solid var(--bg-card);
	}

	.muij-presence-overflow {
		font-size: 0.75rem;
		color: var(--text-muted);
		padding-left: var(--space-xs);
	}

	.muij-presence-empty {
		color: var(--text-muted);
		font-size: 0.75rem;
	}
</style>
