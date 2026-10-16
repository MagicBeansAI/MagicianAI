<script lang="ts">
	import { sanitizeCssValue } from './cssUtil';

	export let data: number[] = [];
	export let width: number = 120;
	export let height: number = 36;
	export let color: string = 'var(--theme-color-accent, var(--accent-primary))';
	export let strokeWidth: number = 2;

	function toFiniteNumber(value: unknown): number | undefined {
		if (typeof value !== 'number' || !Number.isFinite(value)) return undefined;
		return value;
	}

	function linePath(values: number[], safeWidth: number, safeHeight: number): string {
		if (values.length === 0) return '';
		if (values.length === 1) {
			const mid = safeHeight / 2;
			return `M 0 ${mid} L ${safeWidth} ${mid}`;
		}
		const min = Math.min(...values);
		const max = Math.max(...values);
		if (max === min) {
			const mid = safeHeight / 2;
			return values
				.map((_, index) => {
					const x = (index / (values.length - 1)) * safeWidth;
					return `${index === 0 ? 'M' : 'L'} ${x} ${mid}`;
				})
				.join(' ');
		}
		const span = max - min;
		return values
			.map((value, index) => {
				const x = (index / (values.length - 1)) * safeWidth;
				const y = safeHeight - ((value - min) / span) * safeHeight;
				return `${index === 0 ? 'M' : 'L'} ${x} ${y}`;
			})
			.join(' ');
	}

	$: safeWidth = Number.isFinite(width) && width >= 40 ? width : 120;
	$: safeHeight = Number.isFinite(height) && height >= 20 ? height : 36;
	$: safeStrokeWidth = Number.isFinite(strokeWidth) && strokeWidth > 0 ? strokeWidth : 2;
	$: safeColor = sanitizeCssValue(color).trim() || 'var(--theme-color-accent, var(--accent-primary))';
	$: values = data
		.map((entry) => toFiniteNumber(entry))
		.filter((entry): entry is number => entry !== undefined);
	$: path = linePath(values, safeWidth, safeHeight);
	$: minValue = values.length > 0 ? Math.min(...values) : null;
	$: maxValue = values.length > 0 ? Math.max(...values) : null;
</script>

<div class="muij-sparkline">
	{#if values.length === 0}
		<div class="muij-sparkline-empty">No data</div>
	{:else}
		<svg
			class="muij-sparkline-svg"
			viewBox={`0 0 ${safeWidth} ${safeHeight}`}
			role="img"
			aria-label="Sparkline"
		>
				<path
					class="muij-sparkline-path"
					d={path}
					stroke={safeColor}
					stroke-width={safeStrokeWidth}
					fill="none"
					stroke-linecap="round"
				stroke-linejoin="round"
			/>
		</svg>
		<div class="muij-sparkline-range">
			<span>min {minValue?.toLocaleString()}</span>
			<span>max {maxValue?.toLocaleString()}</span>
		</div>
	{/if}
</div>

<style>
	.muij-sparkline {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 0;
	}

	.muij-sparkline-svg {
		width: 100%;
		height: auto;
		overflow: visible;
	}

	.muij-sparkline-path {
		animation: muij-sparkline-draw 450ms ease;
	}

	.muij-sparkline-range {
		display: flex;
		justify-content: space-between;
		gap: 8px;
		font-family: var(--font-primary);
		font-size: 0.625rem;
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.muij-sparkline-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}

	@keyframes muij-sparkline-draw {
		from {
			opacity: 0;
			transform: translateY(3px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
</style>
