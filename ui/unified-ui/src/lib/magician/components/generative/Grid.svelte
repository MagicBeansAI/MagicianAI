<script lang="ts">
	import { sanitizeCssLengthList, sanitizeCssNonNegativeLength } from './cssUtil';

	export let columns: number = 2;
	export let gap: string = 'var(--space-md)';
	export let autoFit: boolean = false;
	export let minColumnWidth: string = '200px';
	export let className: string = '';

	$: safeGap = sanitizeCssLengthList(gap, 'var(--space-md)', 2, { allowNegative: false });
	$: safeMinColWidth = sanitizeCssNonNegativeLength(minColumnWidth, '200px');
	$: safeColumns = Math.max(1, Math.min(12, Number.isFinite(+columns) ? Math.floor(+columns) : 2));
	$: gridColumns = autoFit
		? `repeat(auto-fit, minmax(${safeMinColWidth}, 1fr))`
		: `repeat(${safeColumns}, 1fr)`;
</script>

	<div
		class="muij-grid {className}"
		class:muij-grid--multi={safeColumns >= 2 && !autoFit}
		style="--muij-grid-columns: {gridColumns}; --muij-grid-gap: {safeGap};"
	>
	<slot />
</div>

<style>
	.muij-grid {
		display: grid;
		overflow-wrap: anywhere;
		grid-template-columns: var(--muij-grid-columns);
		gap: var(--muij-grid-gap);
	}

	/* Responsive: any fixed multi-column grid collapses to a single column on
	   narrow viewports. Chart-on-top, table-below is the natural reading
	   order, so vertical stacking preserves the dashboard semantics. */
	@media (max-width: 720px) {
		.muij-grid.muij-grid--multi {
			grid-template-columns: 1fr;
		}
	}
</style>
