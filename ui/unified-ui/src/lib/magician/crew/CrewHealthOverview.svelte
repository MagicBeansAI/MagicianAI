<script lang="ts">
	import { curveMonotoneX, line, scaleLinear, scalePoint } from 'd3';
	import type { CrewAgentHealthResponse, AgentHealthSnapshot } from './health';

	export let health: CrewAgentHealthResponse | null = null;
	export let loading = false;

	const chartWidth = 420;
	const chartHeight = 112;
	const chartPadding = { top: 10, right: 12, bottom: 18, left: 12 };

	$: projection = health?.agent ?? null;
	$: history = (projection?.history ?? []).slice(-30);
	$: xScale = scalePoint<string>()
		.domain(history.map((point) => point.date))
		.range([chartPadding.left, chartWidth - chartPadding.right])
		.padding(0.35);
	$: yScale = scaleLinear()
		.domain([0, 100])
		.range([chartHeight - chartPadding.bottom, chartPadding.top]);
	$: trendPath = line<AgentHealthSnapshot>()
		.x((point) => xScale(point.date) ?? chartPadding.left)
		.y((point) => yScale(point.score))
		.curve(curveMonotoneX)(history);

	function stateLabel(state: string): string {
		return state
			.split('_')
			.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
			.join(' ');
	}

	function trendLabel(trend: string, delta: number | null): string {
		if (trend === 'insufficient_history') return 'Collecting history';
		const signed = delta == null ? '' : ` ${delta > 0 ? '+' : ''}${delta}`;
		return `${stateLabel(trend)}${signed}`;
	}

	function dateLabel(value: string): string {
		const parsed = new Date(`${value}T00:00:00Z`);
		return Number.isNaN(parsed.getTime())
			? value
			: parsed.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
	}
</script>

<section class="crew-health" aria-label="Crew member health">
	<header class="crew-health__header">
		<div>
			<h2>Health</h2>
			<p>Overall readiness and rolling 7-day operations</p>
		</div>
		{#if projection}
			<span class="crew-health__coverage">
				{Math.round(projection.overall.coverage.ratio * 100)}% coverage
			</span>
		{/if}
	</header>

	{#if loading}
		<div class="crew-health__skeleton" aria-label="Loading health">
			<span></span><span></span><span></span>
		</div>
	{:else if projection}
		<div class="crew-health__body">
			<div class="crew-health__overall" data-band={projection.overall.band}>
				<span class="crew-health__eyebrow">Overall</span>
				<strong>{projection.overall.score}</strong>
				<span>{stateLabel(projection.overall.inputs.state)}</span>
				<div class="crew-health__meter" aria-label={`Overall health ${projection.overall.score} out of 100`}>
					<i style={`width: ${projection.overall.score}%`}></i>
				</div>
			</div>

			<div class="crew-health__metrics" aria-label="Rolling 7-day health metrics">
				<div>
					<span>7d average</span>
					<strong>{projection.rolling_7d.score_average?.toFixed(1) ?? '-'}</strong>
				</div>
				<div>
					<span>Trend</span>
					<strong data-trend={projection.rolling_7d.trend}>
						{trendLabel(projection.rolling_7d.trend, projection.rolling_7d.score_delta)}
					</strong>
				</div>
				<div>
					<span>LLM success</span>
					<strong>{projection.rolling_7d.success_rate == null ? '-' : `${Math.round(projection.rolling_7d.success_rate * 100)}%`}</strong>
				</div>
				<div>
					<span>Calls</span>
					<strong>{projection.rolling_7d.calls.toLocaleString()}</strong>
				</div>
				<div>
					<span>Spend</span>
					<strong>{projection.rolling_7d.calls > 0 && (projection.rolling_7d.cost_observed_calls ?? 0) === 0 ? '-' : `$${projection.rolling_7d.spend_usd.toFixed(2)}`}</strong>
					<small>{projection.rolling_7d.cost_observed_calls ?? 0}/{projection.rolling_7d.calls} priced</small>
				</div>
				<div>
					<span>History</span>
					<strong>{projection.rolling_7d.sample_days}/7 days</strong>
				</div>
			</div>

			<div class="crew-health__trend">
				<div class="crew-health__trend-head">
					<span>Daily history</span>
					<small>{history.length > 0 ? `${dateLabel(history[0].date)} - ${dateLabel(history[history.length - 1].date)}` : 'No samples'}</small>
				</div>
				{#if history.length > 0}
					<svg
						viewBox={`0 0 ${chartWidth} ${chartHeight}`}
						role="img"
						aria-label={`Daily health history with ${history.length} samples`}
						preserveAspectRatio="none"
					>
						<line class="crew-health__threshold crew-health__threshold--good" x1={chartPadding.left} x2={chartWidth - chartPadding.right} y1={yScale(70)} y2={yScale(70)} />
						<line class="crew-health__threshold crew-health__threshold--fair" x1={chartPadding.left} x2={chartWidth - chartPadding.right} y1={yScale(40)} y2={yScale(40)} />
						{#if trendPath}<path class="crew-health__line" d={trendPath}></path>{/if}
						{#each history as point (point.date)}
							<circle
								class="crew-health__point"
								data-band={point.band}
								cx={xScale(point.date) ?? chartPadding.left}
								cy={yScale(point.score)}
								r="3.5"
							>
								<title>{dateLabel(point.date)}: {point.score} health ({Math.round(point.coverage.ratio * 100)}% coverage)</title>
							</circle>
						{/each}
					</svg>
				{:else}
					<p class="crew-health__empty">History will appear after the first durable snapshot.</p>
				{/if}
			</div>
		</div>
	{:else}
		<p class="crew-health__empty">Health is currently unavailable.</p>
	{/if}
</section>

<style>
	.crew-health {
		container-type: inline-size;
		min-width: 0;
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
		overflow: hidden;
		padding: 1rem;
		border: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.28));
		border-radius: 8px;
		background: var(--bg-card, rgba(128, 128, 128, 0.04));
		color: var(--text-primary, #272522);
	}
	.crew-health__header,
	.crew-health__trend-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}
	.crew-health__header h2 {
		margin: 0;
		font-size: 1rem;
		letter-spacing: 0;
	}
	.crew-health__header p {
		margin: 0.15rem 0 0;
		font-size: 0.75rem;
		color: var(--text-secondary, #6b6760);
	}
	.crew-health__coverage {
		font-size: 0.72rem;
		color: var(--text-secondary, #6b6760);
		white-space: nowrap;
	}
	.crew-health__body {
		display: grid;
		grid-template-columns: minmax(0, 0.7fr) minmax(0, 1.35fr) minmax(0, 1.7fr);
		gap: 1rem;
		align-items: stretch;
		margin-top: 0.9rem;
		min-width: 0;
	}
	.crew-health__overall {
		display: grid;
		align-content: center;
		gap: 0.3rem;
		min-width: 0;
	}
	.crew-health__eyebrow,
	.crew-health__metrics span,
	.crew-health__trend-head span {
		font-size: 0.68rem;
		font-weight: 650;
		color: var(--text-secondary, #6b6760);
		letter-spacing: 0;
	}
	.crew-health__overall > strong {
		font-size: 2.35rem;
		line-height: 1;
		font-variant-numeric: tabular-nums;
		color: var(--color-success, #27834a);
	}
	.crew-health__overall[data-band='fair'] > strong {
		color: var(--color-warning, #b76c16);
	}
	.crew-health__overall[data-band='poor'] > strong {
		color: var(--color-error, #c64343);
	}
	.crew-health__overall > span:not(.crew-health__eyebrow) {
		font-size: 0.78rem;
	}
	.crew-health__meter {
		height: 5px;
		margin-top: 0.35rem;
		overflow: hidden;
		background: var(--bg-muted, rgba(128, 128, 128, 0.18));
		border-radius: 3px;
	}
	.crew-health__meter i {
		display: block;
		height: 100%;
		background: var(--color-success, #27834a);
	}
	.crew-health__overall[data-band='fair'] .crew-health__meter i {
		background: var(--color-warning, #b76c16);
	}
	.crew-health__overall[data-band='poor'] .crew-health__meter i {
		background: var(--color-error, #c64343);
	}
	.crew-health__metrics {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 0.75rem 1rem;
		padding: 0 1rem;
		border-inline: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.22));
	}
	.crew-health__metrics div {
		display: grid;
		align-content: center;
		gap: 0.2rem;
		min-width: 0;
	}
	.crew-health__metrics strong {
		font-size: 0.84rem;
		font-variant-numeric: tabular-nums;
		overflow-wrap: anywhere;
	}
	.crew-health__metrics strong[data-trend='improving'] {
		color: var(--color-success, #27834a);
	}
	.crew-health__metrics strong[data-trend='declining'] {
		color: var(--color-error, #c64343);
	}
	.crew-health__trend {
		min-width: 0;
	}
	.crew-health__trend-head small {
		font-size: 0.66rem;
		color: var(--text-tertiary, #817c74);
		white-space: nowrap;
	}
	.crew-health__trend svg {
		display: block;
		width: 100%;
		max-width: 100%;
		height: 7rem;
		margin-top: 0.25rem;
		overflow: hidden;
	}
	.crew-health__threshold {
		stroke-width: 1;
		stroke-dasharray: 3 4;
		vector-effect: non-scaling-stroke;
	}
	.crew-health__threshold--good {
		stroke: color-mix(in srgb, var(--color-success, #27834a) 28%, transparent);
	}
	.crew-health__threshold--fair {
		stroke: color-mix(in srgb, var(--color-warning, #b76c16) 28%, transparent);
	}
	.crew-health__line {
		fill: none;
		stroke: var(--accent-primary, #4776b4);
		stroke-width: 2;
		vector-effect: non-scaling-stroke;
	}
	.crew-health__point {
		fill: var(--color-success, #27834a);
		stroke: var(--bg-card, #fff);
		stroke-width: 1.5;
		vector-effect: non-scaling-stroke;
	}
	.crew-health__point[data-band='fair'] {
		fill: var(--color-warning, #b76c16);
	}
	.crew-health__point[data-band='poor'] {
		fill: var(--color-error, #c64343);
	}
	.crew-health__empty {
		margin: 1rem 0 0;
		font-size: 0.78rem;
		color: var(--text-secondary, #6b6760);
	}
	.crew-health__skeleton {
		display: grid;
		grid-template-columns: 0.7fr 1.35fr 1.7fr;
		gap: 1rem;
		margin-top: 0.9rem;
	}
	.crew-health__skeleton span {
		height: 6.5rem;
		border-radius: 6px;
		background: linear-gradient(
			90deg,
			var(--bg-muted, rgba(128, 128, 128, 0.12)) 25%,
			var(--bg-hover, rgba(128, 128, 128, 0.22)) 50%,
			var(--bg-muted, rgba(128, 128, 128, 0.12)) 75%
		);
		background-size: 200% 100%;
		animation: health-skeleton 1.2s ease-in-out infinite;
	}
	@keyframes health-skeleton {
		to { background-position: -200% 0; }
	}
	@container (max-width: 720px) {
		.crew-health__body,
		.crew-health__skeleton {
			grid-template-columns: 1fr;
		}
		.crew-health__metrics {
			grid-template-columns: repeat(3, minmax(0, 1fr));
			padding: 0.8rem 0;
			border-inline: 0;
			border-block: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.22));
		}
		.crew-health__trend {
			grid-column: auto;
		}
	}
	@container (max-width: 480px) {
		.crew-health__metrics {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (max-width: 900px) {
		.crew-health__body {
			grid-template-columns: minmax(0, 0.55fr) minmax(0, 1.45fr);
		}
		.crew-health__trend {
			grid-column: 1 / -1;
		}
		.crew-health__metrics {
			border-inline-end: 0;
		}
	}
	@media (max-width: 620px) {
		.crew-health__header {
			align-items: flex-start;
		}
		.crew-health__body,
		.crew-health__skeleton {
			grid-template-columns: 1fr;
		}
		.crew-health__metrics {
			grid-template-columns: repeat(2, minmax(0, 1fr));
			padding: 0.8rem 0;
			border-inline: 0;
			border-block: 1px solid var(--border-subtle, rgba(128, 128, 128, 0.22));
		}
		.crew-health__trend {
			grid-column: auto;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.crew-health__skeleton span { animation: none; }
	}
</style>
