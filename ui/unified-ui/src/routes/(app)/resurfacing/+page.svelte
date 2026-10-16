<script lang="ts">
	/**
	 * Resurfacing observability — the operator glance surface for the Proactive
	 * Resurfacing Engine. Renders pipeline health (per-kind background passes),
	 * the candidate funnel, recent runs, table sizes + corpus watermarks, and
	 * per-lane engagement, all from the single `/resurfacing/observability`
	 * endpoint.
	 *
	 * Fetch mirrors the sibling observability pages: plain `fetch` (the installed
	 * wrapper attaches the workspace-bound bearer), a manual Refresh button, a light 30s
	 * auto-poll, and a scope-reactive reload so switching workspace refetches.
	 * Degrades gracefully: an engine that has never run maps to a zeroed shape
	 * and renders empty-state copy rather than crashing.
	 */
	import { onMount, onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import Icon from '$lib/shared/icons/Icon.svelte';

	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		emptyObservability,
		fetchResurfacingObservability,
		type PipelineKindStats,
		type ResurfacingObservability
	} from '$lib/resurfacing/observabilityQueries';

	let obs: ResurfacingObservability = emptyObservability();
	let loading = true;
	let failed = false;
	// Per-scope request guard: a scope switch mid-flight must not paint an old
	// response under the new scope (mirrors ResurfacingBand's requestId).
	let requestId = 0;
	let timer: ReturnType<typeof setInterval> | null = null;

	async function load(): Promise<void> {
		const rid = ++requestId;
		const next = await fetchResurfacingObservability();
		if (rid !== requestId) return;
		loading = false;
		if (next === null) {
			failed = true;
			return;
		}
		failed = false;
		obs = next;
	}

	// Fetch on mount and refetch on every scope switch: scopeKey is the sole
	// dependency, so this runs once on mount and again only when the
	// principal/workspace actually change.
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (browser && scopeKey) void load();

	onMount(() => {
		timer = setInterval(() => void load(), 30000);
	});

	onDestroy(() => {
		// Bump the request id so any in-flight fetch is dropped on unmount.
		requestId += 1;
		if (timer) clearInterval(timer);
	});

	// ── Derived views ──────────────────────────────────────────────────────
	const STATE_ORDER = ['candidate', 'surfaced', 'acted', 'dismissed', 'snoozed'];
	const LANE_ORDER = ['memory', 'task', 'comm'];
	const skeletonCards = [0, 1, 2, 3];

	// Always show the canonical states (0-filled) for a stable layout, then any
	// extra states the backend reports beyond the known set.
	$: stateRows = [
		...STATE_ORDER,
		...Object.keys(obs.funnel.by_state).filter((s) => !STATE_ORDER.includes(s))
	].map((state) => ({ state, count: obs.funnel.by_state[state] ?? 0 }));
	$: maxStateCount = Math.max(...stateRows.map((row) => row.count), 1);
	// Include any lanes the backend reports beyond the known three.
	$: laneRows = [
		...LANE_ORDER.filter((l) => l in obs.funnel.by_lane),
		...Object.keys(obs.funnel.by_lane).filter((l) => !LANE_ORDER.includes(l))
	].map((lane) => ({ lane, count: obs.funnel.by_lane[lane] ?? 0 }));
	$: maxLaneCount = Math.max(...laneRows.map((row) => row.count), 1);

	$: sizeRows = [
		{ label: 'Candidates', value: obs.sizes.candidates },
		{ label: 'Embeddings', value: obs.sizes.embeddings },
		{ label: 'Phrasing', value: obs.sizes.phrasing },
		{ label: 'Dismissed signals', value: obs.sizes.dismissed_signals },
		{ label: 'Affinity signals', value: obs.sizes.affinity_signals },
		{ label: 'Runs', value: obs.sizes.runs }
	];

	$: isEmpty =
		!loading &&
		!failed &&
		obs.pipeline.length === 0 &&
		obs.recent_runs.length === 0 &&
		obs.queue.pending === 0 &&
		obs.queue.surfaced === 0 &&
		obs.sizes.candidates === 0 &&
		obs.sizes.runs === 0;
	$: failedPasses = obs.pipeline.reduce((sum, p) => sum + p.failures, 0);
	$: totalRuns = obs.pipeline.reduce((sum, p) => sum + p.total, 0);
	$: totalProduced = obs.pipeline.reduce((sum, p) => sum + p.total_produced, 0);
	$: healthTone = failed ? 'bad' : failedPasses > 0 ? 'warn' : loading ? 'loading' : 'good';
	$: summaryRows = [
		{ label: 'Pending', value: obs.queue.pending, tone: 'pending', icon: 'clock' as const },
		{ label: 'Pool', value: obs.queue.candidate_pool, tone: 'pool', icon: 'inbox' as const },
		{ label: 'Cooling', value: obs.queue.cooling, tone: 'cooling', icon: 'moon' as const },
		{ label: 'Surfaced', value: obs.queue.surfaced, tone: 'surfaced', icon: 'sparkle' as const },
		{ label: 'Runs', value: totalRuns, tone: 'runs', icon: 'rotate-ccw' as const },
		{ label: 'Produced', value: totalProduced, tone: 'produced', icon: 'zap' as const }
	];

	// ── Formatters ─────────────────────────────────────────────────────────
	const numberFmt = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
	function compact(n: number): string {
		return numberFmt.format(n ?? 0);
	}

	function labelize(kind: string): string {
		const trimmed = (kind ?? '').trim();
		if (!trimmed) return '—';
		return trimmed.replace(/_/g, ' ').replace(/\b\w/g, (m) => m.toUpperCase());
	}

	function formatMs(ms: number): string {
		if (!Number.isFinite(ms) || ms <= 0) return '0 ms';
		if (ms < 1000) return `${Math.round(ms)} ms`;
		return `${(ms / 1000).toFixed(ms < 10000 ? 2 : 1)} s`;
	}

	function formatWhen(epochMs: number | null): string {
		if (epochMs === null || !Number.isFinite(epochMs) || epochMs <= 0) return '—';
		const elapsed = Date.now() - epochMs;
		if (elapsed < 0) return 'just now';
		const s = Math.floor(elapsed / 1000);
		if (s < 60) return `${s}s ago`;
		const m = Math.floor(s / 60);
		if (m < 60) return `${m}m ago`;
		const h = Math.floor(m / 60);
		if (h < 24) return `${h}h ago`;
		const d = Math.floor(h / 24);
		return `${d}d ago`;
	}

	function successLabel(p: PipelineKindStats): string {
		return `${compact(p.successes)} ok · ${compact(p.failures)} failed`;
	}
</script>

<div class="resurfacing-page">
	<header class="rs-head">
		<div class="rs-title">
			<p class="rs-kicker">Observation &amp; Insights</p>
			<div class="title-row">
				<h1>Resurfacing engine</h1>
				<span class="health-chip tone-{healthTone}">
					<span class="health-dot"></span>
					{healthTone === 'bad'
						? 'unavailable'
						: healthTone === 'warn'
							? `${compact(failedPasses)} failed`
							: healthTone === 'loading'
								? 'loading'
								: 'healthy'}
				</span>
			</div>
			<p class="rs-subtitle">
				Signals, runs, candidates, and engagement for proactive resurfacing
			</p>
		</div>
		<div class="rs-actions">
			<a
				class="action-button action-button--outline action-button--sm"
				href="/observe"
				title="Back to Observe console"
			>
				<Icon name="chevron-left" size={13} />
				<span>Observe</span>
			</a>
			<a
				class="action-button action-button--outline action-button--sm"
				href="/observe/stats"
				title="View pipeline ingestion and stats"
			>
				<Icon name="git-branch" size={13} />
				<span>Pipelines</span>
			</a>
			<a
				class="action-button action-button--outline action-button--sm"
				href="/today"
				title="View surfaced Today cards"
			>
				<Icon name="inbox" size={13} />
				<span>Today cards</span>
			</a>
			<button
				type="button"
				class="action-button action-button--outline action-button--sm"
				class:spinning={loading}
				on:click={() => void load()}
				title="Refresh resurfacing observability"
				aria-label="Refresh resurfacing observability"
				aria-busy={loading}
			>
				<Icon name="rotate-ccw" size={13} class={loading ? 'spinning' : ''} />
				<span>{loading ? 'Refreshing…' : 'Refresh'}</span>
			</button>
		</div>
	</header>

	{#if loading && obs.pipeline.length === 0 && obs.recent_runs.length === 0}
		<section class="summary-grid loading-grid" aria-label="Loading resurfacing summary">
			{#each skeletonCards as card (card)}
				<div class="summary-card surface-card skeleton-card">
					<span></span>
					<strong></strong>
					<em></em>
				</div>
			{/each}
		</section>
		<section class="panel-group">
			<h2 class="group-head">Pipeline</h2>
			<div class="pipeline-grid">
				{#each skeletonCards as card (card)}
					<div class="surface-card pipeline-card skeleton-panel">
						<span></span>
						<strong></strong>
						<em></em>
					</div>
				{/each}
			</div>
		</section>
	{:else if failed}
		<section class="surface-card panel state-panel danger">
			<span class="state-icon"><Icon name="alert" size={18} /></span>
			<h2>Observability unavailable</h2>
			<p class="muted small">
				Couldn't reach the resurfacing observability endpoint. It may be gated off, or the
				backend isn't running. Retry with Refresh.
			</p>
		</section>
	{:else if isEmpty}
		<section class="surface-card panel state-panel">
			<span class="state-icon"><Icon name="info" size={18} /></span>
			<h2>Nothing to show yet</h2>
			<p class="muted small">
				The resurfacing engine hasn't produced any runs or candidates in this workspace yet.
				Once the scorer / curator / retention passes run, their health and the candidate funnel
				appear here.
			</p>
		</section>
	{:else}
		<section class="summary-grid" aria-label="Resurfacing summary">
			{#each summaryRows as stat (stat.label)}
				<div class="summary-card surface-card tone-{stat.tone}">
					<div class="summary-card-top">
						<span class="summary-card-icon" aria-hidden="true">
							<Icon name={stat.icon} size={13} />
						</span>
						<span class="summary-card-label">{stat.label}</span>
					</div>
					<strong>{compact(stat.value)}</strong>
					<em></em>
				</div>
			{/each}
		</section>

		<!-- Pipeline — per-kind background-pass health. -->
		<section class="panel-group">
			<h2 class="group-head">Pipeline</h2>
			{#if obs.pipeline.length === 0}
				<p class="muted small">No pass runs recorded yet.</p>
			{:else}
				<div class="pipeline-grid">
					{#each obs.pipeline as p (p.kind)}
						<div class="surface-card pipeline-card" class:has-failures={p.failures > 0}>
							<div class="pipeline-card-head">
								<span class="pipeline-kind">{labelize(p.kind)}</span>
								<span class="pipeline-when">{formatWhen(p.last_started_at)}</span>
							</div>
							<div class="pipeline-metrics">
								<div class="pm">
									<span class="pm-value">{compact(p.total)}</span>
									<span class="pm-label">runs</span>
								</div>
								<div class="pm">
									<span class="pm-value">{formatMs(p.avg_duration_ms)}</span>
									<span class="pm-label">avg</span>
								</div>
								<div class="pm">
									<span class="pm-value">{compact(p.total_produced)}</span>
									<span class="pm-label">produced</span>
								</div>
							</div>
							<div class="pipeline-status" class:bad={p.failures > 0}>
								{successLabel(p)}
							</div>
							{#if p.last_error}
								<div class="pipeline-error" title={p.last_error}>
									<span class="err-dot">●</span>
									<span class="err-text">{p.last_error}</span>
								</div>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</section>

		<!-- Funnel — candidate lifecycle + lane breakdown. -->
		<section class="panel-group">
			<h2 class="group-head">Funnel</h2>
			<div class="two-col">
					<div class="surface-card panel">
						<h3>Candidate states</h3>
						<div class="headline-row">
							<div class="headline">
								<span class="headline-value">{compact(obs.queue.pending)}</span>
								<span class="headline-label">pending</span>
							</div>
							<div class="headline">
								<span class="headline-value">{compact(obs.queue.candidate_pool)}</span>
								<span class="headline-label">pool</span>
							</div>
							<div class="headline">
								<span class="headline-value">{compact(obs.queue.cooling)}</span>
								<span class="headline-label">cooling</span>
							</div>
							<div class="headline">
								<span class="headline-value">{compact(obs.queue.surfaced)}</span>
								<span class="headline-label">surfaced</span>
							</div>
						</div>
						<div class="state-bars">
						{#each stateRows as row (row.state)}
							<div class="state-row">
								<div class="state-label">
									<span>{labelize(row.state)}</span>
									<strong>{compact(row.count)}</strong>
								</div>
								<div class="bar-track" aria-hidden="true">
									<div
										class="bar-fill state-{row.state}"
										style="width: {Math.max(3, Math.round((row.count / maxStateCount) * 100))}%"
									></div>
								</div>
							</div>
						{/each}
					</div>
				</div>

				<div class="surface-card panel">
					<h3>By lane</h3>
					{#if laneRows.length === 0}
						<p class="muted small">No lane activity yet.</p>
					{:else}
						<div class="state-bars">
							{#each laneRows as row (row.lane)}
								<div class="state-row">
									<div class="state-label">
										<span class="badge lane-{row.lane}">{labelize(row.lane)}</span>
										<strong>{compact(row.count)}</strong>
									</div>
									<div class="bar-track" aria-hidden="true">
										<div
											class="bar-fill lane-{row.lane}"
											style="width: {Math.max(3, Math.round((row.count / maxLaneCount) * 100))}%"
										></div>
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			</div>
		</section>

		<!-- Recent runs. -->
		<section class="panel-group">
			<h2 class="group-head">Recent runs</h2>
			<div class="surface-card panel">
				{#if obs.recent_runs.length === 0}
					<p class="muted small">No runs recorded yet.</p>
				{:else}
					<div class="run-table">
						<div class="run-row run-header">
							<span>Pass</span>
							<span>When</span>
							<span class="num">Duration</span>
							<span class="num">Produced</span>
							<span>Status</span>
						</div>
						{#each obs.recent_runs as run, i (run.kind + ':' + (run.started_at ?? i))}
							<div class="run-row">
								<span class="run-kind">{labelize(run.kind)}</span>
								<span class="muted small">{formatWhen(run.started_at)}</span>
								<span class="num">{formatMs(run.duration_ms)}</span>
								<span class="num">{compact(run.produced)}</span>
								<span class="run-status">
									{#if run.success}
										<span class="pill pill-ok">ok</span>
									{:else}
										<span class="pill pill-bad" title={run.error ?? 'failed'}>failed</span>
									{/if}
									{#if run.error}<span class="run-err" title={run.error}>{run.error}</span>{/if}
								</span>
							</div>
						{/each}
					</div>
				{/if}
			</div>
		</section>

		<!-- Sizes + watermarks. -->
		<section class="panel-group">
			<h2 class="group-head">Storage</h2>
			<div class="two-col">
				<div class="surface-card panel">
					<h3>Table sizes</h3>
					{#each sizeRows as row (row.label)}
						<div class="kv">
							<span>{row.label}</span>
							<strong>{compact(row.value)}</strong>
						</div>
					{/each}
				</div>

				<div class="surface-card panel">
					<h3>Corpus watermarks</h3>
					{#if obs.watermarks.length === 0}
						<p class="muted small">No corpus scan cursors yet.</p>
					{:else}
						{#each obs.watermarks as w (w.corpus_kind)}
							<div class="kv">
								<span class="badge">{labelize(w.corpus_kind)}</span>
								<strong class="mono">{w.cursor === 0 ? '—' : formatWhen(w.cursor)}</strong>
							</div>
						{/each}
					{/if}
				</div>
			</div>
		</section>

		<!-- Engagement. -->
		<section class="panel-group">
			<h2 class="group-head">Engagement</h2>
			<div class="surface-card panel">
				{#if obs.engagement.length === 0}
					<p class="muted small">No engagement signals recorded yet.</p>
				{:else}
					<div class="eng-table">
						<div class="eng-row eng-header">
							<span>Lane</span>
							<span class="num">Positive</span>
							<span class="num">Negative</span>
							<span class="num">Rate</span>
							<span class="num">Utility ×</span>
						</div>
						{#each obs.engagement as e (e.source_kind)}
							<div class="eng-row">
								<span class="badge">{labelize(e.source_kind)}</span>
								<span class="num">{compact(e.positive)}</span>
								<span class="num">{compact(e.negative)}</span>
								<span class="num">{(e.engagement_rate * 100).toFixed(0)}%</span>
								<span class="num strong">{e.utility_multiplier.toFixed(2)}×</span>
							</div>
						{/each}
					</div>
					<p class="muted small">
						Utility multiplier feeds back into scoring — lanes the user acts on are amplified,
						dismissed lanes damped.
					</p>
				{/if}
			</div>
		</section>
	{/if}
</div>

<style>
	.resurfacing-page {
		--rs-blue: var(--color-info, #3b82f6);
		--rs-green: var(--color-success, #00bb7f);
		--rs-red: var(--color-error, #ff6b6b);
		--rs-yellow: var(--color-warning, #d89a00);
		--rs-purple: var(--accent-secondary, #8b7ec8);
		--rs-coral: var(--accent-primary, #ff6b6b);
		--rs-line: color-mix(in srgb, var(--border-soft) 76%, transparent);
		--rs-panel: color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft));
		--rs-panel-strong: color-mix(in srgb, var(--bg-card) 88%, var(--accent-primary-soft));
		--rs-shadow: 0 16px 42px color-mix(in srgb, var(--text-primary) 7%, transparent);

		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.25rem 1.25rem 2.25rem;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		color: var(--text-primary);
		font-family: var(--font-primary);
		box-sizing: border-box;
		min-width: 0;
		overflow-x: hidden;
	}

	.rs-head {
		display: flex;
		align-items: flex-end;
		justify-content: space-between;
		gap: 1rem;
		min-width: 0;
	}

	.rs-title {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		min-width: 0;
	}

	.rs-kicker {
		margin: 0;
		color: var(--accent-primary);
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}

	.title-row {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.45rem 0.75rem;
		min-width: 0;
	}

	.rs-title h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: clamp(1.55rem, 2vw, 2rem);
		line-height: 1.05;
		color: var(--text-primary);
	}

	.rs-subtitle {
		margin: 0.15rem 0 0;
		color: var(--text-secondary);
		font-size: 0.92rem;
		max-width: 58rem;
	}

	.health-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		min-height: 1.55rem;
		padding: 0.12rem 0.55rem;
		border-radius: 999px;
		border: 1px solid var(--rs-line);
		background: color-mix(in srgb, var(--bg-soft) 62%, transparent);
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-weight: 600;
		align-self: center;
	}

	.health-dot {
		width: 0.45rem;
		height: 0.45rem;
		border-radius: 999px;
		background: var(--text-muted);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--text-muted) 12%, transparent);
	}

	.health-chip.tone-good .health-dot {
		background: var(--rs-green);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--rs-green) 16%, transparent);
	}

	.health-chip.tone-warn .health-dot {
		background: var(--rs-yellow);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--rs-yellow) 18%, transparent);
	}

	.health-chip.tone-bad .health-dot {
		background: var(--rs-red);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--rs-red) 16%, transparent);
	}

	.rs-actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.45rem;
		position: relative;
		z-index: 1;
	}

	:global(.action-button) {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.38rem;
		max-width: 100%;
		min-height: 2.1rem;
		padding: 0.42rem 0.82rem;
		border: 1px solid transparent;
		border-radius: 8px;
		font: inherit;
		font-size: var(--text-sm);
		font-weight: 700;
		line-height: 1.2;
		cursor: pointer;
		text-decoration: none;
		transition:
			background 0.15s ease,
			border-color 0.15s ease,
			color 0.15s ease,
			opacity 0.15s ease,
			transform 0.15s ease;
	}
	:global(.action-button):hover:not(:disabled) {
		transform: translateY(-1px);
	}
	:global(.action-button):disabled {
		cursor: default;
		opacity: 0.56;
	}
	:global(.action-button--outline) {
		background: transparent;
		border-color: var(--button-outline-border, var(--border-soft));
		color: var(--text-secondary);
	}
	:global(.action-button--outline:hover:not(:disabled)) {
		background: var(--button-outline-hover-bg, color-mix(in srgb, var(--accent-primary) 8%, transparent));
		color: var(--button-outline-hover-color, var(--text-primary));
	}
	:global(.action-button--sm) {
		min-height: 1.78rem;
		padding: 0.28rem 0.58rem;
		font-size: var(--text-2xs);
	}
	:global(.spinning) {
		animation: spin 900ms linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(-360deg);
		}
	}

	.muted {
		color: var(--text-muted);
	}

	.small {
		font-size: 0.78rem;
	}

	.mono {
		font-family: var(--font-mono);
		font-weight: 500;
	}

	/* Card surface — scoped, mirrors the observe/stats page idiom. */
	.surface-card {
		background: var(--rs-panel);
		border: 1px solid var(--rs-line);
		border-radius: 8px;
		box-shadow: var(--rs-shadow);
	}

	.panel {
		padding: 1rem 1.1rem;
		min-width: 0;
		box-sizing: border-box;
		overflow: hidden;
	}

	.panel h2,
	.panel h3 {
		margin: 0 0 0.6rem;
		font-size: 0.9rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.state-panel {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		column-gap: 0.75rem;
		align-items: start;
	}

	.state-panel h2,
	.state-panel p {
		grid-column: 2;
	}

	.state-icon {
		grid-row: 1 / span 2;
		width: 2.1rem;
		height: 2.1rem;
		border-radius: 8px;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		color: var(--rs-blue);
		background: color-mix(in srgb, var(--rs-blue) 12%, transparent);
		border: 1px solid color-mix(in srgb, var(--rs-blue) 24%, var(--border-soft));
	}

	.state-panel.danger .state-icon {
		color: var(--rs-red);
		background: color-mix(in srgb, var(--rs-red) 12%, transparent);
		border-color: color-mix(in srgb, var(--rs-red) 24%, var(--border-soft));
	}

	.panel-group {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.group-head {
		margin: 0;
		font-size: 1.05rem;
		font-weight: 600;
		color: var(--text-primary);
		letter-spacing: 0;
	}

	.summary-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(9.5rem, 1fr));
		gap: 0.75rem;
	}

	.summary-card {
		position: relative;
		overflow: hidden;
		min-height: 6.25rem;
		padding: 0.9rem 0.95rem;
		display: flex;
		flex-direction: column;
		justify-content: space-between;
		gap: 0.5rem;
		box-shadow: var(--shadow-sm, 0 1px 2px rgba(0, 0, 0, 0.04));
		transition: border-color 0.15s ease, transform 0.15s ease, box-shadow 0.15s ease;
	}

	.summary-card:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		transform: translateY(-1px);
	}

	.summary-card::before {
		content: '';
		position: absolute;
		inset: 0;
		border-top: 3px solid var(--rs-blue);
		pointer-events: none;
	}

	.summary-card-top {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}

	.summary-card-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.4rem;
		height: 1.4rem;
		border-radius: 5px;
		background: color-mix(in srgb, currentColor 10%, transparent);
		color: inherit;
		flex-shrink: 0;
	}

	.summary-card-label {
		color: var(--text-muted);
		font-size: 0.7rem;
		text-transform: uppercase;
		font-weight: 700;
		letter-spacing: 0.05em;
	}

	.summary-card strong {
		color: var(--text-primary);
		font-family: var(--font-mono, monospace);
		font-size: clamp(1.45rem, 2.5vw, 1.95rem);
		font-weight: 800;
		line-height: 1;
		font-variant-numeric: tabular-nums;
	}

	.summary-card em {
		width: 2.4rem;
		height: 0.28rem;
		border-radius: 999px;
		background: var(--rs-blue);
		opacity: 0.8;
	}

	.summary-card.tone-pending::before {
		border-top-color: var(--rs-yellow);
	}

	.summary-card.tone-pending em {
		background: var(--rs-yellow);
	}

	.summary-card.tone-pending .summary-card-icon {
		color: var(--rs-yellow);
		background: color-mix(in srgb, var(--rs-yellow) 14%, var(--bg-soft));
	}

	.summary-card.tone-pool::before {
		border-top-color: var(--rs-purple);
	}

	.summary-card.tone-pool em {
		background: var(--rs-purple);
	}

	.summary-card.tone-pool .summary-card-icon {
		color: var(--rs-purple);
		background: color-mix(in srgb, var(--rs-purple) 14%, var(--bg-soft));
	}

	.summary-card.tone-cooling::before {
		border-top-color: var(--rs-blue);
	}

	.summary-card.tone-cooling em {
		background: var(--rs-blue);
	}

	.summary-card.tone-cooling .summary-card-icon {
		color: var(--rs-blue);
		background: color-mix(in srgb, var(--rs-blue) 14%, var(--bg-soft));
	}

	.summary-card.tone-surfaced::before {
		border-top-color: var(--rs-coral);
	}

	.summary-card.tone-surfaced em {
		background: var(--rs-coral);
	}

	.summary-card.tone-surfaced .summary-card-icon {
		color: var(--rs-coral);
		background: color-mix(in srgb, var(--rs-coral) 14%, var(--bg-soft));
	}

	.summary-card.tone-runs::before {
		border-top-color: var(--rs-blue);
	}

	.summary-card.tone-runs em {
		background: var(--rs-blue);
	}

	.summary-card.tone-runs .summary-card-icon {
		color: var(--rs-blue);
		background: color-mix(in srgb, var(--rs-blue) 14%, var(--bg-soft));
	}

	.summary-card.tone-produced::before {
		border-top-color: var(--rs-green);
	}

	.summary-card.tone-produced em {
		background: var(--rs-green);
	}

	.summary-card.tone-produced .summary-card-icon {
		color: var(--rs-green);
		background: color-mix(in srgb, var(--rs-green) 14%, var(--bg-soft));
	}

	.summary-card.tone-signals::before {
		border-top-color: var(--rs-purple);
	}

	.summary-card.tone-signals em {
		background: var(--rs-purple);
	}

	.skeleton-card,
	.skeleton-panel {
		box-shadow: none;
	}

	.skeleton-card span,
	.skeleton-card strong,
	.skeleton-card em,
	.skeleton-panel span,
	.skeleton-panel strong,
	.skeleton-panel em {
		display: block;
		border-radius: 999px;
		background: linear-gradient(
			90deg,
			color-mix(in srgb, var(--bg-soft) 88%, transparent),
			color-mix(in srgb, var(--text-muted) 14%, transparent),
			color-mix(in srgb, var(--bg-soft) 88%, transparent)
		);
		background-size: 220% 100%;
		animation: shimmer 1.25s ease-in-out infinite;
	}

	.skeleton-card span {
		width: 4.5rem;
		height: 0.65rem;
	}

	.skeleton-card strong {
		width: 5.5rem;
		height: 1.8rem;
	}

	.skeleton-card em {
		width: 2.4rem;
		height: 0.28rem;
	}

	.skeleton-panel span {
		width: 60%;
		height: 0.8rem;
	}

	.skeleton-panel strong {
		width: 82%;
		height: 2rem;
	}

	.skeleton-panel em {
		width: 45%;
		height: 0.65rem;
	}

	@keyframes shimmer {
		to {
			background-position: -220% 0;
		}
	}

	.two-col {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 20rem), 1fr));
		gap: 0.8rem;
	}

	/* ── Pipeline cards ── */
	.pipeline-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 15rem), 1fr));
		gap: 0.8rem;
	}

	.pipeline-card {
		padding: 0.9rem 1rem;
		display: flex;
		flex-direction: column;
		gap: 0.65rem;
		box-shadow: none;
		transition:
			border-color 120ms ease,
			background 120ms ease,
			transform 120ms ease;
	}

	.pipeline-card:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 24%, var(--border-soft));
		background: var(--rs-panel-strong);
		transform: translateY(-1px);
	}

	.pipeline-card.has-failures {
		border-color: color-mix(in srgb, var(--rs-red) 45%, var(--border-soft));
	}

	.pipeline-card-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.pipeline-kind {
		font-size: 0.95rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.pipeline-when {
		font-size: 0.74rem;
		color: var(--text-muted);
	}

	.pipeline-metrics {
		display: flex;
		gap: 1.1rem;
	}

	.pm {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
	}

	.pm-value {
		font-size: 1.05rem;
		font-weight: 700;
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.pm-label {
		font-size: 0.68rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}

	.pipeline-status {
		font-size: 0.78rem;
		color: var(--text-secondary);
	}

	.pipeline-status.bad {
		color: var(--rs-red);
		font-weight: 600;
	}

	.pipeline-error {
		display: flex;
		align-items: flex-start;
		gap: 0.35rem;
		font-size: 0.74rem;
		color: var(--text-muted);
		border-top: 1px solid var(--rs-line);
		padding-top: 0.5rem;
	}

	.err-dot {
		color: var(--rs-red);
		line-height: 1.4;
	}

	.err-text {
		overflow: hidden;
		text-overflow: ellipsis;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}

	/* ── Funnel ── */
		.headline-row {
			display: flex;
			flex-wrap: wrap;
			gap: 1.5rem;
			margin-bottom: 0.75rem;
		}

	.headline {
		display: flex;
		flex-direction: column;
		gap: 0.1rem;
	}

	.headline-value {
		font-size: 1.4rem;
		font-weight: 700;
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.headline-label {
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}

	.state-bars {
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.state-row {
		display: grid;
		gap: 0.35rem;
	}

	.state-label {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		font-size: 0.85rem;
		color: var(--text-secondary);
	}

	.state-label strong {
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.bar-track {
		height: 0.48rem;
		border-radius: 999px;
		background: color-mix(in srgb, var(--bg-soft) 82%, transparent);
		border: 1px solid var(--rs-line);
		overflow: hidden;
	}

	.bar-fill {
		height: 100%;
		min-width: 2px;
		border-radius: 999px;
		background: var(--rs-blue);
		transition: width 240ms ease;
	}

	.bar-fill.state-candidate {
		background: var(--rs-yellow);
	}

	.bar-fill.state-surfaced {
		background: var(--rs-coral);
	}

	.bar-fill.state-acted {
		background: var(--rs-green);
	}

	.bar-fill.state-dismissed {
		background: var(--rs-red);
	}

	.bar-fill.state-snoozed {
		background: var(--rs-purple);
	}

	.bar-fill.lane-memory {
		background: var(--rs-green);
	}

	.bar-fill.lane-task {
		background: var(--rs-blue);
	}

	.bar-fill.lane-comm {
		background: var(--rs-coral);
	}

	.kv {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.3rem 0;
		font-size: 0.9rem;
		color: var(--text-secondary);
	}

	.kv strong {
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.badge {
		display: inline-flex;
		align-items: center;
		min-height: 1.45rem;
		padding: 0.12rem 0.52rem;
		border-radius: 999px;
		border: 1px solid var(--rs-line);
		background: color-mix(in srgb, var(--bg-soft) 74%, transparent);
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 650;
		white-space: nowrap;
	}

	.badge.lane-memory {
		color: color-mix(in srgb, var(--rs-green) 82%, var(--text-primary));
		background: color-mix(in srgb, var(--rs-green) 12%, transparent);
		border-color: color-mix(in srgb, var(--rs-green) 26%, var(--border-soft));
	}

	.badge.lane-task {
		color: color-mix(in srgb, var(--rs-blue) 82%, var(--text-primary));
		background: color-mix(in srgb, var(--rs-blue) 12%, transparent);
		border-color: color-mix(in srgb, var(--rs-blue) 26%, var(--border-soft));
	}

	.badge.lane-comm {
		color: color-mix(in srgb, var(--rs-coral) 82%, var(--text-primary));
		background: color-mix(in srgb, var(--rs-coral) 12%, transparent);
		border-color: color-mix(in srgb, var(--rs-coral) 26%, var(--border-soft));
	}

	/* ── Tables (recent runs + engagement) ── */
	.run-table,
	.eng-table {
		display: flex;
		flex-direction: column;
		overflow-x: auto;
		scrollbar-width: thin;
		-webkit-overflow-scrolling: touch;
		padding-bottom: 0.1rem;
	}

	.run-row {
		display: grid;
		grid-template-columns: 1fr 1fr 0.8fr 0.8fr 1.6fr;
		gap: 0.5rem;
		align-items: center;
		padding: 0.4rem 0;
		font-size: 0.85rem;
		border-top: 1px solid var(--rs-line);
		min-width: 42rem;
	}

	.eng-row {
		display: grid;
		grid-template-columns: 1.4fr 1fr 1fr 0.8fr 0.9fr;
		gap: 0.5rem;
		align-items: center;
		padding: 0.4rem 0;
		font-size: 0.85rem;
		border-top: 1px solid var(--rs-line);
		min-width: 34rem;
	}

	.run-header,
	.eng-header {
		border-top: 0;
		font-size: 0.72rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--text-muted);
	}

	.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}

	.strong {
		font-weight: 700;
		color: var(--text-primary);
	}

	.run-kind {
		color: var(--text-primary);
		font-weight: 500;
	}

	.run-status {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		min-width: 0;
	}

	.run-err {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--text-muted);
		font-size: 0.76rem;
	}

	.pill {
		display: inline-flex;
		align-items: center;
		min-height: 1.35rem;
		padding: 0.05rem 0.46rem;
		border-radius: 999px;
		font-size: 0.7rem;
		font-weight: 600;
		white-space: nowrap;
	}

	.pill-ok {
		background: color-mix(in srgb, var(--rs-green) 16%, transparent);
		color: var(--rs-green);
	}

	.pill-bad {
		background: color-mix(in srgb, var(--rs-red) 16%, transparent);
		color: var(--rs-red);
	}

	@media (max-width: 768px) {
		.resurfacing-page {
			padding: 1rem 0.85rem 1.5rem;
			gap: 0.85rem;
		}

		.rs-head {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.75rem;
		}

		.rs-actions {
			justify-content: flex-start;
			width: 100%;
		}

		.summary-grid,
		.pipeline-grid,
		.two-col {
			grid-template-columns: 1fr;
		}

		.summary-card {
			min-height: 5.5rem;
		}

		.pipeline-metrics,
		.headline-row {
			flex-wrap: wrap;
			gap: 0.8rem 1.1rem;
		}

		.run-row,
		.eng-row {
			font-size: 0.8rem;
		}
	}
</style>
