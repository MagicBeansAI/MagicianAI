<script lang="ts">
	import { format as d3Format } from 'd3';

	interface StackSegment {
		key?: string;
		label: string;
		value: number;
		color?: string;
	}

	interface RenderSegment extends StackSegment {
		value: number;
		color: string;
		width: number;
		valueLabel: string;
		percentLabel: string;
	}

	interface ChartTooltip {
		label: string;
		valueLabel: string;
		percentLabel: string;
		x: number;
		y: number;
	}

	const DEFAULT_PALETTE = [
		'var(--accent-primary)',
		'var(--observe-green, var(--color-success))',
		'var(--observe-blue, var(--color-info))',
		'var(--color-warning)',
		'var(--text-muted)'
	];

	export let segments: StackSegment[] = [];
	export let total: number | undefined = undefined;
	export let ariaLabel = 'Stacked bar chart';
	export let showLegend = true;
	export let formatValue: (value: number) => string = defaultFormatValue;

	let activeTooltip: ChartTooltip | null = null;

	function finiteNumber(value: unknown): number | undefined {
		const n = Number(value);
		return Number.isFinite(n) ? n : undefined;
	}

	function defaultFormatValue(value: number): string {
		return d3Format('.3~s')(value).replace('G', 'B');
	}

	function percentLabel(value: number, denominator: number): string {
		if (denominator <= 0 || value <= 0) return '0%';
		const pct = (value / denominator) * 100;
		if (pct < 1) return '<1%';
		if (pct < 10) return `${pct.toFixed(1).replace(/\.0$/, '')}%`;
		return `${Math.round(pct)}%`;
	}

	function tooltipPoint(event: MouseEvent): { x: number; y: number } {
		const target = event.currentTarget;
		if (!(target instanceof Element)) return { x: 0, y: 0 };
		const root = target.closest('.stacked-chart');
		if (!(root instanceof HTMLElement)) return { x: 0, y: 0 };
		const rect = root.getBoundingClientRect();
		const pad = 86;
		const maxX = Math.max(pad, rect.width - pad);
		return {
			x: Math.min(maxX, Math.max(pad, event.clientX - rect.left)),
			y: Math.max(34, event.clientY - rect.top)
		};
	}

	function showTooltip(segment: RenderSegment, event: MouseEvent): void {
		const point = tooltipPoint(event);
		activeTooltip = {
			label: segment.label,
			valueLabel: segment.valueLabel,
			percentLabel: segment.percentLabel,
			x: point.x,
			y: point.y
		};
	}

	function moveTooltip(event: MouseEvent): void {
		if (!activeTooltip) return;
		activeTooltip = { ...activeTooltip, ...tooltipPoint(event) };
	}

	function clearTooltip(): void {
		activeTooltip = null;
	}

	$: normalized = segments
		.map((segment, index) => {
			const value = finiteNumber(segment.value);
			if (value === undefined) return null;
			const label = segment.label?.trim() || `Segment ${index + 1}`;
			return {
				...segment,
				label,
				value: Math.max(0, value),
				color: segment.color || DEFAULT_PALETTE[index % DEFAULT_PALETTE.length]
			};
		})
		.filter((segment): segment is StackSegment & { value: number; color: string } => segment !== null);

	$: safeTotal = (() => {
		const explicit = finiteNumber(total);
		if (explicit !== undefined && explicit > 0) return explicit;
		const inferred = normalized.reduce((sum, segment) => sum + segment.value, 0);
		return inferred > 0 ? inferred : 0;
	})();

	$: renderSegments = normalized.map((segment): RenderSegment => ({
		...segment,
		width: safeTotal > 0 ? Math.max(0, Math.min(100, (segment.value / safeTotal) * 100)) : 0,
		valueLabel: formatValue(segment.value),
		percentLabel: percentLabel(segment.value, safeTotal)
	}));

	$: hasData = renderSegments.some((segment) => segment.value > 0);
</script>

<div class="stacked-chart">
	{#if !hasData}
		<div class="stacked-empty">No data</div>
	{:else}
		<div class="stacked-track" role="img" aria-label={ariaLabel}>
			{#each renderSegments as segment, index (`${segment.key ?? segment.label}:${index}`)}
				<div
					class="stacked-segment"
					class:stacked-segment-empty={segment.value <= 0}
					role="graphics-symbol"
					style={`--segment-color: ${segment.color}; --segment-width: ${segment.width}%;`}
					title={`${segment.label}: ${segment.valueLabel} (${segment.percentLabel})`}
					on:mouseenter={(event) => showTooltip(segment, event)}
					on:mousemove={moveTooltip}
					on:mouseleave={clearTooltip}
				></div>
			{/each}
		</div>

		{#if showLegend}
			<div class="stacked-legend">
				{#each renderSegments as segment, index (`${segment.key ?? segment.label}:legend:${index}`)}
					<div
						class="stacked-legend-item"
						role="graphics-symbol"
						title={`${segment.label}: ${segment.valueLabel} (${segment.percentLabel})`}
						on:mouseenter={(event) => showTooltip(segment, event)}
						on:mousemove={moveTooltip}
						on:mouseleave={clearTooltip}
					>
						<span class="stacked-dot" style={`background:${segment.color}`}></span>
						<span class="stacked-label">{segment.label}</span>
						<span class="stacked-value">{segment.valueLabel} · {segment.percentLabel}</span>
					</div>
				{/each}
			</div>
		{/if}

		{#if activeTooltip}
			<div class="stacked-tooltip" role="tooltip" style={`left:${activeTooltip.x}px;top:${activeTooltip.y}px;`}>
				<strong>{activeTooltip.label}</strong>
				<span>{activeTooltip.valueLabel} · {activeTooltip.percentLabel}</span>
			</div>
		{/if}
	{/if}
</div>

<style>
	.stacked-chart {
		position: relative;
		display: grid;
		gap: 0.72rem;
		min-width: 0;
	}

	.stacked-empty {
		font-size: 0.78rem;
		color: var(--text-muted);
	}

	.stacked-track {
		display: flex;
		width: 100%;
		height: 1.2rem;
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		background: var(--bg-soft);
		overflow: hidden;
	}

	.stacked-segment {
		flex: 0 0 var(--segment-width);
		min-width: 2px;
		height: 100%;
		background: var(--segment-color);
		transition:
			filter 0.12s ease,
			opacity 0.12s ease;
	}

	.stacked-segment-empty {
		min-width: 0;
		pointer-events: none;
	}

	.stacked-segment:hover {
		filter: brightness(1.08);
		opacity: 0.92;
	}

	.stacked-legend {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(10.5rem, 1fr));
		gap: 0.42rem 0.75rem;
	}

	.stacked-legend-item {
		display: grid;
		grid-template-columns: 10px minmax(0, 1fr) auto;
		align-items: center;
		gap: 0.45rem;
		min-width: 0;
		font-size: 0.74rem;
		color: var(--text-secondary);
	}

	.stacked-dot {
		width: 10px;
		height: 10px;
		border-radius: 999px;
	}

	.stacked-label {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-primary);
	}

	.stacked-value {
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
		color: var(--text-muted);
	}

	.stacked-tooltip {
		position: absolute;
		z-index: 20;
		display: grid;
		gap: 0.12rem;
		min-width: 8.5rem;
		max-width: min(18rem, calc(100vw - 2rem));
		padding: 0.45rem 0.58rem;
		border: 1px solid color-mix(in srgb, var(--border-soft) 65%, transparent);
		border-radius: 8px;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--text-primary) 8%);
		box-shadow: 0 8px 24px color-mix(in srgb, #000 18%, transparent);
		color: var(--text-primary);
		font-size: 0.76rem;
		font-variant-numeric: tabular-nums;
		line-height: 1.25;
		pointer-events: none;
		transform: translate(-50%, calc(-100% - 0.65rem));
		white-space: nowrap;
	}

	.stacked-tooltip span {
		color: var(--text-secondary);
	}

	@media (max-width: 640px) {
		.stacked-legend {
			grid-template-columns: 1fr;
		}
	}
</style>
