<script lang="ts">
	import type { BadgeStatusTone } from '$lib/shared/statusTone';

	type BadgeColor = 'default' | 'success' | 'warning' | 'error' | 'info';

	export let text = '';
	export let color: BadgeColor = 'default';
	export let status: BadgeStatusTone | null = null;
	export let className = '';
	/**
	 * Whether this badge is a **live region** — that is, whether a screen reader
	 * should read it out when it changes.
	 *
	 * On by default, because a badge usually *is* the announcement: it appears when
	 * something starts running or fails and nothing else on screen says so. Off is
	 * for a badge that restates a state some other element already announces, and
	 * two live regions describing one fact is worse than one — the reader hears the
	 * same change twice, in an order nothing controls.
	 *
	 * The task panel's header chips are exactly that case: the verdict line below
	 * them is the panel's one `status` region and already reads the state as a
	 * sentence.
	 */
	export let announce = true;

	const COLORS = new Set<BadgeColor>(['default', 'success', 'warning', 'error', 'info']);
	const STATUSES = new Set<BadgeStatusTone>([
		'running',
		'paused',
		'failed',
		'attention',
		'completed'
	]);

	$: safeStatus = status && STATUSES.has(status) ? status : null;
	$: safeColor = COLORS.has(color) ? color : 'default';
	$: tone = safeStatus ? `status-${safeStatus}` : safeColor;
	$: badgeClass = ['native-badge', `native-badge--${tone}`, className].filter(Boolean).join(' ');
</script>

<span class={badgeClass} role={announce && tone !== 'default' ? 'status' : undefined}>
	{text}
</span>

<style>
	.native-badge {
		display: inline-flex;
		align-items: center;
		max-width: 100%;
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		padding: 0.125rem 0.5rem;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 600;
		line-height: 1.35;
		color: var(--text-secondary);
		background: var(--bg-soft);
		overflow-wrap: anywhere;
	}

	.native-badge--success,
	.native-badge--status-completed {
		border-color: color-mix(in srgb, var(--color-success, var(--status-completed)) 34%, transparent);
		background: color-mix(in srgb, var(--color-success, var(--status-completed)) 13%, transparent);
		color: var(--color-success, var(--status-completed));
	}

	.native-badge--warning,
	.native-badge--status-paused,
	.native-badge--status-attention {
		border-color: color-mix(in srgb, var(--color-warning, var(--status-attention)) 34%, transparent);
		background: color-mix(in srgb, var(--color-warning, var(--status-attention)) 13%, transparent);
		color: var(--color-warning, var(--status-attention));
	}

	.native-badge--error,
	.native-badge--status-failed {
		border-color: color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 13%, transparent);
		color: var(--color-error, var(--status-failed));
	}

	.native-badge--info,
	.native-badge--status-running {
		border-color: color-mix(in srgb, var(--accent-primary, var(--status-running)) 34%, transparent);
		background: color-mix(in srgb, var(--accent-primary, var(--status-running)) 13%, transparent);
		color: var(--accent-primary, var(--status-running));
	}

	:global([data-theme^='retro-16bit']) .native-badge {
		border-radius: 0;
		border-color: var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
		font-family: var(--font-mono);
		text-transform: uppercase;
	}
</style>
