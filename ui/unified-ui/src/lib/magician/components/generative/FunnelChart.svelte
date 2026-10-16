<script lang="ts">
	import { format as d3Format, scaleLinear } from 'd3';

	interface FunnelDatum {
		key?: string;
		label: string;
		value: number;
		color?: string;
	}

	interface RenderDatum extends FunnelDatum {
		value: number;
		color: string;
		points: string;
		shareLabel: string;
		valueLabel: string;
	}

	const CHART_UNITS = 1000;
	const BAND_HEIGHT = 34;
	const BAND_Y_TOP = 4;
	const BAND_Y_BOTTOM = 30;
	const MIN_VISIBLE_UNITS = 6;
	const DEFAULT_PALETTE = [
		'var(--accent-primary)',
		'var(--observe-blue, var(--accent-primary))',
		'var(--observe-green, var(--color-success))',
		'var(--color-warning)',
		'var(--text-muted)'
	];

	export let data: FunnelDatum[] = [];
	export let total: number | undefined = undefined;
	export let ariaLabel = 'Funnel chart';
	export let sequential = true;
	export let formatValue: (value: number) => string = defaultFormatValue;

	function finiteNumber(value: unknown): number | undefined {
		const n = Number(value);
		return Number.isFinite(n) ? n : undefined;
	}

	function defaultFormatValue(value: number): string {
		return d3Format('.3~s')(value).replace('G', 'B');
	}

	function shareLabel(value: number, denominator: number): string {
		if (denominator <= 0 || value <= 0) return '0%';
		const pct = (value / denominator) * 100;
		if (pct < 1) return '<1%';
		if (pct < 10) return `${pct.toFixed(1).replace(/\.0$/, '')}%`;
		return `${Math.round(pct)}%`;
	}

	function visibleWidth(value: number, scale: (value: number) => number): number {
		if (value <= 0) return 0;
		return Math.max(MIN_VISIBLE_UNITS, scale(value));
	}

	function bandPoints(topWidth: number, bottomWidth: number): string {
		const topLeft = (CHART_UNITS - topWidth) / 2;
		const topRight = topLeft + topWidth;
		const bottomLeft = (CHART_UNITS - bottomWidth) / 2;
		const bottomRight = bottomLeft + bottomWidth;
		return `${topLeft},${BAND_Y_TOP} ${topRight},${BAND_Y_TOP} ${bottomRight},${BAND_Y_BOTTOM} ${bottomLeft},${BAND_Y_BOTTOM}`;
	}

	function tooltipLabel(row: RenderDatum): string {
		return `${row.label}: ${row.valueLabel} · ${row.shareLabel}`;
	}

	$: normalized = data
		.map((entry, index) => {
			const value = finiteNumber(entry.value);
			if (value === undefined) return null;
			const label = entry.label?.trim() || `Stage ${index + 1}`;
			return {
				...entry,
				label,
				value: Math.max(0, value),
				color: entry.color || DEFAULT_PALETTE[index % DEFAULT_PALETTE.length]
			};
		})
		.filter((entry): entry is FunnelDatum & { value: number; color: string } => entry !== null);

	$: safeTotal = (() => {
		const explicit = finiteNumber(total);
		if (explicit !== undefined && explicit > 0) return explicit;
		const inferred = Math.max(...normalized.map((entry) => entry.value), 0);
		return inferred > 0 ? inferred : 1;
	})();

	$: valueScale = scaleLinear().domain([0, safeTotal]).range([0, CHART_UNITS]).clamp(true);

	$: renderData = normalized.map((entry, index): RenderDatum => {
		const next = normalized[index + 1];
		const topWidth = visibleWidth(entry.value, valueScale);
		const bottomWidth =
			sequential && next ? visibleWidth(next.value, valueScale) : visibleWidth(entry.value, valueScale);
		return {
			...entry,
			points: bandPoints(topWidth, bottomWidth),
			shareLabel: shareLabel(entry.value, safeTotal),
			valueLabel: formatValue(entry.value)
		};
	});
</script>

<div class="funnel-chart">
	{#if renderData.length === 0}
		<div class="funnel-empty">No data</div>
	{:else}
		<div class="funnel-rows" role="list" aria-label={ariaLabel}>
			{#each renderData as row, index (`${row.key ?? row.label}:${index}`)}
				<div class="funnel-row" role="listitem" title={tooltipLabel(row)} aria-label={tooltipLabel(row)}>
					<div class="funnel-label" title={row.label}>{row.label}</div>
					<svg
						class="funnel-band-svg"
						viewBox={`0 0 ${CHART_UNITS} ${BAND_HEIGHT}`}
						preserveAspectRatio="none"
						aria-hidden="true"
					>
						<line class="funnel-guide" x1="0" y1={BAND_HEIGHT / 2} x2={CHART_UNITS} y2={BAND_HEIGHT / 2} />
						<polygon class="funnel-band" points={row.points} style={`--band-color: ${row.color};`} />
					</svg>
					<div class="funnel-value">
						<strong>{row.valueLabel}</strong>
						<span>{row.shareLabel}</span>
					</div>
					<div class="funnel-tooltip" role="tooltip">
						<strong>{row.label}</strong>
						<span>{row.valueLabel} · {row.shareLabel}</span>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.funnel-chart {
		min-width: 0;
		margin: 0.8rem 0 0.95rem;
	}

	.funnel-empty {
		font-size: 0.82rem;
		color: var(--text-muted);
	}

	.funnel-rows {
		display: grid;
		gap: 0.42rem;
	}

	.funnel-row {
		position: relative;
		display: grid;
		grid-template-columns: minmax(7rem, 9.5rem) minmax(8rem, 1fr) minmax(4.5rem, max-content);
		align-items: center;
		gap: 0.75rem;
		min-width: 0;
	}

	.funnel-label {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.82rem;
		font-weight: 600;
		color: var(--text-secondary);
	}

	.funnel-band-svg {
		display: block;
		width: 100%;
		height: 2.15rem;
		overflow: visible;
	}

	.funnel-guide {
		stroke: var(--border-soft);
		stroke-width: 1;
		vector-effect: non-scaling-stroke;
	}

	.funnel-band {
		fill: var(--band-color);
		fill-opacity: 0.22;
		stroke: var(--band-color);
		stroke-opacity: 0.82;
		stroke-width: 1.6;
		vector-effect: non-scaling-stroke;
		transition:
			points 0.24s ease,
			fill-opacity 0.16s ease;
	}

	.funnel-row:hover .funnel-band {
		fill-opacity: 0.3;
	}

	.funnel-tooltip {
		position: absolute;
		z-index: 20;
		left: 50%;
		top: 0;
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
		opacity: 0;
		pointer-events: none;
		transform: translate(-50%, calc(-100% - 0.5rem));
		transition:
			opacity 0.12s ease,
			transform 0.12s ease;
		white-space: nowrap;
	}

	.funnel-tooltip span {
		color: var(--text-secondary);
	}

	.funnel-row:hover .funnel-tooltip {
		opacity: 1;
		transform: translate(-50%, calc(-100% - 0.7rem));
	}

	.funnel-value {
		display: grid;
		justify-items: end;
		gap: 0.05rem;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.funnel-value strong {
		font-size: 0.82rem;
		line-height: 1.1;
		color: var(--text-primary);
	}

	.funnel-value span {
		font-size: 0.72rem;
		line-height: 1.1;
		color: var(--text-muted);
	}

	@media (max-width: 640px) {
		.funnel-row {
			grid-template-columns: minmax(5.5rem, 6.5rem) minmax(5rem, 1fr) max-content;
			gap: 0.5rem;
		}

		.funnel-label,
		.funnel-value strong {
			font-size: 0.76rem;
		}

		.funnel-value span {
			display: none;
		}
	}
</style>
