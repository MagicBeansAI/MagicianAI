<script lang="ts">
	import { sanitizeCssNonNegativeLength, sanitizeCssValue } from './cssUtil';

	export let ratio: number = 0.5;
	export let direction: 'horizontal' | 'vertical' = 'horizontal';
	export let minFirst: string = '100px';
	export let minSecond: string = '100px';

	$: clampedRatio = Math.max(0.1, Math.min(0.9, Number.isFinite(+ratio) ? +ratio : 0.5));
	$: safeDirection = direction === 'vertical' ? 'vertical' : 'horizontal';
	$: isHorizontal = safeDirection === 'horizontal';
	$: safeMinFirst = sanitizeCssNonNegativeLength(minFirst, '100px');
	$: safeMinSecond = sanitizeCssNonNegativeLength(minSecond, '100px');
	// R576: Build complete style strings with explicit property names to avoid
	// CSS property interpolation and ensure all dynamic values are sanitized.
	$: firstPaneStyle = `flex-basis: ${sanitizeCssValue(clampedRatio * 100)}%; ${isHorizontal ? 'min-width' : 'min-height'}: ${safeMinFirst};`;
	$: secondPaneStyle = `flex: 1; ${isHorizontal ? 'min-width' : 'min-height'}: ${safeMinSecond};`;
</script>

	<div
		class="muij-split-panel"
		class:muij-split-horizontal={isHorizontal}
		class:muij-split-vertical={!isHorizontal}
>
	<div
		class="muij-split-pane muij-split-first"
		style={firstPaneStyle}
	>
		<slot />
	</div>
	<div class="muij-split-divider" role="separator" aria-orientation={safeDirection === 'horizontal' ? 'vertical' : 'horizontal'}></div>
	<div
		class="muij-split-pane muij-split-second"
		style={secondPaneStyle}
	>
		<slot name="second" />
	</div>
</div>

<style>
	.muij-split-panel {
		display: flex;
		overflow: hidden;
	}

	.muij-split-horizontal {
		flex-direction: row;
	}

	.muij-split-vertical {
		flex-direction: column;
	}

	.muij-split-pane {
		overflow: auto;
		overflow-wrap: anywhere;
		/* R337: Prevent flex items from shrinking below min-width/min-height */
		flex-shrink: 0;
	}

	.muij-split-divider {
		flex-shrink: 0;
		background: var(--border-soft);
	}

	.muij-split-horizontal > .muij-split-divider {
		width: 1px;
		align-self: stretch;
	}

	.muij-split-vertical > .muij-split-divider {
		height: 1px;
		align-self: stretch;
	}
</style>
