<script lang="ts">
	import { sanitizeCssNonNegativeLength } from './cssUtil';

	export let variant: 'text' | 'circle' | 'rect' = 'text';
	export let animate: boolean = true;
	export let width: string = '100%';
	export let height: string = '';

	$: safeVariant = variant === 'circle' || variant === 'rect' ? variant : 'text';
	$: defaultHeight = safeVariant === 'text' ? '12px' : safeVariant === 'circle' ? '32px' : '64px';
	$: resolvedHeight = height || defaultHeight;
	$: safeWidth = sanitizeCssNonNegativeLength(width, '100%');
	$: safeHeight = sanitizeCssNonNegativeLength(resolvedHeight, defaultHeight);
</script>

<div
	class={`muij-skeleton muij-skeleton-${safeVariant} ${animate ? 'muij-skeleton-animate' : ''}`}
	style={`width:${safeWidth};height:${safeHeight};`}
	aria-hidden="true"
></div>

<style>
	.muij-skeleton {
		background: linear-gradient(
			90deg,
			color-mix(in srgb, var(--bg-soft) 88%, white) 25%,
			color-mix(in srgb, var(--bg-soft) 70%, white) 50%,
			color-mix(in srgb, var(--bg-soft) 88%, white) 75%
		);
		background-size: 220% 100%;
		border-radius: var(--radius-sm);
	}

	.muij-skeleton-circle {
		border-radius: 999px;
	}

	.muij-skeleton-animate {
		animation: muij-skeleton-wave 1.2s ease infinite;
	}

	@keyframes muij-skeleton-wave {
		0% { background-position: 200% 0; }
		100% { background-position: -20% 0; }
	}
</style>
