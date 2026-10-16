<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { sanitizeCssValue } from './cssUtil';
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
		'var(--theme-chart-color-3, #ef4444)',
		'var(--theme-chart-color-4, #8b5cf6)',
		'var(--theme-chart-color-5, #06b6d4)',
		'var(--theme-chart-color-6, #84cc16)',
		'var(--theme-chart-color-7, #f97316)'
	];

	interface PieSegment {
		label: string;
		value: number;
		color?: string;
	}

	export let segments: PieSegment[] = [];
	export let size: number = 180;
	export let innerRatio: number = 0.58;
	export let showLegend: boolean = true;
	export let showLegendValues: boolean = false;
	export let showTotal: boolean = false;
	export let totalLabel: string = 'Total';
	export let formatValue: (value: number) => string = defaultFormatValue;

	/**
	 * Live data binding — sets `segments` from a SQL fetch on mount and on
	 * `magician:dashboard-refresh`. labelField maps to segment label,
	 * valueField to segment value.
	 */
	export let dataSource: LiveDataSource | null = null;
	export let labelField: string = 'label';
	export let valueField: string = 'value';

	let liveError: string | null = null;
	let liveRefreshing: boolean = false;
	let activeTooltip: ChartTooltip | null = null;

	let liveMounted = false;

	const liveRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				liveRefreshing = true;
			},
			onRows: ({ records }) => {
				segments = records.map((r) => ({
					label: String(r[labelField] ?? ''),
					value: Number(r[valueField] ?? 0)
				}));
				liveError = null;
			},
			onError: (err) => {
				liveError = err.message;
			},
			onSettled: () => {
				liveRefreshing = false;
			}
		})
	);

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

	function defaultFormatValue(value: number): string {
		return value.toLocaleString('en-US');
	}

	function percentLabel(percent: number): string {
		const pct = percent * 100;
		if (pct > 0 && pct < 1) return '<1%';
		if (pct < 10) return `${pct.toFixed(1).replace(/\.0$/, '')}%`;
		return `${Math.round(pct)}%`;
	}

	interface ChartTooltip {
		label: string;
		valueLabel: string;
		percentLabel: string;
		x: number;
		y: number;
	}

	interface RenderSegment {
		label: string;
		value: number;
		color: string;
		startAngle: number;
		endAngle: number;
		percent: number;
		path: string;
	}

	function tooltipPoint(event: MouseEvent): { x: number; y: number } {
		const target = event.currentTarget;
		if (!(target instanceof Element)) return { x: 0, y: 0 };
		const root = target.closest('.muij-pie-chart');
		if (!(root instanceof HTMLElement)) return { x: 0, y: 0 };
		const rect = root.getBoundingClientRect();
		const pad = 86;
		const maxX = Math.max(pad, rect.width - pad);
		return {
			x: Math.min(maxX, Math.max(pad, event.clientX - rect.left)),
			y: Math.max(36, event.clientY - rect.top)
		};
	}

	function showSegmentTooltip(segment: RenderSegment, event: MouseEvent): void {
		const point = tooltipPoint(event);
		activeTooltip = {
			label: segment.label,
			valueLabel: formatValue(segment.value),
			percentLabel: percentLabel(segment.percent),
			x: point.x,
			y: point.y
		};
	}

	function moveSegmentTooltip(event: MouseEvent): void {
		if (!activeTooltip) return;
		activeTooltip = { ...activeTooltip, ...tooltipPoint(event) };
	}

	function clearSegmentTooltip(): void {
		activeTooltip = null;
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

	function polarToCartesian(radius: number, angleDeg: number): { x: number; y: number } {
		const radians = (angleDeg - 90) * (Math.PI / 180);
		return {
			x: radius * Math.cos(radians),
			y: radius * Math.sin(radians)
		};
	}

	function arcPath(startAngle: number, endAngle: number, outerRadius: number, innerRadiusPx: number): string {
		const sweep = Math.max(0, Math.min(359.999, endAngle - startAngle));
		if (sweep <= 0) return '';

		const startOuter = polarToCartesian(outerRadius, endAngle);
		const endOuter = polarToCartesian(outerRadius, startAngle);
		const largeArc = sweep > 180 ? 1 : 0;

		if (innerRadiusPx <= 0) {
			return [
				`M ${startOuter.x} ${startOuter.y}`,
				`A ${outerRadius} ${outerRadius} 0 ${largeArc} 0 ${endOuter.x} ${endOuter.y}`,
				'L 0 0',
				'Z'
			].join(' ');
		}

		const startInner = polarToCartesian(innerRadiusPx, startAngle);
		const endInner = polarToCartesian(innerRadiusPx, endAngle);

		return [
			`M ${startOuter.x} ${startOuter.y}`,
			`A ${outerRadius} ${outerRadius} 0 ${largeArc} 0 ${endOuter.x} ${endOuter.y}`,
			`L ${startInner.x} ${startInner.y}`,
			`A ${innerRadiusPx} ${innerRadiusPx} 0 ${largeArc} 1 ${endInner.x} ${endInner.y}`,
			'Z'
		].join(' ');
	}

	$: safeSize = Number.isFinite(size) && size >= 96 ? size : 180;
	$: outerRadius = Math.max(24, safeSize / 2 - 8);
	$: safeInnerRatio = Number.isFinite(innerRatio) ? Math.min(0.9, Math.max(0, innerRatio)) : 0.58;
	$: innerRadiusPx = outerRadius * safeInnerRatio;

	$: normalized = segments
		.map((segment, index) => {
			const value = toFiniteNumber(segment?.value);
			if (value === undefined || value <= 0) return null;
			const label = typeof segment?.label === 'string' && segment.label.trim().length > 0
				? segment.label.trim()
				: `Item ${index + 1}`;
			return {
				label,
				value,
				color: normalizeColor(segment?.color) || DEFAULT_COLORS[index % DEFAULT_COLORS.length]
			};
		})
		.filter((segment): segment is { label: string; value: number; color: string } => segment !== null);

	$: total = normalized.reduce((sum, segment) => sum + segment.value, 0);
	$: renderSegments = (() => {
		if (total <= 0) return [] as RenderSegment[];
		let cursor = 0;
		return normalized.map((segment) => {
			const startAngle = cursor;
			const endAngle = cursor + (segment.value / total) * 360;
			cursor = endAngle;
			const percent = segment.value / total;
			return {
				label: segment.label,
				value: segment.value,
				color: segment.color,
				startAngle,
				endAngle,
				percent,
				path: arcPath(startAngle, endAngle, outerRadius, innerRadiusPx)
			};
		});
	})();
</script>

<div class="muij-pie-chart" class:muij-pie-chart-live={!!dataSource}>
	{#if dataSource}
		<LiveDataRefreshButton
			loading={liveRefreshing}
			label="Refetch pie chart data"
			on:refresh={refreshLiveData}
		/>
	{/if}
	{#if liveError}
		<div class="muij-pie-error" role="alert">⚠ {liveError}</div>
	{:else if renderSegments.length === 0}
		<div class="muij-pie-empty">No data</div>
	{:else}
		<div class="muij-pie-body">
			<svg
				class="muij-pie-svg"
				viewBox={`0 0 ${safeSize} ${safeSize}`}
				role="img"
				aria-label="Pie chart"
			>
			<g transform={`translate(${safeSize / 2} ${safeSize / 2})`}>
				{#each renderSegments as segment, segmentIndex (`${segment.label}:${segment.startAngle}:${segmentIndex}`)}
					<path
						class="muij-pie-segment"
						role="graphics-symbol"
						d={segment.path}
						fill={segment.color}
						on:mouseenter={(event) => showSegmentTooltip(segment, event)}
						on:mousemove={moveSegmentTooltip}
						on:mouseleave={clearSegmentTooltip}
					>
						<title>{segment.label}: {formatValue(segment.value)} ({percentLabel(segment.percent)})</title>
					</path>
				{/each}
			</g>
			</svg>

			{#if showLegend}
				<div class="muij-pie-legend">
					{#if showTotal}
						<div class="muij-pie-legend-total">
							<span>{totalLabel}</span>
							<strong>{formatValue(total)}</strong>
						</div>
					{/if}
					{#each renderSegments as segment, segmentIndex (`${segment.label}:${segmentIndex}`)}
						<div
							class="muij-pie-legend-item"
							role="graphics-symbol"
							title={`${segment.label}: ${formatValue(segment.value)} (${percentLabel(segment.percent)})`}
							on:mouseenter={(event) => showSegmentTooltip(segment, event)}
							on:mousemove={moveSegmentTooltip}
							on:mouseleave={clearSegmentTooltip}
						>
							<span class="muij-pie-dot" style={`background:${segment.color}`}></span>
							<span class="muij-pie-label">{segment.label}</span>
							<span class="muij-pie-value">
								{#if showLegendValues}
									{formatValue(segment.value)} · {percentLabel(segment.percent)}
								{:else}
									{percentLabel(segment.percent)}
								{/if}
							</span>
						</div>
					{/each}
				</div>
			{/if}
		</div>
		{#if activeTooltip}
			<div
				class="muij-pie-tooltip"
				role="tooltip"
				style={`left:${activeTooltip.x}px;top:${activeTooltip.y}px;`}
			>
				<strong>{activeTooltip.label}</strong>
				<span>{activeTooltip.valueLabel} · {activeTooltip.percentLabel}</span>
			</div>
		{/if}
	{/if}
</div>

<style>
	.muij-pie-chart {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-pie-chart-live {
		padding-top: 30px;
	}

	.muij-pie-error {
		font-family: var(--theme-font-mono, var(--font-primary), monospace);
		font-size: 0.85rem;
		color: var(--theme-color-accent, var(--text-secondary));
		padding: 10px 14px;
		border: 1px solid var(--theme-color-accent, var(--border-light));
		border-radius: 6px;
		background-color: var(--theme-color-surface);
	}

	.muij-pie-body {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-md);
		align-items: center;
	}

	.muij-pie-svg {
		width: min(220px, 100%);
		height: auto;
		flex: 0 0 auto;
	}

	.muij-pie-segment {
		cursor: default;
		transition:
			opacity 0.12s ease,
			filter 0.12s ease;
	}

	.muij-pie-segment:hover {
		filter: brightness(1.08);
		opacity: 0.92;
	}

	.muij-pie-legend {
		display: grid;
		gap: 6px;
		min-width: 150px;
		flex: 1 1 180px;
	}

	.muij-pie-legend-total {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		padding-bottom: 6px;
		margin-bottom: 2px;
		border-bottom: 1px solid var(--border-soft, var(--border-light));
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	.muij-pie-legend-total strong {
		color: var(--text-primary, var(--text-body));
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.muij-pie-legend-item {
		display: grid;
		grid-template-columns: 10px minmax(0, 1fr) auto;
		gap: 8px;
		align-items: center;
		font-family: var(--font-primary);
		font-size: 0.75rem;
	}

	.muij-pie-legend-item:hover {
		color: var(--text-primary, var(--text-body));
	}

	.muij-pie-dot {
		width: 10px;
		height: 10px;
		border-radius: 999px;
	}

	.muij-pie-label {
		color: var(--text-body);
		overflow-wrap: anywhere;
	}

	.muij-pie-value {
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.muij-pie-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}

	.muij-pie-tooltip {
		position: absolute;
		z-index: 20;
		display: grid;
		gap: 0.12rem;
		min-width: 8.5rem;
		max-width: min(18rem, calc(100vw - 2rem));
		padding: 0.45rem 0.58rem;
		border: 1px solid color-mix(in srgb, var(--border-soft, var(--border-light)) 65%, transparent);
		border-radius: 8px;
		background: color-mix(in srgb, var(--bg-card, var(--theme-color-surface)) 92%, var(--text-primary, #111827) 8%);
		box-shadow: 0 8px 24px color-mix(in srgb, #000 18%, transparent);
		color: var(--text-primary, var(--text-body));
		font-family: var(--font-primary);
		font-size: 0.76rem;
		font-variant-numeric: tabular-nums;
		line-height: 1.25;
		pointer-events: none;
		transform: translate(-50%, calc(-100% - 0.65rem));
		white-space: nowrap;
	}

	.muij-pie-tooltip span {
		color: var(--text-secondary);
	}
</style>
