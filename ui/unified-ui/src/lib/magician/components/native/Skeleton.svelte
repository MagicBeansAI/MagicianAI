<script lang="ts">
	export let variant: 'text' | 'circle' | 'rect' = 'text';
	export let animate = true;
	export let width = '100%';
	export let height = '';

	$: safeVariant = variant === 'circle' || variant === 'rect' ? variant : 'text';
	$: resolvedHeight = height || (safeVariant === 'text' ? '12px' : safeVariant === 'circle' ? '32px' : '64px');
	$: skeletonClass = [
		'native-skeleton',
		`native-skeleton--${safeVariant}`,
		animate ? 'native-skeleton--animate' : ''
	]
		.filter(Boolean)
		.join(' ');
</script>

<div class={skeletonClass} style={`width:${width};height:${resolvedHeight};`} aria-hidden="true"></div>

<style>
	.native-skeleton {
		border-radius: var(--radius-sm, 6px);
		background: linear-gradient(
			90deg,
			color-mix(in srgb, var(--bg-soft) 88%, white) 25%,
			color-mix(in srgb, var(--bg-soft) 68%, white) 50%,
			color-mix(in srgb, var(--bg-soft) 88%, white) 75%
		);
		background-size: 220% 100%;
	}

	.native-skeleton--circle {
		border-radius: 999px;
	}

	.native-skeleton--animate {
		animation: native-skeleton-wave 1.2s ease infinite;
	}

	@keyframes native-skeleton-wave {
		0% {
			background-position: 200% 0;
		}
		100% {
			background-position: -20% 0;
		}
	}
</style>
