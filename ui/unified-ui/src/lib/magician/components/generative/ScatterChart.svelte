<script lang="ts">
	import { sanitizeCssValue } from './cssUtil';

	const PADDING = { top: 16, right: 14, bottom: 24, left: 28 };
	const DEFAULT_COLOR = 'var(--theme-color-accent, var(--theme-chart-color-0, #3b82f6))';

	interface ScatterPoint {
		x: number;
		y: number;
		size?: number;
		color?: string;
		label?: string;
	}

	export let points: ScatterPoint[] = [];
	export let width: number = 360;
	export let height: number = 220;

	interface RenderPoint {
		x: number;
		y: number;
		size: number;
		color: string;
		label: string;
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return value != null && typeof value === 'object' && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function toFiniteNumber(value: unknown): number | undefined {
		if (typeof value !== 'number' || !Number.isFinite(value)) return undefined;
		return value;
	}

	function normalizeColor(value: unknown): string | undefined {
		if (typeof value !== 'string') return undefined;
		const trimmed = sanitizeCssValue(value).trim();
		return trimmed.length > 0 ? trimmed : undefined;
	}

	function normalizeLabel(value: unknown, fallback: string): string {
		if (typeof value === 'string' && value.trim().length > 0) return value.trim();
		return fallback;
	}

	$: safeWidth = Number.isFinite(width) && width >= 120 ? width : 360;
	$: safeHeight = Number.isFinite(height) && height >= 120 ? height : 220;
	$: plotWidth = safeWidth - PADDING.left - PADDING.right;
	$: plotHeight = safeHeight - PADDING.top - PADDING.bottom;

	$: normalizedPoints = points
		.map((entry, index) => {
			const rec = asRecord(entry);
			if (!rec) return null;
			const x = toFiniteNumber(rec.x);
			const y = toFiniteNumber(rec.y);
			if (x === undefined || y === undefined) return null;
			const size = toFiniteNumber(rec.size);
			return {
				x,
				y,
				size: size !== undefined ? Math.min(12, Math.max(2, size)) : 4,
				color: normalizeColor(rec.color) || DEFAULT_COLOR,
				label: normalizeLabel(rec.label, `Point ${index + 1}`)
			};
		})
		.filter((entry): entry is RenderPoint => entry !== null);

	$: domain = (() => {
		if (normalizedPoints.length === 0) {
			return { minX: 0, maxX: 1, minY: 0, maxY: 1 };
		}
		let minX = Math.min(...normalizedPoints.map((point) => point.x));
		let maxX = Math.max(...normalizedPoints.map((point) => point.x));
		let minY = Math.min(...normalizedPoints.map((point) => point.y));
		let maxY = Math.max(...normalizedPoints.map((point) => point.y));
		if (minX === maxX) {
			minX -= 1;
			maxX += 1;
		}
		if (minY === maxY) {
			minY -= 1;
			maxY += 1;
		}
		return { minX, maxX, minY, maxY };
	})();

	function mapX(value: number): number {
		return PADDING.left + ((value - domain.minX) / (domain.maxX - domain.minX)) * plotWidth;
	}

	function mapY(value: number): number {
		return PADDING.top + (1 - (value - domain.minY) / (domain.maxY - domain.minY)) * plotHeight;
	}
</script>

<div class="muij-scatter-chart">
	{#if normalizedPoints.length === 0}
		<div class="muij-scatter-empty">No data</div>
	{:else}
		<svg class="muij-scatter-svg" viewBox={`0 0 ${safeWidth} ${safeHeight}`} role="img" aria-label="Scatter chart">
			<line class="muij-scatter-axis" x1={PADDING.left} y1={safeHeight - PADDING.bottom} x2={safeWidth - PADDING.right} y2={safeHeight - PADDING.bottom}></line>
			<line class="muij-scatter-axis" x1={PADDING.left} y1={PADDING.top} x2={PADDING.left} y2={safeHeight - PADDING.bottom}></line>
			{#each normalizedPoints as point, pointIndex (`${point.label}:${pointIndex}`)}
				<circle
					cx={mapX(point.x)}
					cy={mapY(point.y)}
					r={point.size}
					fill={point.color}
					fill-opacity="0.72"
				>
					<title>{point.label}: ({point.x.toFixed(2)}, {point.y.toFixed(2)})</title>
				</circle>
			{/each}
		</svg>
	{/if}
</div>

<style>
	.muij-scatter-chart {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-scatter-svg {
		width: 100%;
		height: auto;
	}

	.muij-scatter-axis {
		stroke: var(--border-soft);
		stroke-width: 1;
	}

	.muij-scatter-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}
</style>
