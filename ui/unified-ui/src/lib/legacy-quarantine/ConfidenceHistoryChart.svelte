<script lang="ts">
	interface ConfidenceAnnotation {
		timestamp: number;
		label: string;
		type?: 'clarification' | 'resume' | 'retry' | 'system';
	}

	export let history: Array<[number, number]> = [];
	export let annotations: ConfidenceAnnotation[] = [];
	export let width = 280;
	export let height = 160;
	export let compact = false;

	// Adjust padding based on compact mode
	$: PADDING = compact
		? { top: 2, right: 4, bottom: 12, left: 18 }
		: { top: 12, right: 16, bottom: 28, left: 44 };

	const yTicks = [0, 0.25, 0.5, 0.75, 1];

	const clamp = (value: number, min: number, max: number) =>
		Math.min(max, Math.max(min, value));

	// Normalise the history and clone into a two-point line if there is only one sample
	$: normalisedHistory = history
		.map(([ts, value]) => {
			const timestamp = typeof ts === 'number' ? ts : Date.parse(String(ts));
			const numericValue = typeof value === 'number' ? value : Number(value);
			return [timestamp, numericValue] as [number, number];
		})
		.filter(([ts, value]) => Number.isFinite(ts) && Number.isFinite(value))
		.map(([ts, value]) => [ts, clamp(value, 0, 1)] as [number, number])
		.sort((a, b) => a[0] - b[0]);

	$: chartHistory =
		normalisedHistory.length === 1
			? [
					normalisedHistory[0],
					[normalisedHistory[0][0] + 1000, normalisedHistory[0][1]] as [number, number]
			  ]
			: normalisedHistory;

	$: hasChart = chartHistory.length >= 2;

	$: minTime = hasChart ? chartHistory[0][0] : Date.now();
	$: maxTime = hasChart ? chartHistory[chartHistory.length - 1][0] : minTime + 1;
	$: timeSpan = Math.max(1, maxTime - minTime);

	const minValue = 0;
	const maxValue = 1;
	const valueSpan = maxValue - minValue || 1;

	$: innerWidth = width - PADDING.left - PADDING.right;
	$: innerHeight = height - PADDING.top - PADDING.bottom;

	const scaleX = (timestamp: number) => {
		if (!hasChart) return PADDING.left;
		return PADDING.left + ((timestamp - minTime) / timeSpan) * innerWidth;
	};

	const scaleY = (value: number) => {
		return PADDING.top + (1 - (value - minValue) / valueSpan) * innerHeight;
	};

	$: linePath = hasChart
		? chartHistory
				.map(([ts, value], index) => `${index === 0 ? 'M' : 'L'}${scaleX(ts).toFixed(2)},${scaleY(value).toFixed(2)}`)
				.join(' ')
		: '';

	$: areaPath = hasChart
		? `${chartHistory
				.map(([ts, value], index) => `${index === 0 ? 'M' : 'L'}${scaleX(ts).toFixed(2)},${scaleY(value).toFixed(2)}`)
				.join(' ')} L${scaleX(maxTime).toFixed(2)},${scaleY(minValue).toFixed(
				2
		  )} L${scaleX(minTime).toFixed(2)},${scaleY(minValue).toFixed(2)} Z`
		: '';

	$: annotationDots = annotations
		.filter((annotation) => Number.isFinite(annotation.timestamp))
		.map((annotation) => ({
			...annotation,
			x: scaleX(clamp(annotation.timestamp, minTime, maxTime)),
			y: PADDING.top + 6
		}));

	$: startLabel = hasChart
		? new Date(minTime).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
		: '';
	$: endLabel = hasChart
		? new Date(maxTime).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
		: '';
	$: lastValue = hasChart ? chartHistory[chartHistory.length - 1][1] : null;
	$: firstValue = hasChart ? chartHistory[0][1] : null;
	$: deltaLabel =
		lastValue !== null && firstValue !== null
			? `${lastValue >= firstValue ? '+' : ''}${((lastValue - firstValue) * 100).toFixed(1)}%`
			: null;
</script>

<div class="chart-container" class:compact>
	{#if !compact}
		<div class="chart-header">
			<div>
				<div class="chart-title">Confidence Trend</div>
				{#if deltaLabel}
					<div class="chart-delta">{deltaLabel}</div>
				{/if}
			</div>
			{#if lastValue !== null}
				<div class="chart-value">{(lastValue * 100).toFixed(1)}%</div>
			{/if}
		</div>
	{/if}

	{#if hasChart}
		<svg class="chart-canvas" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none">
			<!-- Grid lines - fewer in compact mode -->
			{#each compact ? [0, 1] : yTicks as tick}
				<line
					x1={PADDING.left}
					x2={width - PADDING.right}
					y1={scaleY(tick)}
					y2={scaleY(tick)}
					class="grid-line"
				/>
				<text x={PADDING.left - 4} y={scaleY(tick) + 3} class="grid-label" class:compact>{Math.round(tick * 100)}</text>
			{/each}

			<!-- Area under the curve -->
			<path d={areaPath} class="area-fill" />

			<!-- Confidence line -->
			<path d={linePath} class="line-path" class:compact />

			<!-- Data points - smaller in compact mode -->
			{#each chartHistory as [ts, value]}
				<circle cx={scaleX(ts)} cy={scaleY(value)} r={compact ? 1.5 : 2.5} class="line-point" />
			{/each}

			<!-- Annotations - skip in compact mode -->
			{#if !compact}
				{#each annotationDots as annotation}
					<g transform={`translate(${annotation.x}, ${annotation.y})`} class={`annotation annotation-${annotation.type ?? 'system'}`}>
						<circle r="4" />
						<text x="0" y="-8">{annotation.label}</text>
					</g>
				{/each}
			{/if}
		</svg>
		<div class="chart-footer" class:compact>
			<span>{startLabel}</span>
			<span>{endLabel}</span>
		</div>
	{:else}
		<div class="empty-state" class:compact>
			{compact ? 'No data' : 'Run a mission to see confidence changes over time.'}
		</div>
	{/if}
</div>

<style>
	.chart-container {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.chart-container.compact {
		gap: 0.25rem;
	}

	.chart-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
	}

	.chart-title {
		font-weight: 600;
		color: var(--text-primary, #0f172a);
		font-size: 0.95rem;
	}

	.chart-delta {
		font-size: 0.75rem;
		color: var(--color-success, #16a34a);
		font-weight: 600;
		margin-top: 0.1rem;
	}

	.chart-value {
		font-weight: 700;
		font-size: 1.1rem;
		color: var(--accent-primary, #1d4ed8);
	}

	.chart-canvas {
		width: 100%;
		height: auto;
	}

	.grid-line {
		stroke: var(--border-soft, rgba(148, 163, 184, 0.35));
		stroke-width: 1;
	}

	.grid-label {
		fill: var(--text-muted, #64748b);
		font-size: 0.65rem;
		text-anchor: end;
	}

	.grid-label.compact {
		font-size: 0.5rem;
	}

	.area-fill {
		fill: color-mix(in srgb, var(--accent-primary, #3b82f6) 12%, transparent);
	}

	.line-path {
		fill: none;
		stroke: var(--accent-primary, #2563eb);
		stroke-width: 2.25;
	}

	.line-path.compact {
		stroke-width: 1.5;
	}

	.line-point {
		fill: var(--accent-primary, #2563eb);
		stroke: var(--bg-elevated, #ffffff);
		stroke-width: 1.5;
	}

	.annotation circle {
		fill: var(--color-warning, #f59e0b);
	}

	.annotation text {
		fill: var(--color-warning, #f59e0b);
		font-size: 0.6rem;
		text-anchor: middle;
		font-weight: 600;
	}

	.annotation-clarification circle,
	.annotation-clarification text {
		fill: var(--color-warning, #f97316);
		color: var(--color-warning, #f97316);
	}

	.annotation-resume circle,
	.annotation-resume text {
		fill: var(--color-success, #10b981);
		color: var(--color-success, #10b981);
	}

	.annotation-retry circle,
	.annotation-retry text {
		fill: var(--accent-plum, #6366f1);
		color: var(--accent-plum, #6366f1);
	}

	.chart-footer {
		display: flex;
		justify-content: space-between;
		font-size: 0.7rem;
		color: var(--text-muted, #64748b);
	}

	.chart-footer.compact {
		font-size: 0.55rem;
	}

	.empty-state {
		background: var(--bg-surface, #f8fafc);
		border: 1px dashed var(--border-soft, #cbd5f5);
		border-radius: 8px;
		padding: 1rem;
		color: var(--text-muted, #64748b);
		font-size: 0.85rem;
		text-align: center;
	}

	.empty-state.compact {
		padding: 0.5rem;
		font-size: 0.65rem;
		border-radius: 4px;
	}

	/* ── Retro 16-bit Dark Theme ── */
	:global([data-theme="retro-16bit"]) .chart-container {
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit"]) .chart-title {
		color: #ffb000;
	}

	:global([data-theme="retro-16bit"]) .chart-delta {
		color: #00ff41;
	}

	:global([data-theme="retro-16bit"]) .chart-value {
		color: #ffb000;
	}

	:global([data-theme="retro-16bit"]) .grid-line {
		stroke: #333;
	}

	:global([data-theme="retro-16bit"]) .grid-label {
		fill: #805800;
	}

	:global([data-theme="retro-16bit"]) .area-fill {
		fill: rgba(255, 176, 0, 0.1);
	}

	:global([data-theme="retro-16bit"]) .line-path {
		stroke: #ffb000;
	}

	:global([data-theme="retro-16bit"]) .line-point {
		fill: #ffb000;
		stroke: #000;
	}

	:global([data-theme="retro-16bit"]) .annotation circle {
		fill: #ff6600;
	}

	:global([data-theme="retro-16bit"]) .annotation text {
		fill: #ff6600;
	}

	:global([data-theme="retro-16bit"]) .annotation-resume circle,
	:global([data-theme="retro-16bit"]) .annotation-resume text {
		fill: #00ff41;
		color: #00ff41;
	}

	:global([data-theme="retro-16bit"]) .annotation-retry circle,
	:global([data-theme="retro-16bit"]) .annotation-retry text {
		fill: #00aaff;
		color: #00aaff;
	}

	:global([data-theme="retro-16bit"]) .chart-footer {
		color: #805800;
	}

	:global([data-theme="retro-16bit"]) .empty-state {
		background: #111;
		border-color: #333;
		border-radius: 0;
		color: #805800;
	}

	:global([data-theme="retro-16bit"]) .empty-state.compact {
		border-radius: 0;
	}

	/* ── Retro 16-bit Light Theme ── */
	:global([data-theme="retro-16bit-light"]) .chart-container {
		font-family: var(--font-mono);
	}

	:global([data-theme="retro-16bit-light"]) .chart-title {
		color: #1a1a1a;
	}

	:global([data-theme="retro-16bit-light"]) .chart-delta {
		color: #1a6b1a;
	}

	:global([data-theme="retro-16bit-light"]) .chart-value {
		color: #1a1a1a;
	}

	:global([data-theme="retro-16bit-light"]) .grid-line {
		stroke: #ccc;
	}

	:global([data-theme="retro-16bit-light"]) .grid-label {
		fill: #666;
	}

	:global([data-theme="retro-16bit-light"]) .area-fill {
		fill: rgba(26, 26, 26, 0.06);
	}

	:global([data-theme="retro-16bit-light"]) .line-path {
		stroke: #1a1a1a;
	}

	:global([data-theme="retro-16bit-light"]) .line-point {
		fill: #1a1a1a;
		stroke: #f5f5f0;
	}

	:global([data-theme="retro-16bit-light"]) .annotation circle {
		fill: #8a8a00;
	}

	:global([data-theme="retro-16bit-light"]) .annotation text {
		fill: #8a8a00;
	}

	:global([data-theme="retro-16bit-light"]) .annotation-resume circle,
	:global([data-theme="retro-16bit-light"]) .annotation-resume text {
		fill: #1a6b1a;
		color: #1a6b1a;
	}

	:global([data-theme="retro-16bit-light"]) .annotation-retry circle,
	:global([data-theme="retro-16bit-light"]) .annotation-retry text {
		fill: #444;
		color: #444;
	}

	:global([data-theme="retro-16bit-light"]) .chart-footer {
		color: #666;
	}

	:global([data-theme="retro-16bit-light"]) .empty-state {
		background: #eeeee8;
		border-color: #999;
		border-radius: 0;
		color: #666;
	}

	:global([data-theme="retro-16bit-light"]) .empty-state.compact {
		border-radius: 0;
	}
</style>
