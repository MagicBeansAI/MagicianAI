<script lang="ts">
	/**
	 * TodayOpsEconomicsSlide.svelte
	 *
	 * Slide 1 of the Today Operations carousel ("Economics of Operations"):
	 * daily LLM spend hero, delta vs yesterday, model calls, the compact
	 * 24-hour spend histogram and the Analytics link.
	 */
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { formatSpend, spendTone } from '$lib/today/pulseFormat';
	import { formatDelta, type LlmPulse, type PulseSnapshot } from '$lib/today/pulseQueries';
	import { avgCostPerCall, formatPerCall, peakSpendHour } from '$lib/today/opsCarousel';

	export let llm: LlmPulse | null | undefined = null;
	/** The rest of the pulse (coding runs, memories, evals) for the stat grid. */
	export let pulse: PulseSnapshot | null | undefined = null;

	// --- Model Economics & LLM Spend ---
	$: spendToday = llm?.spendToday ?? 0;
	$: spendYesterday = llm?.spendYesterday ?? 0;
	$: callsToday = llm?.callsToday ?? 0;
	$: hourlySpend = llm?.hourlySpend ?? new Array(24).fill(0);
	$: hourlyCalls = llm?.hourlyCalls ?? new Array(24).fill(0);
	$: topProvider = llm?.topProvider ?? null;
	$: topProviderShare = topProvider
		? (topProvider.sharePct % 1 === 0
			? topProvider.sharePct.toString()
			: topProvider.sharePct.toFixed(2))
		: '';

	$: deltaTone = spendTone(spendToday, spendYesterday);
	$: deltaString = formatDelta(spendToday, spendYesterday, 'currency');
	$: deltaLabel = deltaString
		? `${deltaString} vs ${formatSpend(spendYesterday)} yday`
		: `steady vs ${formatSpend(spendYesterday)} yday`;

	// Splitting formatted spend for newspaper styling ($0 .42)
	$: formattedSpend = formatSpend(spendToday);
	$: spendMatch = formattedSpend.match(/^\$([0-9]+)(?:\.([0-9]{2}))?$/);
	$: spendDollars = spendMatch ? spendMatch[1] : '0';
	$: spendCents = spendMatch && spendMatch[2] ? `.${spendMatch[2]}` : '';

	// Stat grid: fills the slide now that the fleet pie lives on slide 2.
	$: perCall = formatPerCall(avgCostPerCall(spendToday, callsToday));
	$: peakHour = peakSpendHour(hourlySpend) ?? '—';
	$: codingRuns = pulse?.codingRunsToday ?? 0;
	$: memories = pulse?.memoriesToday ?? 0;
	$: evalCases = pulse?.evals?.casesToday ?? 0;
	$: evalPasses = pulse?.evals?.passesToday ?? 0;

	// 24-Hour Graph Calculations
	$: maxHourlySpend = Math.max(...hourlySpend, 0.001);
	const currentHour = new Date().getHours();
	let hoveredBar: BarPoint | null = null;

	// SVG coordinate generation for compact 24-hour hourly histogram & trendline
	const CHART_WIDTH = 136;
	const CHART_HEIGHT = 38;
	const BASELINE_Y = 30;
	const MAX_BAR_HEIGHT = 24;

	interface BarPoint {
		hour: number;
		x: number;
		y: number;
		height: number;
		spend: number;
		calls: number;
		label: string;
		isCurrent: boolean;
	}

	$: barPoints = Array.from({ length: 24 }, (_, i): BarPoint => {
		const spend = hourlySpend[i] ?? 0;
		const calls = hourlyCalls[i] ?? 0;
		const height = spend > 0 ? Math.max(3, (spend / maxHourlySpend) * MAX_BAR_HEIGHT) : 1.5;
		const x = i * 5.2 + 3;
		const y = BASELINE_Y - height;
		const hour12 = i === 0 ? '12a' : i === 12 ? '12p' : i > 12 ? `${i - 12}p` : `${i}a`;
		return {
			hour: i,
			x,
			y,
			height,
			spend,
			calls,
			label: hour12,
			isCurrent: i === currentHour
		};
	});

	$: trendlinePoints = barPoints
		.map((p) => `${p.x + 1.7},${p.spend > 0 ? p.y : BASELINE_Y}`)
		.join(' ');
	$: areaPolygon = `3,${BASELINE_Y} ${trendlinePoints} 124,${BASELINE_Y}`;
</script>

<div class="np-ledger__panel np-ops-economics">
	<div class="np-panel__kicker">
		<div class="kicker-group">
			<span class="kicker-dot"></span>
			{#if topProvider}
				<span class="kicker-text" title="Dominant LLM provider and model">
					TOP PROVIDER: <strong>{topProvider.name}</strong> / {topProvider.model} ({topProviderShare}%)
				</span>
			{:else}
				<span class="kicker-text">COMMERCIAL MODEL EXPENDITURES</span>
			{/if}
		</div>
		<a href="/llm#today" class="np-panel__link" title="Open detailed LLM spend ledger">
			<span>Analytics</span>
			<Icon name="arrow-right" size={11} />
		</a>
	</div>

	<!-- Vertically Aligned Spend Row with Compact Graph Placed Directly Next to Comparison & Model Calls -->
	<div class="np-spend-row">
		<div class="np-spend-hero">
			<div class="np-spend-hero__figure">
				<span class="np-spend-currency">$</span>
				<span class="np-spend-dollars">{spendDollars}</span>
				{#if spendCents}
					<span class="np-spend-cents">{spendCents}</span>
				{/if}
			</div>
			<div class="np-spend-hero__divider" aria-hidden="true"></div>
			<div class="np-spend-hero__meta">
				<div
					class="np-spend-delta"
					class:is-good={deltaTone === 'good'}
					class:is-bad={deltaTone === 'bad'}
					class:is-neutral={deltaTone === 'neutral'}
				>
					<span>{deltaLabel}</span>
				</div>
				<div class="np-spend-calls">
					<strong>{callsToday.toLocaleString()}</strong> model calls today
				</div>
			</div>
		</div>

		<dl class="np-ops-stats" aria-label="Today at a glance">
			<div class="np-ops-stat">
				<dt>Avg / call</dt>
				<dd>{perCall}</dd>
			</div>
			<div class="np-ops-stat">
				<dt>Peak hour</dt>
				<dd>{peakHour}</dd>
			</div>
			<div class="np-ops-stat">
				<dt>Coding runs</dt>
				<dd>{codingRuns.toLocaleString()}</dd>
			</div>
			<div class="np-ops-stat">
				{#if evalCases > 0}
					<dt>Evals passed</dt>
					<dd>{evalPasses}/{evalCases}</dd>
				{:else}
					<dt>Memories</dt>
					<dd>{memories.toLocaleString()}</dd>
				{/if}
			</div>
		</dl>

		<!-- Compact 24-Hour Spend Histogram Placed Horizontally Beside Hero & Calls -->
		<div class="np-chart-wrap" aria-label="24-hour spend graph">
			<svg
				class="np-chart-svg"
				viewBox="0 0 {CHART_WIDTH} {CHART_HEIGHT}"
				preserveAspectRatio="none"
				role="img"
				aria-label="Hourly spend graph over 24 hours"
			>
				<defs>
					<linearGradient id="spendAreaGradTheme" x1="0" y1="0" x2="0" y2="1">
						<stop offset="0%" stop-color="var(--accent-primary, #6366f1)" stop-opacity="0.3" />
						<stop offset="100%" stop-color="var(--accent-primary, #6366f1)" stop-opacity="0.0" />
					</linearGradient>
				</defs>

				{#if spendToday > 0}
					<polygon points={areaPolygon} fill="url(#spendAreaGradTheme)" />
					<polyline
						points={trendlinePoints}
						fill="none"
						stroke="var(--accent-primary, #6366f1)"
						stroke-width="1.2"
						stroke-linecap="round"
						stroke-linejoin="round"
						opacity="0.75"
					/>
				{/if}

				<line
					x1="2"
					y1={BASELINE_Y}
					x2={CHART_WIDTH - 2}
					y2={BASELINE_Y}
					stroke="var(--border-color, rgba(128, 128, 128, 0.25))"
					stroke-width="1"
				/>

				{#each barPoints as p (p.hour)}
					<rect
						x={p.x}
						y={p.y}
						width="3.5"
						height={p.height}
						rx="0.75"
						class="np-bar"
						class:is-active={p.spend > 0}
						class:is-current={p.isCurrent}
						class:is-hovered={hoveredBar?.hour === p.hour}
					/>
				{/each}

				<!-- Transparent full-height hitboxes spanning each column for instant interactive cost + calls tooltip -->
				{#each barPoints as p (p.hour)}
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<rect
						x={p.x - 0.8}
						y={0}
						width="5.2"
						height={CHART_HEIGHT}
						fill="transparent"
						class="np-bar-hit"
						on:mouseenter={() => (hoveredBar = p)}
						on:mouseleave={() => (hoveredBar = null)}
					>
						<title>{`${p.label}: $${p.spend.toFixed(2)} · ${p.calls} ${p.calls === 1 ? 'call' : 'calls'}${p.isCurrent ? ' (Current Hour)' : ''}`}</title>
					</rect>
				{/each}
			</svg>

			<div class="np-chart-axis">
				{#if hoveredBar}
					<span class="axis-hover-info">
						<strong>{hoveredBar.label}:</strong> ${hoveredBar.spend.toFixed(2)} <span class="axis-calls">({hoveredBar.calls} {hoveredBar.calls === 1 ? 'call' : 'calls'})</span>
					</span>
				{:else}
					<span>12a</span>
					<span>12p</span>
					<span class="axis-now">NOW</span>
				{/if}
			</div>
		</div>
	</div>
</div>

<style>
	.np-ops-economics {
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.85rem;
		height: 100%;
		min-width: 0;
	}

	.np-panel__kicker {
		display: flex;
		justify-content: space-between;
		align-items: center;
		font-family: 'Cinzel', Georgia, serif;
		font-size: 0.68rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--text-muted, #64748b);
	}

	.kicker-text {
		font-family: var(--font-primary, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif);
		font-size: 0.72rem;
		font-weight: 500;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--text-muted, #64748b);
	}

	.kicker-text strong {
		font-weight: 700;
		color: var(--text-primary, #1e293b);
	}

	.kicker-group {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.kicker-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--accent-primary, #6366f1);
	}

	.np-panel__link {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		color: var(--text-muted, #64748b);
		text-decoration: none;
		font-family: 'Cinzel', Georgia, serif;
		font-size: 0.65rem;
		letter-spacing: 0.08em;
		transition: color 0.15s ease;
	}

	.np-panel__link:hover {
		color: var(--accent-primary, #6366f1);
		text-decoration: underline;
	}

	/* Spend Hero Row - Figure + Divider + Meta + Compact Sparkline */
	.np-ops-stats {
		display: grid;
		grid-template-columns: repeat(2, minmax(4.5rem, auto));
		gap: 0.3rem 1.1rem;
		margin: 0;
		padding: 0 0.9rem;
		border-left: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
		border-right: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
	}

	.np-ops-stat {
		display: flex;
		flex-direction: column;
		gap: 0.05rem;
		min-width: 0;
	}

	.np-ops-stat dt {
		font-family: var(--font-mono, monospace);
		font-size: 0.6rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #64748b);
	}

	.np-ops-stat dd {
		margin: 0;
		font-family: 'Newsreader', Georgia, serif;
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary, #1e293b);
		font-variant-numeric: tabular-nums;
	}

	@media (max-width: 860px) {
		.np-ops-stats {
			order: 3;
			grid-template-columns: repeat(4, minmax(0, 1fr));
			width: 100%;
			padding: 0.35rem 0 0;
			border-left: none;
			border-right: none;
			border-top: 1px solid var(--border-color, rgba(128, 128, 128, 0.2));
		}
	}

	.np-spend-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.85rem;
		flex-wrap: wrap;
	}

	.np-spend-hero {
		display: flex;
		align-items: center;
		gap: 0.85rem;
	}

	.np-spend-hero__figure {
		display: inline-flex;
		align-items: center;
		font-family: 'Playfair Display', Georgia, serif;
		color: var(--text-primary, #1e293b);
		line-height: 1;
		margin: 0;
	}

	.np-spend-currency {
		font-size: 1.35rem;
		font-weight: 600;
		color: var(--text-muted, #64748b);
		margin-right: 0.1rem;
		line-height: 1;
	}

	.np-spend-dollars {
		font-size: 2.25rem;
		font-weight: 800;
		letter-spacing: -0.02em;
		line-height: 1;
	}

	.np-spend-cents {
		font-size: 1.25rem;
		font-weight: 600;
		color: var(--text-muted, #64748b);
		line-height: 1;
	}

	.np-spend-hero__divider {
		width: 1px;
		height: 30px;
		background: var(--border-color, rgba(128, 128, 128, 0.2));
		flex-shrink: 0;
		margin: 0;
	}

	.np-spend-hero__meta {
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.15rem;
		margin: 0;
	}

	.np-spend-delta {
		font-size: 0.76rem;
		font-weight: 600;
		font-family: ui-monospace, SFMono-Regular, monospace;
		line-height: 1.2;
		margin: 0;
	}

	.np-spend-delta.is-good {
		color: var(--status-success, #16a34a);
	}

	.np-spend-delta.is-bad {
		color: var(--status-error, #dc2626);
	}

	.np-spend-delta.is-neutral {
		color: var(--text-muted, #64748b);
	}

	.np-spend-calls {
		font-size: 0.76rem;
		color: var(--text-muted, #64748b);
		font-family: 'Newsreader', Georgia, serif;
		line-height: 1.2;
		margin: 0;
	}

	.np-spend-calls strong {
		color: var(--text-primary, #1e293b);
	}

	/* Compact 24-Hour Sparkline Box placed next to comparison */
	.np-chart-wrap {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		width: clamp(130px, 42%, 320px);
		flex-shrink: 0;
		background: color-mix(in srgb, var(--text-primary) 3%, transparent);
		padding: 0.25rem 0.45rem 0.15rem;
		border: 1px solid var(--border-color, rgba(128, 128, 128, 0.15));
		border-radius: 4px;
	}

	.np-chart-svg {
		width: 100%;
		height: 44px;
		overflow: visible;
	}

	.np-bar {
		fill: var(--border-color, rgba(128, 128, 128, 0.35));
		transition: fill 0.15s ease, height 0.2s ease;
	}

	.np-bar.is-active {
		fill: var(--accent-primary, #6366f1);
		opacity: 0.85;
	}

	.np-bar.is-current {
		fill: var(--text-primary, #0f172a);
		opacity: 1;
	}

	.np-bar:hover,
	.np-bar.is-hovered {
		fill: var(--accent-hover, #4f46e5);
		opacity: 1;
		cursor: crosshair;
	}

	.np-bar-hit {
		cursor: crosshair;
		pointer-events: all;
	}

	.np-chart-axis {
		display: flex;
		justify-content: space-between;
		font-size: 0.58rem;
		font-family: ui-monospace, SFMono-Regular, monospace;
		color: var(--text-muted, #94a3b8);
		line-height: 1;
		min-height: 0.75rem;
	}

	.axis-now {
		font-weight: 700;
		color: var(--accent-primary, #6366f1);
	}

	.axis-hover-info {
		font-size: 0.58rem;
		font-family: ui-monospace, SFMono-Regular, monospace;
		color: var(--accent-primary, #6366f1);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		width: 100%;
		text-align: center;
	}

	.axis-hover-info strong {
		color: var(--text-primary, #1e293b);
	}

	.axis-hover-info .axis-calls {
		color: var(--text-muted, #64748b);
	}
</style>
