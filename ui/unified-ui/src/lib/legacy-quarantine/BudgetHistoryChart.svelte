<script lang="ts">
	interface BudgetSpendPoint {
		amount: number;
		timestamp: number;
		reason?: string;
		stage_context?: string;
	}

	export let initial = 0;
	export let remaining = 0;
	export let spent: BudgetSpendPoint[] = [];
	export let width = 280;
	export let height = 160;
	export let compact = false;

	// Adjust padding based on compact mode
	$: PADDING = compact
		? { top: 2, right: 4, bottom: 12, left: 20 }
		: { top: 12, right: 16, bottom: 28, left: 52 };

	const clamp = (value: number, min: number, max: number) =>
		Math.min(max, Math.max(min, value));

	$: sortedSpends = [...(spent ?? [])]
		.map((entry) => {
			const timestamp =
				typeof entry.timestamp === 'number'
					? entry.timestamp
					: Date.parse(String(entry.timestamp));
			const amount = Number(entry.amount);
			return { amount, timestamp, reason: entry.reason, stage_context: entry.stage_context };
		})
		.filter((entry) => Number.isFinite(entry.timestamp) && Number.isFinite(entry.amount))
		.sort((a, b) => a.timestamp - b.timestamp);

	$: baseTimestamp = sortedSpends.length
		? sortedSpends[0].timestamp
		: Date.now() - 5 * 60 * 1000;
	$: finalTimestamp = sortedSpends.length
		? sortedSpends[sortedSpends.length - 1].timestamp
		: Date.now();

	$: timeline = (() => {
		const points: Array<{ timestamp: number; remaining: number; spent: number; reason?: string }> = [];
		let cumulative = 0;

		points.push({
			timestamp: baseTimestamp - 1000,
			remaining: Math.max(0, initial),
			spent: 0
		});

		for (const entry of sortedSpends) {
			cumulative += entry.amount;
			points.push({
				timestamp: entry.timestamp,
				remaining: Math.max(0, initial - cumulative),
				spent: Math.max(0, cumulative),
				reason: entry.reason
			});
		}

		const inferredSpent = Math.max(0, initial - remaining);
		points.push({
			timestamp: Math.max(finalTimestamp + 1000, Date.now()),
			remaining: Math.max(0, remaining),
			spent: inferredSpent
		});

		return points;
	})();

	$: hasChart = initial > 0 && timeline.length >= 2;

	$: minTime = hasChart ? timeline[0].timestamp : Date.now() - 1;
	$: maxTime = hasChart ? timeline[timeline.length - 1].timestamp : minTime + 1;
	$: timeSpan = Math.max(1, maxTime - minTime);

	$: maxAmount = hasChart
		? Math.max(
				initial,
				...timeline.map((point) => point.spent),
				...timeline.map((point) => point.remaining)
		  )
		: 1;

	$: tickValues = hasChart
		? Array.from({ length: 4 }, (_, idx) => (maxAmount / 3) * idx)
		: [];

	$: innerWidth = width - PADDING.left - PADDING.right;
	$: innerHeight = height - PADDING.top - PADDING.bottom;

	const scaleX = (timestamp: number) =>
		PADDING.left + ((timestamp - minTime) / timeSpan) * innerWidth;

	const scaleY = (value: number) =>
		PADDING.top + (1 - clamp(value, 0, maxAmount) / maxAmount) * innerHeight;

	const makePath = (values: number[]) =>
		values
			.map((value, index) => {
				const point = timeline[index];
				const x = scaleX(point.timestamp).toFixed(2);
				const y = scaleY(value).toFixed(2);
				return `${index === 0 ? 'M' : 'L'}${x},${y}`;
			})
			.join(' ');

	$: spentPath = hasChart ? makePath(timeline.map((point) => point.spent)) : '';
	$: remainingPath = hasChart ? makePath(timeline.map((point) => point.remaining)) : '';

	$: spentAreaPath = hasChart
		? `${spentPath} L${scaleX(maxTime).toFixed(2)},${scaleY(0).toFixed(2)} L${scaleX(minTime).toFixed(
				2
		  )},${scaleY(0).toFixed(2)} Z`
		: '';

	$: startLabel = hasChart
		? new Date(minTime).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
		: '';
	$: endLabel = hasChart
		? new Date(maxTime).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
		: '';
</script>

<div class="chart-container" class:compact>
	{#if !compact}
		<div class="chart-header">
			<div>
				<div class="chart-title">Budget Usage</div>
				<div class="chart-subtitle">Initial {initial.toFixed(2)} • Remaining {remaining.toFixed(2)}</div>
			</div>
			<div class="chart-value">
				Spent {(Math.max(0, initial - remaining)).toFixed(2)}
			</div>
		</div>
	{/if}

	{#if hasChart}
		<svg class="chart-canvas" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none">
			<!-- Grid - fewer ticks in compact mode -->
			{#each compact ? tickValues.filter((_, i) => i % 2 === 0) : tickValues as tick}
				<line
					x1={PADDING.left}
					x2={width - PADDING.right}
					y1={scaleY(tick)}
					y2={scaleY(tick)}
					class="grid-line"
				/>
				<text x={PADDING.left - 4} y={scaleY(tick) + 3} class="grid-label" class:compact>{tick.toFixed(0)}</text>
			{/each}

			<!-- Spent area -->
			<path d={spentAreaPath} class="spent-area" />

			<!-- Spent line -->
			<path d={spentPath} class="spent-line" class:compact />

			<!-- Remaining line -->
			<path d={remainingPath} class="remaining-line" class:compact />

			<!-- Data markers - smaller in compact mode -->
			{#each timeline as point, idx}
				<g transform={`translate(${scaleX(point.timestamp)}, ${scaleY(point.remaining)})`}>
					<circle r={compact ? 1.5 : 2.5} class="remaining-dot" />
				</g>
				<g transform={`translate(${scaleX(point.timestamp)}, ${scaleY(point.spent)})`}>
					<circle r={compact ? 1.2 : 2} class="spent-dot" />
				</g>
			{/each}
		</svg>
		<div class="chart-footer" class:compact>
			<span>{startLabel}</span>
			<span>{endLabel}</span>
		</div>
	{:else}
		<div class="empty-state" class:compact>
			{compact ? 'No data' : 'Budget activity will appear once the mission starts spending tokens.'}
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

	.chart-subtitle {
		font-size: 0.75rem;
		color: var(--text-muted, #64748b);
		margin-top: 0.1rem;
	}

	.chart-value {
		font-weight: 700;
		font-size: 1rem;
		color: var(--color-error, #db2777);
	}

	.chart-canvas {
		width: 100%;
		height: auto;
	}

	.grid-line {
		stroke: var(--border-soft, rgba(148, 163, 184, 0.3));
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

	.spent-area {
		fill: color-mix(in srgb, var(--color-error, #dc2626) 12%, transparent);
	}

	.spent-line {
		fill: none;
		stroke: var(--color-error, #dc2626);
		stroke-width: 2.25;
	}

	.spent-line.compact {
		stroke-width: 1.5;
	}

	.remaining-line {
		fill: none;
		stroke: var(--color-success, #10b981);
		stroke-width: 2.25;
	}

	.remaining-line.compact {
		stroke-width: 1.5;
	}

	.remaining-dot {
		fill: var(--color-success, #10b981);
		stroke: var(--bg-elevated, white);
		stroke-width: 1.5;
	}

	.spent-dot {
		fill: var(--color-error, #dc2626);
		stroke: var(--bg-elevated, white);
		stroke-width: 1.2;
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

	:global([data-theme="retro-16bit"]) .chart-subtitle {
		color: #805800;
	}

	:global([data-theme="retro-16bit"]) .chart-value {
		color: #ff4444;
	}

	:global([data-theme="retro-16bit"]) .grid-line {
		stroke: #333;
	}

	:global([data-theme="retro-16bit"]) .grid-label {
		fill: #805800;
	}

	:global([data-theme="retro-16bit"]) .spent-area {
		fill: rgba(255, 68, 68, 0.1);
	}

	:global([data-theme="retro-16bit"]) .spent-line {
		stroke: #ff4444;
	}

	:global([data-theme="retro-16bit"]) .remaining-line {
		stroke: #00ff41;
	}

	:global([data-theme="retro-16bit"]) .remaining-dot {
		fill: #00ff41;
		stroke: #000;
	}

	:global([data-theme="retro-16bit"]) .spent-dot {
		fill: #ff4444;
		stroke: #000;
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

	:global([data-theme="retro-16bit-light"]) .chart-subtitle {
		color: #666;
	}

	:global([data-theme="retro-16bit-light"]) .chart-value {
		color: #8b0000;
	}

	:global([data-theme="retro-16bit-light"]) .grid-line {
		stroke: #ccc;
	}

	:global([data-theme="retro-16bit-light"]) .grid-label {
		fill: #666;
	}

	:global([data-theme="retro-16bit-light"]) .spent-area {
		fill: rgba(139, 0, 0, 0.06);
	}

	:global([data-theme="retro-16bit-light"]) .spent-line {
		stroke: #8b0000;
	}

	:global([data-theme="retro-16bit-light"]) .remaining-line {
		stroke: #1a6b1a;
	}

	:global([data-theme="retro-16bit-light"]) .remaining-dot {
		fill: #1a6b1a;
		stroke: #f5f5f0;
	}

	:global([data-theme="retro-16bit-light"]) .spent-dot {
		fill: #8b0000;
		stroke: #f5f5f0;
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
