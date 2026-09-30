<script lang="ts" context="module">
	let tooltipSequence = 0;

	function nextTooltipId(): string {
		tooltipSequence += 1;
		return `muij-tooltip-${tooltipSequence}`;
	}
</script>

<script lang="ts">
	export let content: string = '';
	export let position: 'top' | 'bottom' | 'left' | 'right' = 'top';
	const tooltipId = nextTooltipId();

	$: safePosition = ['bottom', 'left', 'right'].includes(position) ? position : 'top';
	$: hasContent = content.trim().length > 0;
</script>

<span
	class="muij-tooltip-wrap"
	aria-describedby={hasContent ? tooltipId : undefined}
>
	<slot />
	{#if hasContent}
		<span id={tooltipId} class={`muij-tooltip-bubble muij-tooltip-${safePosition}`} role="tooltip">{content}</span>
	{/if}
</span>

<style>
	.muij-tooltip-wrap {
		position: relative;
		display: inline-flex;
		align-items: center;
	}

	.muij-tooltip-bubble {
		position: absolute;
		z-index: 10;
		max-width: 240px;
		padding: 6px 8px;
		border-radius: var(--radius-sm);
		background: #111827;
		color: #f8fafc;
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		line-height: 1.3;
		white-space: normal;
		overflow-wrap: anywhere;
		opacity: 0;
		visibility: hidden;
		pointer-events: none;
		transition: opacity 120ms ease;
	}

	.muij-tooltip-wrap:hover .muij-tooltip-bubble,
	.muij-tooltip-wrap:focus-within .muij-tooltip-bubble {
		opacity: 1;
		visibility: visible;
	}

	.muij-tooltip-top {
		bottom: calc(100% + 8px);
		left: 50%;
		transform: translateX(-50%);
	}

	.muij-tooltip-bottom {
		top: calc(100% + 8px);
		left: 50%;
		transform: translateX(-50%);
	}

	.muij-tooltip-left {
		right: calc(100% + 8px);
		top: 50%;
		transform: translateY(-50%);
	}

	.muij-tooltip-right {
		left: calc(100% + 8px);
		top: 50%;
		transform: translateY(-50%);
	}
</style>
