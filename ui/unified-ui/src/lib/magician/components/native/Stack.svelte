<script lang="ts">
	export let direction: 'row' | 'column' = 'column';
	export let align = 'stretch';
	export let justify = 'flex-start';
	export let gap = 'var(--space-sm)';
	export let wrap = false;
	export let className = '';

	const ALIGN = new Set(['stretch', 'flex-start', 'flex-end', 'center', 'baseline']);
	const JUSTIFY = new Set([
		'flex-start',
		'flex-end',
		'center',
		'space-between',
		'space-around',
		'space-evenly'
	]);

	$: safeDirection = direction === 'row' ? 'row' : 'column';
	$: safeAlign = ALIGN.has(align) ? align : 'stretch';
	$: safeJustify = JUSTIFY.has(justify) ? justify : 'flex-start';
</script>

<div
	class={['native-stack', className].filter(Boolean).join(' ')}
	style={`flex-direction:${safeDirection};align-items:${safeAlign};justify-content:${safeJustify};gap:${gap};flex-wrap:${wrap ? 'wrap' : 'nowrap'};`}
>
	<slot />
</div>

<style>
	.native-stack {
		display: flex;
		min-width: 0;
		overflow-wrap: anywhere;
	}
</style>
