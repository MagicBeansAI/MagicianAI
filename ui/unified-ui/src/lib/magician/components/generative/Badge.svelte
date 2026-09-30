<script lang="ts">
	import type { BadgeStatusTone } from '$lib/shared/statusTone';

	export let text: string = '';
	export let color: 'default' | 'success' | 'warning' | 'error' | 'info' | 'primary' = 'default';
	/**
	 * Operational status mode. When set, the badge ignores `color` and
	 * renders using the theme's `--status-*` tokens, giving every theme
	 * a consistent semantic palette for the five execution states.
	 */
	export let status: BadgeStatusTone | null = null;
	/** Extra class hook so page CSS (e.g. presto task chip bands) can size the badge. */
	export let className: string = '';

	$: safeColor = (
		color === 'success' || color === 'warning' || color === 'error' || color === 'info' || color === 'primary'
			? color
			: 'default'
	) as 'default' | 'success' | 'warning' | 'error' | 'info' | 'primary';
	$: variantClass = status ? `muij-badge-status-${status}` : `muij-badge-${safeColor}`;
	$: ariaModifier = status ?? (safeColor !== 'default' ? safeColor : null);
</script>

<!-- R661: aria-label conveys semantic color meaning to screen readers (WCAG 1.4.1) -->
<span
	class="muij-badge {variantClass} {className}"
	role={ariaModifier ? 'status' : undefined}
	aria-label={ariaModifier ? `${text} (${ariaModifier})` : undefined}
>{text}</span>

<style>
	.muij-badge {
		display: inline-block;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		font-weight: 500;
		padding: 2px 8px;
		border-radius: var(--radius-full, 9999px);
		line-height: 1.4;
		white-space: nowrap;
	}

	.muij-badge-default {
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.muij-badge-success {
		background: color-mix(in srgb, var(--color-success) 15%, transparent);
		color: var(--color-success);
	}

	.muij-badge-warning {
		background: color-mix(in srgb, var(--color-warning, #f59e0b) 15%, transparent);
		color: var(--color-warning, #f59e0b);
	}

	.muij-badge-error {
		background: color-mix(in srgb, var(--color-error) 15%, transparent);
		color: var(--color-error);
	}

	.muij-badge-info,
	.muij-badge-primary {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		color: var(--accent-primary);
	}

	/* Operational status palette — pulls from the theme's --status-* tokens
	   so every theme picks up theme-appropriate values via the cascade. */
	.muij-badge-status-running {
		background: var(--status-running-soft, color-mix(in srgb, var(--status-running, #4d9de0) 15%, transparent));
		color: var(--status-running, #4d9de0);
	}

	.muij-badge-status-paused {
		background: var(--status-paused-soft, color-mix(in srgb, var(--status-paused, #ffe66d) 22%, transparent));
		color: color-mix(in srgb, var(--status-paused, #ffe66d) 60%, var(--text-primary, #2d2a26));
	}

	.muij-badge-status-failed {
		background: var(--status-failed-soft, color-mix(in srgb, var(--status-failed, #ff6b6b) 14%, transparent));
		color: var(--status-failed, #ff6b6b);
	}

	.muij-badge-status-attention {
		background: var(--status-attention-soft, color-mix(in srgb, var(--status-attention, #ff6b6b) 12%, transparent));
		color: var(--status-attention, #ff6b6b);
	}

	.muij-badge-status-completed {
		background: var(--status-completed-soft, color-mix(in srgb, var(--status-completed, #00bb7f) 14%, transparent));
		color: var(--status-completed, #00bb7f);
	}
</style>
