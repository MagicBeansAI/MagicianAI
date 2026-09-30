<script lang="ts">
	import { sanitizeCssNonNegativeLength } from './cssUtil';

	export let orientation: 'horizontal' | 'vertical' = 'horizontal';
	export let variant: 'solid' | 'dashed' | 'dotted' = 'solid';
	export let spacing: string = 'var(--space-md)';

	$: safeOrientation = (orientation === 'vertical' ? 'vertical' : 'horizontal') as 'horizontal' | 'vertical';
	$: safeVariant = (variant === 'dashed' || variant === 'dotted' ? variant : 'solid') as 'solid' | 'dashed' | 'dotted';
	$: safeSpacing = sanitizeCssNonNegativeLength(spacing, 'var(--space-md)');
</script>

<div
	class="muij-divider"
	class:muij-divider-horizontal={safeOrientation === 'horizontal'}
	class:muij-divider-vertical={safeOrientation === 'vertical'}
	style="border-style: {safeVariant}; --muij-divider-spacing: {safeSpacing};"
	role="separator"
	aria-orientation={safeOrientation}
></div>

<style>
	.muij-divider {
		border-color: var(--border-soft);
		flex-shrink: 0;
	}

	.muij-divider-horizontal {
		border-width: 1px 0 0 0;
		width: 100%;
		margin: var(--muij-divider-spacing) 0;
	}

	.muij-divider-vertical {
		border-width: 0 0 0 1px;
		height: 100%;
		margin: 0 var(--muij-divider-spacing);
		align-self: stretch;
	}
</style>
