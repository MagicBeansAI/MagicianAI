<script lang="ts">
	import { sanitizeCssLengthList } from './cssUtil';

	export let direction: 'row' | 'column' = 'column';
	export let gap: string = 'var(--space-md)';
	export let padding: string = '0';

	$: safeDirection = direction === 'row' ? 'row' : 'column';
	$: safeGap = sanitizeCssLengthList(gap, 'var(--space-md)', 2, { allowNegative: false });
	$: safePadding = sanitizeCssLengthList(padding, '0', 4, { allowNegative: false });
</script>

<div
	class="muij-container"
	style="flex-direction: {safeDirection}; gap: {safeGap}; padding: {safePadding};"
>
	<slot />
</div>

<style>
	.muij-container {
		display: flex;
		overflow-wrap: anywhere;
	}
</style>
