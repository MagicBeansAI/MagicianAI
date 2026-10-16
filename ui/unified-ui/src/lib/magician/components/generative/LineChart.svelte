<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { sanitizeCssValue } from './cssUtil';
	import { isRetro16Bit } from '$lib/shared/stores/themeStore';
	import {
		formatCompactChartNumber,
		formatSignedCompactChartDelta,
		generateAsciiLineChart,
		referenceBaseForChartAxis,
		parseNumericDuckDbValue
	} from './chartUtil';
	import {
		createLiveRewirer,
		setupLiveDataSource,
		type LiveDataSource
	} from '$lib/magician/dashboard/useLiveDataSource';
	import LiveDataRefreshButton from './LiveDataRefreshButton.svelte';

	const DEFAULT_COLORS = [
		'var(--theme-chart-color-0, #3b82f6)',
		'var(--theme-chart-color-1, #10b981)',
		'var(--theme-chart-color-2, #f59e0b)',
		'var(--theme-chart-color-3, #8b5cf6)'
	];
	const PADDING = { top: 24, right: 14, bottom: 24, left: 52 };

	interface LinePoint {
		x: number;
		y: number;
	}

	interface LineSeries {
		name?: string;
		color?: string;
		points: LinePoint[];
	}

	export let series: LineSeries[] = [];
	export let points: LinePoint[] = [];
	export let width: number = 360;
	export let height: number = 220;
	export let showLegend: boolean = true;

	/**
	 * Live data binding — sets `series` from a SQL fetch on mount and on
	 * `magician:dashboard-refresh`. Row → point mapping uses xField/yField;
	 * optional seriesField groups rows into named series.
	 */
	export let dataSource: LiveDataSource | null = null;
	export let xField: string = 'x';
	export let yField: string = 'y';
	export let seriesField: string | null = null;

	let liveLoading: boolean = false;
	let liveError: string | null = null;
	let liveRefreshing: boolean = false;

	function recordsToSeries(
		records: Array<Record<string, unknown>>
	): LineSeries[] {
		if (seriesField) {
			const buckets = new Map<string, LinePoint[]>();
			for (const r of records) {
				const name = String(r[seriesField] ?? '');
				const point: LinePoint = {
					x: Number(r[xField] ?? 0),
					y: Number(r[yField] ?? 0)
				};
				if (!buckets.has(name)) buckets.set(name, []);
				buckets.get(name)!.push(point);
			}
			return Array.from(buckets.entries()).map(([name, pts]) => ({ name, points: pts }));
		}
		const pts: LinePoint[] = records.map((r) => ({
			x: Number(r[xField] ?? 0),
			y: Number(r[yField] ?? 0)
		}));
		return [{ points: pts }];
	}

	let liveMounted = false;

	const liveRewirer = createLiveRewirer((source) => {
		liveLoading = true;
		return setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				liveLoading = true;
				liveRefreshing = true;
			},
			onRows: ({ records }) => {
				series = recordsToSeries(records);
				liveError = null;
			},
			onError: (err) => {
				liveError = err.message;
				liveLoading = false;
			},
			onSettled: () => {
				liveLoading = false;
				liveRefreshing = false;
			}
		});
	});

	onMount(() => {
		liveMounted = true;
	});

	// Initial wiring AND rewiring when the bound query changes: filter-driven
	// pages rebuild `dataSource.sql` reactively, and a controller frozen at
	// mount would keep re-fetching the original query on every refresh
	// event. Static dataSource props never retrigger this (key comparison
	// inside the rewirer).
	$: if (liveMounted) liveRewirer.sync(dataSource);

	onDestroy(() => {
		liveRewirer.destroy();
	});

	function refreshLiveData(): void {
		if (!liveRewirer.controller || liveRefreshing) return;
		void liveRewirer.controller.refresh();
	}

	interface RenderSeries {
		name: string;
		color: string;
		points: LinePoint[];
		path: string;
	}

	function asRecord(value: unknown): Record<string, unknown> | null {
		return value != null && typeof value === 'object' && !Array.isArray(value)
			? (value as Record<string, unknown>)
			: null;
	}

	function toFiniteNumber(value: unknown): number | undefined {
		return parseNumericDuckDbValue(value);
	}

	function normalizeColor(value: unknown): string | undefined {
		if (typeof value !== 'string') return undefined;
		const trimmed = sanitizeCssValue(value).trim();
		return trimmed.length > 0 ? trimmed : undefined;
	}

	function normalizePoints(value: unknown): LinePoint[] {
		if (!Array.isArray(value)) return [];
		const normalized: LinePoint[] = [];
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

	function linePath(pointsInput: LinePoint[], mapX: (x: number) => number, mapY: (y: number) => number): string {
		if (pointsInput.length === 0) return '';
		if (pointsInput.length === 1) {
			const p = pointsInput[0];
			const x = mapX(p.x);
			const y = mapY(p.y);
			return `M ${x} ${y} L ${x} ${y}`;
		}
		return pointsInput
			.map((point, index) => `${index === 0 ? 'M' : 'L'} ${mapX(point.x)} ${mapY(point.y)}`)
			.join(' ');
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
				.filter((entry): entry is { name: string; color: string; points: LinePoint[] } => entry !== null);
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

	$: renderSeries = normalizedSeries.map((entry): RenderSeries => ({
		name: entry.name,
		color: entry.color,
		points: entry.points,
		path: linePath(entry.points, mapX, mapY)
	}));

	$: yAxisBase = referenceBaseForChartAxis(domain.minY, domain.maxY);
	$: yAxisUsesBase = yAxisBase !== 0;

	function formatAxisTick(value: number): string {
		return yAxisUsesBase
			? formatSignedCompactChartDelta(value - yAxisBase)
			: formatCompactChartNumber(value);
	}

	$: yTicks = Array.from({ length: 4 }, (_, index) => {
		const ratio = index / 3;
		const y = PADDING.top + ratio * plotHeight;
		const value = domain.maxY - ratio * (domain.maxY - domain.minY);
		return { y, value };
	});

	$: xTicks = (() => {
		if (domain.maxX === domain.minX) return [];
		return Array.from({ length: 4 }, (_, index) => {
			const ratio = index / 3;
			const xValue = domain.minX + ratio * (domain.maxX - domain.minX);
			const xPos = PADDING.left + ratio * plotWidth;
			
			let labelStr = '';
			if (xValue > 1e11) {
				// Looks like a timestamp
				labelStr = new Date(xValue).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
			} else {
				labelStr = formatCompactChartNumber(xValue);
			}
			return { xPos, value: xValue, label: labelStr };
		});
	})();

	$: asciiChart = $isRetro16Bit ? generateAsciiLineChart(allPoints, 40, 10) : '';
</script>

<div class="muij-line-chart" class:muij-line-chart-live={!!dataSource} class:retro={$isRetro16Bit}>
	{#if dataSource}
		<LiveDataRefreshButton
			loading={liveRefreshing}
			label="Refetch line chart data"
			on:refresh={refreshLiveData}
		/>
	{/if}
	{#if liveError}
		<div class="muij-line-error" role="alert">⚠ {liveError}</div>
	{:else if liveLoading && renderSeries.length === 0}
		<div class="muij-line-empty">Loading…</div>
	{:else if renderSeries.length === 0}
		<div class="muij-line-empty">No data</div>
	{:else if $isRetro16Bit}
		<pre class="ascii-chart">{asciiChart}</pre>
	{:else}
		<svg class="muij-line-svg" viewBox={`0 0 ${safeWidth} ${safeHeight}`} role="img" aria-label="Line chart">
			{#if yAxisUsesBase}
				<text class="muij-line-axis-base" x={PADDING.left} y="12">
					Base {formatCompactChartNumber(yAxisBase)}
				</text>
			{/if}
			{#each yTicks as tick (tick.y)}
				<line class="muij-line-grid" x1={PADDING.left} y1={tick.y} x2={safeWidth - PADDING.right} y2={tick.y}></line>
				<text class="muij-line-axis-text" x={PADDING.left - 6} y={tick.y + 4} text-anchor="end">
					<title>{tick.value.toLocaleString('en-US')}</title>
					{formatAxisTick(tick.value)}
				</text>
			{/each}
			{#each xTicks as tick (tick.xPos)}
				<text class="muij-line-axis-text" x={tick.xPos} y={safeHeight - 6} text-anchor="middle">
					{tick.label}
				</text>
			{/each}
			{#each renderSeries as entry, entryIndex (`${entry.name}:${entryIndex}`)}
				<path d={entry.path} fill="none" stroke={entry.color} stroke-width="2.25" stroke-linecap="round">
					<title>{entry.name}</title>
				</path>
			{/each}
		</svg>

		{#if showLegend}
			<div class="muij-line-legend">
				{#each renderSeries as entry, entryIndex (`${entry.name}:${entryIndex}`)}
					<div class="muij-line-legend-item">
						<span class="muij-line-legend-dot" style={`background:${entry.color}`}></span>
						<span>{entry.name}</span>
					</div>
				{/each}
			</div>
		{/if}
	{/if}
</div>

<style>
	.muij-line-chart {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-line-chart-live {
		padding-top: 30px;
	}

	.muij-line-chart.retro {
		background: #000;
		color: #ffb000;
		padding: 1rem;
		border: 1px solid #ffb000;
	}

	.muij-line-chart-live.retro {
		padding-top: calc(1rem + 30px);
	}

	.muij-line-error {
		font-family: var(--theme-font-mono, var(--font-primary), monospace);
		font-size: 0.85rem;
		color: var(--theme-color-accent, var(--text-secondary));
		padding: 10px 14px;
		border: 1px solid var(--theme-color-accent, var(--border-light));
		border-radius: 6px;
		background-color: var(--theme-color-surface);
	}

	.ascii-chart {
		font-family: var(--font-mono);
		font-size: 11px;
		line-height: 1.2;
		white-space: pre;
		overflow-x: auto;
		color: #ffb000;
	}

	.muij-line-svg {
		width: 100%;
		height: auto;
		max-height: 280px;
	}

	.muij-line-grid {
		stroke: var(--border-soft);
		stroke-width: 1;
	}

	.muij-line-axis-text {
		fill: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 9px;
		font-variant-numeric: tabular-nums;
	}

	.muij-line-axis-base {
		fill: var(--text-tertiary, var(--text-secondary));
		font-family: var(--font-mono);
		font-size: 8px;
		font-variant-numeric: tabular-nums;
	}

	.muij-line-legend {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
	}

	.muij-line-legend-item {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-body);
	}

	.muij-line-legend-dot {
		width: 9px;
		height: 9px;
		border-radius: 999px;
	}

	.muij-line-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}
</style>
