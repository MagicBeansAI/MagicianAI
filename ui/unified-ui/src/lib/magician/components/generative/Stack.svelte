<script lang="ts">
	import { sanitizeCssKeyword, sanitizeCssLengthList } from './cssUtil';

export let direction: 'row' | 'column' = 'column';
export let align: string = 'stretch';
export let justify: string = 'flex-start';
export let gap: string = 'var(--space-sm)';
export let wrap: boolean = false;
export let className: string = '';

	const ALLOWED_ALIGN = ['stretch', 'flex-start', 'flex-end', 'center', 'baseline'] as const;
	const ALLOWED_JUSTIFY = [
		'flex-start',
		'flex-end',
		'center',
		'space-between',
		'space-around',
		'space-evenly'
	] as const;

	$: safeDirection = direction === 'row' ? 'row' : 'column';
	$: safeAlign = sanitizeCssKeyword(align, ALLOWED_ALIGN, 'stretch');
	$: safeJustify = sanitizeCssKeyword(justify, ALLOWED_JUSTIFY, 'flex-start');
	$: safeGap = sanitizeCssLengthList(gap, 'var(--space-sm)', 2, { allowNegative: false });
</script>

<div
	class="muij-stack {className}"
	style="flex-direction: {safeDirection}; align-items: {safeAlign}; justify-content: {safeJustify}; gap: {safeGap}; flex-wrap: {wrap ? 'wrap' : 'nowrap'};"
>
	<slot />
</div>

<style>
	.muij-stack {
		display: flex;
		overflow-wrap: anywhere;
	}
</style>
