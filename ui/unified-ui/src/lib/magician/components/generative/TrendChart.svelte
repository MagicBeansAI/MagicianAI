<script lang="ts">
	import { curveMonotoneX, extent, line as d3Line, max, min, scaleLinear, scaleTime } from 'd3';
	import { sanitizeCssValue } from './cssUtil';

	const PADDING = { top: 16, right: 14, bottom: 24, left: 34 };
	const FALLBACK_TS_BASE_MS = Date.UTC(2000, 0, 1, 0, 0, 0, 0);

	interface TrendDatum {
		label: string;
		value: number;
	}

	export let data: TrendDatum[] = [];
	export let width: number = 420;
	export let height: number = 220;
	export let color: string = 'var(--theme-color-accent, var(--accent-primary))';

	interface TrendPoint {
		label: string;
		timestamp: Date;
		value: number;
	}

	let pathVersion = 0;
	let lastPathSignature = '';

	function parseTimestamp(rawLabel: string, index: number): Date {
		const parsedDate = Date.parse(rawLabel);
		if (Number.isFinite(parsedDate)) return new Date(parsedDate);

		const numeric = Number(rawLabel);
		if (Number.isFinite(numeric)) {
			const millis = numeric > 10_000_000_000 ? numeric : numeric * 1000;
			return new Date(millis);
		}

		return new Date(FALLBACK_TS_BASE_MS + index * 60_000);
	}

	function toFiniteNumber(value: unknown): number | undefined {
		if (typeof value !== 'number' || !Number.isFinite(value)) return undefined;
		return value;
	}

	$: safeWidth = Number.isFinite(width) && width >= 140 ? width : 420;
	$: safeHeight = Number.isFinite(height) && height >= 120 ? height : 220;
	$: safeColor = sanitizeCssValue(color).trim() || 'var(--theme-color-accent, var(--accent-primary))';
	$: plotWidth = safeWidth - PADDING.left - PADDING.right;
	$: plotHeight = safeHeight - PADDING.top - PADDING.bottom;

	$: points = data
		.map((entry, index) => {
			const value = toFiniteNumber(entry?.value);
			if (value === undefined) return null;
			const label = typeof entry?.label === 'string' && entry.label.trim().length > 0
				? entry.label.trim()
				: `${index + 1}`;
			return {
				label,
				value,
				timestamp: parseTimestamp(label, index)
			};
		})
		.filter((entry): entry is TrendPoint => entry !== null)
		.sort((left, right) => left.timestamp.getTime() - right.timestamp.getTime());

	$: xExtent = extent(points, (point) => point.timestamp);
	$: rawMinY = min(points, (point) => point.value) ?? 0;
	$: rawMaxY = max(points, (point) => point.value) ?? 1;
	$: yMin = rawMinY === rawMaxY ? rawMinY - 1 : rawMinY;
	$: yMax = rawMinY === rawMaxY ? rawMaxY + 1 : rawMaxY;

	$: xScale = scaleTime().range([PADDING.left, safeWidth - PADDING.right]);
	$: {
		if (points.length === 0) {
			xScale.domain([new Date(0), new Date(1)]);
		} else if (points.length === 1) {
			const ts = points[0].timestamp.getTime();
			xScale.domain([new Date(ts - 60_000), new Date(ts + 60_000)]);
		} else if (xExtent[0] && xExtent[1]) {
			xScale.domain([xExtent[0], xExtent[1]]);
		}
	}

	$: yScale = scaleLinear()
		.range([safeHeight - PADDING.bottom, PADDING.top])
		.domain([yMin, yMax])
		.nice();

	$: lineGenerator = d3Line<TrendPoint>()
		.x((point) => xScale(point.timestamp))
		.y((point) => yScale(point.value))
		.curve(curveMonotoneX);

	$: path = points.length > 0 ? (lineGenerator(points) ?? '') : '';

	$: yTicks = Array.from({ length: 4 }, (_, index) => {
		const ratio = index / 3;
		const y = PADDING.top + ratio * plotHeight;
		const value = yMax - ratio * (yMax - yMin);
		return { y, value };
	});

	$: xLeftLabel = points[0]?.label ?? '';
	$: xRightLabel = points[points.length - 1]?.label ?? '';

	$: {
		const signature = points.map((point) => `${point.timestamp.getTime()}:${point.value}`).join('|');
		if (signature !== lastPathSignature) {
			lastPathSignature = signature;
			pathVersion += 1;
		}
	}
</script>

<div class="muij-trend-chart">
	{#if points.length === 0}
		<div class="muij-trend-empty">No data</div>
	{:else}
		<svg class="muij-trend-svg" viewBox={`0 0 ${safeWidth} ${safeHeight}`} role="img" aria-label="Trend chart">
			{#each yTicks as tick (tick.y)}
				<line class="muij-trend-grid" x1={PADDING.left} y1={tick.y} x2={safeWidth - PADDING.right} y2={tick.y}></line>
				<text class="muij-trend-axis-text" x={PADDING.left - 6} y={tick.y + 4} text-anchor="end">
					{tick.value.toFixed(1)}
				</text>
			{/each}

			{#key pathVersion}
					<path
						class="muij-trend-path"
						d={path}
						fill="none"
						stroke={safeColor}
						stroke-width="2.35"
						stroke-linecap="round"
						stroke-linejoin="round"
				/>
			{/key}

			<text class="muij-trend-axis-text" x={PADDING.left} y={safeHeight - 4} text-anchor="start">
				{xLeftLabel}
			</text>
			<text class="muij-trend-axis-text" x={safeWidth - PADDING.right} y={safeHeight - 4} text-anchor="end">
				{xRightLabel}
			</text>
		</svg>
	{/if}
</div>

<style>
	.muij-trend-chart {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-trend-svg {
		width: 100%;
		height: auto;
	}

	.muij-trend-grid {
		stroke: var(--border-soft);
		stroke-width: 1;
	}

	.muij-trend-axis-text {
		fill: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 10px;
	}

	.muij-trend-path {
		stroke-dasharray: 900;
		stroke-dashoffset: 900;
		animation: muij-trend-draw 700ms ease forwards;
	}

	.muij-trend-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}

	@keyframes muij-trend-draw {
		to {
			stroke-dashoffset: 0;
		}
	}
</style>
