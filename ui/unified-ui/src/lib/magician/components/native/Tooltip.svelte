<script lang="ts" context="module">
	let nativeTooltipSequence = 0;

	function nextNativeTooltipId(): string {
		nativeTooltipSequence += 1;
		return `native-tooltip-${nativeTooltipSequence}`;
	}
</script>

<script lang="ts">
	export let content = '';
	export let position: 'top' | 'bottom' | 'left' | 'right' = 'top';

	const tooltipId = nextNativeTooltipId();

	$: safePosition = position === 'bottom' || position === 'left' || position === 'right' ? position : 'top';
	$: hasContent = content.trim().length > 0;
</script>

<span class="native-tooltip" aria-describedby={hasContent ? tooltipId : undefined}>
	<slot />
	{#if hasContent}
		<span
			id={tooltipId}
			class="native-tooltip__bubble native-tooltip__bubble--{safePosition}"
			role="tooltip"
		>
			{content}
		</span>
	{/if}
</span>

<style>
	.native-tooltip {
		position: relative;
		display: inline-flex;
		align-items: center;
		min-width: 0;
	}

	.native-tooltip__bubble {
		position: absolute;
		z-index: 10;
		max-width: 15rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 35%, transparent);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--bg-inverse) 94%, transparent);
		color: var(--text-inverse);
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		line-height: 1.35;
		opacity: 0;
		overflow-wrap: anywhere;
		padding: 0.375rem 0.5rem;
		pointer-events: none;
		transition: opacity 120ms ease;
		visibility: hidden;
		white-space: normal;
	}

	.native-tooltip:hover .native-tooltip__bubble,
	.native-tooltip:focus-within .native-tooltip__bubble {
		opacity: 1;
		visibility: visible;
	}

	.native-tooltip__bubble--top {
		bottom: calc(100% + 0.5rem);
		left: 50%;
		transform: translateX(-50%);
	}

	.native-tooltip__bubble--bottom {
		left: 50%;
		top: calc(100% + 0.5rem);
		transform: translateX(-50%);
	}

	.native-tooltip__bubble--left {
		right: calc(100% + 0.5rem);
		top: 50%;
		transform: translateY(-50%);
	}

	.native-tooltip__bubble--right {
		left: calc(100% + 0.5rem);
		top: 50%;
		transform: translateY(-50%);
	}
</style>
