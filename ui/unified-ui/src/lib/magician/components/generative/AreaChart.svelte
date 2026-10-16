<script lang="ts">
	import { sanitizeCssValue } from './cssUtil';

	const DEFAULT_COLORS = [
		'var(--theme-chart-color-0, #3b82f6)',
		'var(--theme-chart-color-1, #10b981)',
		'var(--theme-chart-color-2, #f59e0b)',
		'var(--theme-chart-color-3, #8b5cf6)'
	];
	const PADDING = { top: 16, right: 14, bottom: 24, left: 32 };

	interface AreaPoint {
		x: number;
		y: number;
	}

	interface AreaSeries {
		name?: string;
		color?: string;
		points: AreaPoint[];
	}

	export let series: AreaSeries[] = [];
	export let points: AreaPoint[] = [];
	export let width: number = 360;
	export let height: number = 220;

	interface RenderSeries {
		name: string;
		color: string;
		linePath: string;
		areaPath: string;
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

	function normalizePoints(value: unknown): AreaPoint[] {
		if (!Array.isArray(value)) return [];
		const normalized: AreaPoint[] = [];
		for (let i = 0; i < value.length; i++) {
			const item = value[i];
			if (typeof item === 'number' && Number.isFinite(item)) {
				normalized.push({ x: i, y: item });
				continue;
			}
			const rec = asRecord(item);
			if (!rec) continue;
			const x = toFiniteNumber(rec.x) ?? i;
			const y = toFiniteNumber(rec.y);
			if (y === undefined) continue;
			normalized.push({ x, y });
		}
		return normalized.sort((left, right) => left.x - right.x);
	}

	function linePath(pointsInput: AreaPoint[], mapX: (x: number) => number, mapY: (y: number) => number): string {
		return pointsInput
			.map((point, index) => `${index === 0 ? 'M' : 'L'} ${mapX(point.x)} ${mapY(point.y)}`)
			.join(' ');
	}

	function areaPath(
		pointsInput: AreaPoint[],
		mapX: (x: number) => number,
		mapY: (y: number) => number,
		baselineY: number
	): string {
		if (pointsInput.length === 0) return '';
		const line = linePath(pointsInput, mapX, mapY);
		const first = pointsInput[0];
		const last = pointsInput[pointsInput.length - 1];
		return `${line} L ${mapX(last.x)} ${baselineY} L ${mapX(first.x)} ${baselineY} Z`;
	}

	$: safeWidth = Number.isFinite(width) && width >= 120 ? width : 360;
	$: safeHeight = Number.isFinite(height) && height >= 120 ? height : 220;
	$: plotWidth = safeWidth - PADDING.left - PADDING.right;
	$: plotHeight = safeHeight - PADDING.top - PADDING.bottom;

	$: normalizedSeries = (() => {
		if (series.length > 0) {
			return series
				.map((entry, index) => {
					const normalizedPoints = normalizePoints(entry?.points);
					if (normalizedPoints.length === 0) return null;
					const name = typeof entry?.name === 'string' && entry.name.trim().length > 0
						? entry.name.trim()
						: `Series ${index + 1}`;
					return {
						name,
						color: normalizeColor(entry?.color) || DEFAULT_COLORS[index % DEFAULT_COLORS.length],
						points: normalizedPoints
					};
				})
				.filter((entry): entry is { name: string; color: string; points: AreaPoint[] } => entry !== null);
		}
		const fallbackPoints = normalizePoints(points);
		if (fallbackPoints.length === 0) return [];
		return [{ name: 'Series 1', color: DEFAULT_COLORS[0], points: fallbackPoints }];
	})();

	$: allPoints = normalizedSeries.flatMap((entry) => entry.points);
	$: domain = (() => {
		if (allPoints.length === 0) {
			return { minX: 0, maxX: 1, minY: 0, maxY: 1 };
		}
		let minX = Math.min(...allPoints.map((point) => point.x));
		let maxX = Math.max(...allPoints.map((point) => point.x));
		let minY = Math.min(...allPoints.map((point) => point.y));
		let maxY = Math.max(...allPoints.map((point) => point.y));
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

	function mapX(x: number): number {
		return PADDING.left + ((x - domain.minX) / (domain.maxX - domain.minX)) * plotWidth;
	}

	function mapY(y: number): number {
		return PADDING.top + (1 - (y - domain.minY) / (domain.maxY - domain.minY)) * plotHeight;
	}

	$: baselineValue = Math.max(Math.min(0, domain.maxY), domain.minY);
	$: baselineY = mapY(baselineValue);
	$: renderSeries = normalizedSeries.map((entry): RenderSeries => ({
		name: entry.name,
		color: entry.color,
		linePath: linePath(entry.points, mapX, mapY),
		areaPath: areaPath(entry.points, mapX, mapY, baselineY)
	}));
</script>

<div class="muij-area-chart">
	{#if renderSeries.length === 0}
		<div class="muij-area-empty">No data</div>
	{:else}
		<svg class="muij-area-svg" viewBox={`0 0 ${safeWidth} ${safeHeight}`} role="img" aria-label="Area chart">
			{#each renderSeries as entry, entryIndex (`${entry.name}:${entryIndex}`)}
				<path d={entry.areaPath} fill={entry.color} fill-opacity="0.22"></path>
				<path d={entry.linePath} fill="none" stroke={entry.color} stroke-width="2.1" stroke-linecap="round">
					<title>{entry.name}</title>
				</path>
			{/each}
			<line class="muij-area-baseline" x1={PADDING.left} y1={baselineY} x2={safeWidth - PADDING.right} y2={baselineY}></line>
		</svg>
	{/if}
</div>

<style>
	.muij-area-chart {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-area-svg {
		width: 100%;
		height: auto;
	}

	.muij-area-baseline {
		stroke: var(--border-soft);
		stroke-width: 1;
		stroke-dasharray: 4 4;
	}

	.muij-area-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}
</style>
