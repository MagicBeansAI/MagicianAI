<!--
  /evals — the eval suite as one surface.

  Lanes are declared by `## eval:` annotations in the repo Makefile and served
  by `/v2/evals`. Four views over the one uniform run record:

    Overview   every lane: readiness, last status, duration, cost, and Run
    Lane       one lane's history, a duration/status trend strip, its report
    Cost       spend by lane over a date range
    Runs       chronological feed, filterable by lane

  Two things this page refuses to do, because they are the reasons it exists:

  1. **Unknown cost renders `—`, never `$0.00`.** A lane that spent money must
     not look free because a ledger query failed. The rule is enforced in
     `$lib/evals/format.ts` (and at the wire boundary in `api.ts`), never by a
     `?? 0` in this file. It extends upward: a range total that omits
     unknown-cost runs renders as a floor (`≥$1.20`), not as a fact.

  2. **Nothing silently disappears.** A malformed annotation still draws its
     lane, with the parse error inline; an annotation that bound to no target
     at all is a banner at the top of the page, not a footnote — an eval
     missing from this grid is indistinguishable from an eval that was never
     written, which is the failure mode the whole feature is against.

  Runs go through the ordinary task system, so the Run button starts an
  execution and returns; progress and cancellation live where they already do.
  A lane that is not ready, or whose kind did not parse, cannot be started at
  all — the check happens before the money is spent, not after.
-->
<script lang="ts">
	import LifecycleRunOptions from '$lib/evals/LifecycleRunOptions.svelte';
	import type { EvalRunOptions } from '$lib/evals/api';
	import { onMount } from 'svelte';

	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import {
		listEvalLanes,
		listEvalRuns,
		runEvalLane,
		type EvalCost,
		type EvalKind,
		type EvalLane,
		type EvalRun,
		type EvalRunStarted,
		type OrphanedAnnotation
	} from '$lib/evals/api';
	import {
		aggregateCost,
		aggregateCostTitle,
		formatAggregateCost,
		formatCost,
		formatDuration,
		formatRequirement,
		formatRunStatus,
		formatTimestamp,
		runDisabledReason,
		runStatusTone,
		trendBarHeightPercent,
		UNKNOWN,
		type CostAggregate,
		type EvalTone
	} from '$lib/evals/format';

	type EvalsTab = 'overview' | 'lane' | 'cost' | 'runs';

	const TABS: Array<{ id: EvalsTab; label: string; hint: string }> = [
		{ id: 'overview', label: 'Overview', hint: 'Every lane: readiness, last outcome, cost' },
		{ id: 'lane', label: 'Lane', hint: 'One lane: history and trend' },
		{ id: 'cost', label: 'Cost', hint: 'Spend by lane over a date range' },
		{ id: 'runs', label: 'Runs', hint: 'Every run, newest first' }
	];

	const LANE_PAGE_SIZE = 20;
	const RUN_PAGE_SIZE = 25;
	const COST_ROW_PAGE_SIZE = 20;
	/** One page of the run feed while scanning a cost range. */
	const COST_SCAN_PAGE = 200;
	/** Hard ceiling on a cost scan; anything past it is reported, not hidden. */
	const COST_SCAN_CAP = 1_000;
	const DAY_MS = 86_400_000;
	/** How many runs the trend strip draws. */
	const TREND_LIMIT = 24;

	let activeTab: EvalsTab = 'overview';

	// ── Lanes ────────────────────────────────────────────────────────────────

	let lanes: EvalLane[] = [];
	let orphaned: OrphanedAnnotation[] = [];
	let lanesLoading = true;
	let lanesError: string | null = null;
	let startingLaneId: string | null = null;
	let lifecycleOptions: EvalRunOptions = { profiles: [], repeats: 3, partition: 'all' };
	let lifecycleOptionsValid = true;
	/** Lane id → the execution the last Run press created, for the row hint. */
	let startedRuns: Record<string, EvalRunStarted> = {};

	async function loadLanes(): Promise<void> {
		lanesLoading = lanes.length === 0;
		lanesError = null;
		try {
			const response = await listEvalLanes();
			lanes = response.lanes;
			orphaned = response.orphaned;
		} catch (err) {
			lanesError = messageOf(err);
		} finally {
			lanesLoading = false;
		}
	}

	async function refreshAll(): Promise<void> {
		await loadLanes();
		if (activeTab === 'runs') await loadFeed(feedPage);
		if (activeTab === 'cost') await loadCost();
		if (activeTab === 'lane' && selectedLaneId) await loadLaneRuns(laneRunsPage);
	}

	/**
	 * Starts a lane. Readiness is re-checked here as well as on the button so a
	 * stale grid cannot spend money: the server rejects it too (409), and that
	 * body is what the operator sees.
	 */
	async function startRun(lane: EvalLane): Promise<void> {
		if (startingLaneId) return;
		const blocked = runDisabledReason(lane);
		if (blocked) {
			showError(`Cannot run ${lane.id}`, blocked);
			return;
		}

		if (lane.run_options === 'memory_lifecycle' && !lifecycleOptionsValid) {
			showError('Cannot start lifecycle evaluation', 'Select at most three profiles.');
			return;
		}
		startingLaneId = lane.id;
		try {
			const started = await runEvalLane(lane.id, lane.run_options === 'memory_lifecycle' ? lifecycleOptions : {});
			startedRuns = { ...startedRuns, [lane.id]: started };
			showSuccess(`Started ${lane.id}`, `Task ${started.task_id}`);
			await loadLanes();
		} catch (err) {
			// The 409 body names the in-flight task, or the services that are
			// down. Show it verbatim — paraphrasing it loses the only useful part.
			showError(`Could not start ${lane.id}`, messageOf(err));
		} finally {
			startingLaneId = null;
		}
	}

	// ── Overview (client-paged over the one lanes response) ──────────────────

	let overviewPage = 1;

	$: overviewPageCount = Math.max(1, Math.ceil(lanes.length / LANE_PAGE_SIZE));
	$: overviewCurrent = Math.min(Math.max(1, overviewPage), overviewPageCount);
	$: overviewRows = lanes.slice(
		(overviewCurrent - 1) * LANE_PAGE_SIZE,
		overviewCurrent * LANE_PAGE_SIZE
	);
	$: overviewStart = lanes.length === 0 ? 0 : (overviewCurrent - 1) * LANE_PAGE_SIZE + 1;
	$: overviewEnd = Math.min(lanes.length, overviewCurrent * LANE_PAGE_SIZE);

	$: readyCount = lanes.filter((lane) => lane.readiness.ready && lane.kind !== 'unknown').length;
	$: unparseableCount = lanes.filter((lane) => !!lane.parse_error || lane.kind === 'unknown').length;
	$: failingCount = lanes.filter((lane) => lane.last_run?.status === 'failed').length;
	$: neverRunCount = lanes.filter((lane) => !lane.last_run).length;

	// ── Lane detail ──────────────────────────────────────────────────────────

	let selectedLaneId = '';
	let laneRuns: EvalRun[] = [];
	let laneRunsTotal = 0;
	let laneRunsPage = 1;
	let laneRunsLoading = false;
	let laneRunsError: string | null = null;
	let laneRequest = 0;

	$: selectedLane = lanes.find((lane) => lane.id === selectedLaneId) ?? null;
	$: laneRunsPageCount = Math.max(1, Math.ceil(laneRunsTotal / RUN_PAGE_SIZE));
	$: laneRunsStart = laneRunsTotal === 0 ? 0 : (laneRunsPage - 1) * RUN_PAGE_SIZE + 1;
	$: laneRunsEnd = Math.min(laneRunsTotal, (laneRunsPage - 1) * RUN_PAGE_SIZE + laneRuns.length);

	/** Oldest → newest, so the strip reads left to right like a timeline. */
	$: trendRuns = laneRuns.slice(0, TREND_LIMIT).reverse();
	$: trendMax = trendRuns.reduce(
		(max, run) => (Number.isFinite(run.duration_ms) ? Math.max(max, run.duration_ms) : max),
		0
	);

	async function loadLaneRuns(target: number): Promise<void> {
		if (!selectedLaneId) {
			laneRuns = [];
			laneRunsTotal = 0;
			return;
		}
		const request = ++laneRequest;
		const page = Math.max(1, Math.floor(target));
		laneRunsLoading = true;
		laneRunsError = null;
		try {
			const response = await listEvalRuns({
				lane: selectedLaneId,
				limit: RUN_PAGE_SIZE,
				offset: (page - 1) * RUN_PAGE_SIZE
			});
			if (request !== laneRequest) return;
			laneRuns = response.runs;
			laneRunsTotal = response.total;
			laneRunsPage = page;
		} catch (err) {
			if (request !== laneRequest) return;
			laneRunsError = messageOf(err);
			laneRuns = [];
			laneRunsTotal = 0;
		} finally {
			if (request === laneRequest) laneRunsLoading = false;
		}
	}

	function openLane(laneId: string): void {
		selectedLaneId = laneId;
		activeTab = 'lane';
		laneRunsPage = 1;
		void loadLaneRuns(1);
	}

	// ── Run feed ─────────────────────────────────────────────────────────────

	let feedRuns: EvalRun[] = [];
	let feedTotal = 0;
	let feedPage = 1;
	let feedLoading = false;
	let feedError: string | null = null;
	let feedLoaded = false;
	let feedLaneFilter = '';
	let feedRequest = 0;

	$: feedPageCount = Math.max(1, Math.ceil(feedTotal / RUN_PAGE_SIZE));
	$: feedStart = feedTotal === 0 ? 0 : (feedPage - 1) * RUN_PAGE_SIZE + 1;
	$: feedEnd = Math.min(feedTotal, (feedPage - 1) * RUN_PAGE_SIZE + feedRuns.length);

	async function loadFeed(target: number): Promise<void> {
		const request = ++feedRequest;
		const page = Math.max(1, Math.floor(target));
		feedLoading = true;
		feedError = null;
		try {
			const response = await listEvalRuns({
				lane: feedLaneFilter || null,
				limit: RUN_PAGE_SIZE,
				offset: (page - 1) * RUN_PAGE_SIZE
			});
			if (request !== feedRequest) return;
			feedRuns = response.runs;
			feedTotal = response.total;
			feedPage = page;
			feedLoaded = true;
		} catch (err) {
			if (request !== feedRequest) return;
			feedError = messageOf(err);
			feedRuns = [];
			feedTotal = 0;
		} finally {
			if (request === feedRequest) feedLoading = false;
		}
	}

	// ── Cost ─────────────────────────────────────────────────────────────────

	interface CostRow {
		laneId: string;
		runs: number;
		aggregate: CostAggregate;
		lastRunMs: number;
	}

	let costFrom = '';
	let costTo = '';
	let costRows: CostRow[] = [];
	let costTotals: CostAggregate = { usd: 0, knownCount: 0, unknownCount: 0 };
	let costScanned = 0;
	let costTotalInRange = 0;
	let costTruncated = false;
	let costLoading = false;
	let costLoaded = false;
	let costError: string | null = null;
	let costPage = 1;
	let costRequest = 0;

	$: costPageCount = Math.max(1, Math.ceil(costRows.length / COST_ROW_PAGE_SIZE));
	$: costCurrent = Math.min(Math.max(1, costPage), costPageCount);
	$: costPageRows = costRows.slice(
		(costCurrent - 1) * COST_ROW_PAGE_SIZE,
		costCurrent * COST_ROW_PAGE_SIZE
	);
	$: costStart = costRows.length === 0 ? 0 : (costCurrent - 1) * COST_ROW_PAGE_SIZE + 1;
	$: costEnd = Math.min(costRows.length, costCurrent * COST_ROW_PAGE_SIZE);

	/**
	 * Scans the run feed over the range and folds it by lane.
	 *
	 * The scan is bounded (`COST_SCAN_CAP`). When the range holds more than
	 * that, `costTruncated` says so on the page rather than letting the totals
	 * quietly describe a subset — an under-reported spend total is the same
	 * class of lie as rendering an unknown cost as zero.
	 */
	async function loadCost(): Promise<void> {
		const fromMs = dayStartMs(costFrom);
		const toMs = dayEndMs(costTo);
		if (fromMs !== null && toMs !== null && fromMs > toMs) {
			costError = 'The start of the range is after its end.';
			return;
		}

		const request = ++costRequest;
		costLoading = true;
		costError = null;
		try {
			const collected: EvalRun[] = [];
			let total = 0;
			const maxPages = Math.ceil(COST_SCAN_CAP / COST_SCAN_PAGE);
			for (let index = 0; index < maxPages; index += 1) {
				const response = await listEvalRuns({
					lane: null,
					fromMs,
					toMs,
					limit: COST_SCAN_PAGE,
					offset: collected.length
				});
				if (request !== costRequest) return;
				total = response.total;
				if (response.runs.length === 0) break;
				collected.push(...response.runs);
				if (collected.length >= total) break;
			}

			const byLane = new Map<string, { costs: EvalCost[]; lastRunMs: number }>();
			for (const run of collected) {
				const entry = byLane.get(run.lane_id) ?? { costs: [], lastRunMs: 0 };
				entry.costs.push(run.cost);
				entry.lastRunMs = Math.max(entry.lastRunMs, run.started_at_ms || 0);
				byLane.set(run.lane_id, entry);
			}

			costRows = [...byLane.entries()]
				.map(([laneId, entry]) => ({
					laneId,
					runs: entry.costs.length,
					aggregate: aggregateCost(entry.costs),
					lastRunMs: entry.lastRunMs
				}))
				.sort(
					(a, b) =>
						b.aggregate.usd - a.aggregate.usd ||
						b.aggregate.unknownCount - a.aggregate.unknownCount ||
						a.laneId.localeCompare(b.laneId)
				);
			costTotals = aggregateCost(collected.map((run) => run.cost));
			costScanned = collected.length;
			costTotalInRange = Math.max(total, collected.length);
			// Only claim truncation when something was actually scanned; a server
			// reporting a total it then serves no rows for is a different fault.
			costTruncated = collected.length > 0 && collected.length < costTotalInRange;
			costPage = 1;
			costLoaded = true;
		} catch (err) {
			if (request !== costRequest) return;
			costError = messageOf(err);
			costRows = [];
			costTotals = { usd: 0, knownCount: 0, unknownCount: 0 };
			costScanned = 0;
			costTotalInRange = 0;
			costTruncated = false;
		} finally {
			if (request === costRequest) costLoading = false;
		}
	}

	// ── Helpers ──────────────────────────────────────────────────────────────

	function messageOf(err: unknown): string {
		return err instanceof Error ? err.message : String(err);
	}

	function kindTone(kind: EvalKind): EvalTone {
		if (kind === 'live') return 'warning';
		if (kind === 'harness') return 'info';
		return 'error';
	}

	function laneCost(lane: EvalLane): EvalCost | null {
		return lane.last_run?.cost ?? null;
	}

	function relative(epochMs: number | null | undefined): string {
		if (epochMs === null || epochMs === undefined || !Number.isFinite(epochMs) || epochMs <= 0) {
			return UNKNOWN;
		}
		return formatRelativeTime(epochMs) || UNKNOWN;
	}

	function isMissing(lane: EvalLane, requirement: string): boolean {
		return lane.readiness.missing.includes(requirement);
	}

	function dayStartMs(value: string): number | null {
		if (!value) return null;
		const parsed = Date.parse(`${value}T00:00:00`);
		return Number.isFinite(parsed) ? parsed : null;
	}

	function dayEndMs(value: string): number | null {
		const start = dayStartMs(value);
		return start === null ? null : start + DAY_MS - 1;
	}

	function isoDay(epochMs: number): string {
		const date = new Date(epochMs);
		const pad = (value: number) => String(value).padStart(2, '0');
		return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
	}

	function trendTitle(run: EvalRun): string {
		return `${formatRunStatus(run.status)} · ${formatDuration(run.duration_ms)} · ${formatCost(
			run.cost
		)} · ${formatTimestamp(run.started_at_ms)}`;
	}

	function openTab(tab: EvalsTab): void {
		activeTab = tab;
		if (tab === 'runs' && !feedLoaded && !feedLoading) void loadFeed(1);
		if (tab === 'cost' && !costLoaded && !costLoading) void loadCost();
		if (tab === 'lane' && selectedLaneId && laneRuns.length === 0 && !laneRunsLoading) {
			void loadLaneRuns(laneRunsPage);
		}
	}

	onMount(() => {
		const now = Date.now();
		costFrom = isoDay(now - 30 * DAY_MS);
		costTo = isoDay(now);

		// `#cost`, `#runs`, `#lane=<id>` deep-link a view, matching /llm's
		// hash convention.
		const hash = window.location.hash.replace(/^#/, '');
		if (hash.startsWith('lane=')) {
			selectedLaneId = decodeURIComponent(hash.slice('lane='.length));
			activeTab = 'lane';
		} else if (hash === 'cost' || hash === 'runs' || hash === 'overview') {
			activeTab = hash;
		}

		void loadLanes().then(() => openTab(activeTab));
	});
</script>

<svelte:head>
	<title>Evals · Magican</title>
</svelte:head>

<div class="ev-shell">
	<header class="ev-header">
		<div class="ev-heading">
			<h1>Evals</h1>
			<p>
				Every lane declared by a <code>## eval:</code> annotation in the Makefile — what exists,
				whether it passed, what it cost, and whether it is drifting.
			</p>
		</div>
		<div class="ev-header-actions">
			<Button
				variant="secondary"
				size="sm"
				label="Refresh"
				interactive={!lanesLoading}
				on:click={() => void refreshAll()}
			/>
		</div>
	</header>

	<!--
		Orphaned annotations first, and loud. Each one is an eval that declared
		itself and then bound to nothing — it is absent from every list below,
		and an absent eval looks exactly like an eval nobody wrote. This banner
		is the only thing standing between those two states.
	-->
	{#if orphaned.length > 0}
		<section class="ev-orphans" role="alert" aria-label="Eval annotations that bound to no target">
			<h2>
				{orphaned.length} eval annotation{orphaned.length === 1 ? '' : 's'} bound to no target
			</h2>
			<p>
				These declared an eval and then attached to nothing, so the lane is missing from this page
				entirely. Fix the annotation in the Makefile at the line given.
			</p>
			<ul>
				{#each orphaned as entry (entry.line + ':' + entry.text)}
					<li>
						<span class="ev-orphan-line">Makefile:{entry.line}</span>
						<code>{entry.text}</code>
						<span class="ev-orphan-reason">{entry.reason}</span>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	<div class="ev-tabs" role="tablist" aria-label="Evals views">
		{#each TABS as tab (tab.id)}
			<button
				type="button"
				role="tab"
				class="ev-tab"
				class:ev-tab--active={activeTab === tab.id}
				aria-selected={activeTab === tab.id}
				title={tab.hint}
				on:click={() => openTab(tab.id)}
			>
				{tab.label}
			</button>
		{/each}
	</div>

	{#if lanesError}
		<div class="ev-error" role="alert">
			<span>Couldn't load eval lanes: {lanesError}</span>
			<Button variant="outline" size="sm" label="Retry" on:click={() => void loadLanes()} />
		</div>
	{/if}

	{#if activeTab === 'overview'}
		{#if lanesLoading}
			<div class="ev-loading"><Spinner size="md" label="Loading lanes…" centered /></div>
		{:else if lanes.length === 0 && !lanesError}
			<EmptyState
				icon="✓"
				title="No eval lanes"
				description="Nothing in the Makefile carries a `## eval:` annotation yet."
			/>
		{:else if lanes.length > 0}
			<section class="ev-panel" aria-label="Eval lanes">
				<div class="ev-toolbar">
					<p class="ev-count">
						<strong>{lanes.length}</strong> lane{lanes.length === 1 ? '' : 's'}
						<span>{readyCount} ready</span>
						{#if failingCount > 0}
							<span class="ev-count-bad">{failingCount} failing</span>
						{/if}
						{#if unparseableCount > 0}
							<span class="ev-count-bad">{unparseableCount} unparseable</span>
						{/if}
						{#if neverRunCount > 0}
							<span>{neverRunCount} never run</span>
						{/if}
					</p>
				</div>

				{#if overviewPageCount > 1}
					<div class="ev-pager ev-pager--top">
						<ServerPager
							currentPage={overviewCurrent}
							pageCount={overviewPageCount}
							startItem={overviewStart}
							endItem={overviewEnd}
							totalItems={lanes.length}
							ariaLabel="Eval lanes (top)"
							on:pagechange={(event) => (overviewPage = event.detail.page)}
						/>
					</div>
				{/if}

				<div class="ev-table-wrap">
					<table class="ev-table">
						<thead>
							<tr>
								<th scope="col">Lane</th>
								<th scope="col">Kind</th>
								<th scope="col">Requires</th>
								<th scope="col">Last run</th>
								<th scope="col">Duration</th>
								<th scope="col" class="ev-num">Cost</th>
								<th scope="col"><span class="ev-sr-only">Actions</span></th>
							</tr>
						</thead>
						<tbody>
							{#each overviewRows as lane (lane.id)}
								{@const blocked = runDisabledReason(lane)}
								<tr class:ev-row--unparseable={!!lane.parse_error || lane.kind === 'unknown'}>
									<td class="ev-cell-lane">
										<button type="button" class="ev-lane-link" on:click={() => openLane(lane.id)}>
											{lane.id}
										</button>
										{#if lane.desc}
											<span class="ev-lane-desc">{lane.desc}</span>
										{/if}
										<span class="ev-lane-target" title={`Declared at Makefile:${lane.line}`}>
											make {lane.target}
										</span>
										<!--
											Rendered inline, never used to hide the row: a lane
											with a broken annotation is a lane someone has to fix,
											and thinning it out of the grid hides the defect.
										-->
										{#if lane.parse_error}
											<p class="ev-parse-error">
												<strong>Malformed annotation</strong> (Makefile:{lane.line}) —
												{lane.parse_error}
											</p>
										{/if}
										{#if startedRuns[lane.id]}
											<p class="ev-started">
												Started · task <code>{startedRuns[lane.id].task_id}</code>
											</p>
										{/if}
									</td>
									<td><Badge text={lane.kind} color={kindTone(lane.kind)} /></td>
									<td class="ev-cell-requires">
										{#if lane.requires.length === 0}
											<span class="ev-muted">none</span>
										{:else}
											<ul class="ev-req-list">
												{#each lane.requires as requirement (requirement)}
													<li
														class="ev-req"
														class:ev-req--missing={isMissing(lane, requirement)}
														title={isMissing(lane, requirement)
															? `${formatRequirement(requirement)} is not available`
															: `${formatRequirement(requirement)} is available`}
													>
														{formatRequirement(requirement)}
													</li>
												{/each}
											</ul>
										{/if}
									</td>
									<td class="ev-cell-last">
										{#if lane.last_run}
											<Badge
												text={formatRunStatus(lane.last_run.status)}
												color={runStatusTone(lane.last_run.status)}
											/>
											<span
												class="ev-when"
												title={formatTimestamp(lane.last_run.started_at_ms)}
											>
												{relative(lane.last_run.started_at_ms)}
											</span>
										{:else}
											<span class="ev-muted">Never run</span>
										{/if}
									</td>
									<td class="ev-num">{formatDuration(lane.last_run?.duration_ms ?? null)}</td>
									<td class="ev-num ev-cost">{formatCost(laneCost(lane))}</td>
									<td class="ev-cell-actions">
										<!-- The title lives on the wrapper too: a disabled button
										     shows no tooltip in most browsers, and the reason is
										     the whole point of the disabled state. -->
										<span class="ev-run-wrap" title={blocked ?? `Run make ${lane.target}`}>
											<button
												type="button"
												class="ev-run"
												disabled={!!blocked || startingLaneId === lane.id}
												aria-label={blocked
													? `Cannot run ${lane.id}: ${blocked}`
													: `Run ${lane.id}`}
												on:click={() => void startRun(lane)}
											>
												{startingLaneId === lane.id ? 'Starting…' : 'Run'}
											</button>
										</span>
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>

				{#if overviewPageCount > 1}
					<div class="ev-pager">
						<ServerPager
							currentPage={overviewCurrent}
							pageCount={overviewPageCount}
							startItem={overviewStart}
							endItem={overviewEnd}
							totalItems={lanes.length}
							ariaLabel="Eval lanes (bottom)"
							on:pagechange={(event) => (overviewPage = event.detail.page)}
						/>
					</div>
				{/if}
			</section>
		{/if}
	{/if}

	{#if activeTab === 'lane'}
		<section class="ev-panel" aria-label="Lane detail">
			<div class="ev-toolbar">
				<label class="ev-field">
					<span>Lane</span>
					<select
						bind:value={selectedLaneId}
						on:change={() => {
							laneRunsPage = 1;
							void loadLaneRuns(1);
						}}
					>
						<option value="">Select a lane…</option>
						{#each lanes as lane (lane.id)}
							<option value={lane.id}>{lane.id}</option>
						{/each}
					</select>
				</label>
				{#if selectedLane}
					<p class="ev-count">
						<strong>{laneRunsTotal}</strong> recorded run{laneRunsTotal === 1 ? '' : 's'}
						<span>Newest first</span>
					</p>
				{/if}
			</div>

			<!-- Truthy-first so `selectedLane` narrows to a lane inside the branch. -->
			{#if selectedLane}
				{@const blocked = runDisabledReason(selectedLane)}
				<div class="ev-lane-head">
					<div class="ev-lane-meta">
						<h2>{selectedLane.id}</h2>
						{#if selectedLane.desc}<p>{selectedLane.desc}</p>{/if}
						<dl class="ev-meta-grid">
							<div><dt>Target</dt><dd><code>make {selectedLane.target}</code></dd></div>
							<div>
								<dt>Kind</dt>
								<dd><Badge text={selectedLane.kind} color={kindTone(selectedLane.kind)} /></dd>
							</div>
							<div>
								<dt>Requires</dt>
								<dd>
									{selectedLane.requires.length === 0
										? 'none'
										: selectedLane.requires.map(formatRequirement).join(', ')}
								</dd>
							</div>
							<div>
								<dt>Readiness</dt>
								<dd>
									{selectedLane.readiness.ready
										? 'Ready'
										: `Not ready — missing ${
												selectedLane.readiness.missing.map(formatRequirement).join(', ') ||
												'(unreported)'
											}`}
								</dd>
							</div>
							<div>
								<dt>Report directory</dt>
								<dd>
									{#if selectedLane.report_dir}
										<code>{selectedLane.report_dir}</code>
									{:else}
										<span class="ev-muted">this lane writes no report</span>
									{/if}
								</dd>
							</div>
							<div><dt>Declared</dt><dd>Makefile:{selectedLane.line}</dd></div>
						</dl>
						{#if selectedLane.run_options === 'memory_lifecycle'}
							<LifecycleRunOptions bind:value={lifecycleOptions} bind:valid={lifecycleOptionsValid} />
						{/if}
						{#if selectedLane.parse_error}
							<p class="ev-parse-error">
								<strong>Malformed annotation</strong> (Makefile:{selectedLane.line}) —
								{selectedLane.parse_error}
							</p>
						{/if}
					</div>
					<span class="ev-run-wrap" title={blocked ?? `Run make ${selectedLane.target}`}>
						<button
							type="button"
							class="ev-run ev-run--lg"
							disabled={!!blocked || startingLaneId === selectedLane.id}
							on:click={() => selectedLane && void startRun(selectedLane)}
						>
							{startingLaneId === selectedLane.id ? 'Starting…' : 'Run lane'}
						</button>
					</span>
				</div>

				{#if laneRunsError}
					<div class="ev-error" role="alert">
						<span>Couldn't load runs: {laneRunsError}</span>
						<Button
							variant="outline"
							size="sm"
							label="Retry"
							on:click={() => void loadLaneRuns(laneRunsPage)}
						/>
					</div>
				{/if}

				{#if laneRunsLoading && laneRuns.length === 0}
					<div class="ev-loading"><Spinner size="md" label="Loading runs…" centered /></div>
				{:else if laneRuns.length === 0 && !laneRunsError}
					<EmptyState
						icon="◌"
						title="No runs recorded"
						description="This lane has not run since run recording began. History cannot be backfilled — run it once to start the trend."
					/>
				{:else if laneRuns.length > 0}
					<!-- Trend strip: bar height is duration against the slowest run on
					     this page, colour is the outcome. Deliberately not a chart —
					     the question it answers is "is this getting slower or redder". -->
					<div class="ev-trend" aria-label="Recent run trend">
						{#each trendRuns as run (run.run_id)}
							<!-- `class:` rather than an interpolated class name so the
							     compiler can see the tone classes are used. -->
							<span
								class="ev-trend-bar"
								class:ev-trend-bar--success={run.status === 'passed'}
								class:ev-trend-bar--error={run.status === 'failed'}
								class:ev-trend-bar--warning={run.status === 'interrupted'}
								style={`height:${trendBarHeightPercent(run.duration_ms, trendMax)}%`}
								title={trendTitle(run)}
							></span>
						{/each}
					</div>

					{#if laneRunsPageCount > 1}
						<div class="ev-pager ev-pager--top">
							<ServerPager
								currentPage={laneRunsPage}
								pageCount={laneRunsPageCount}
								startItem={laneRunsStart}
								endItem={laneRunsEnd}
								totalItems={laneRunsTotal}
								loading={laneRunsLoading}
								ariaLabel="Lane run history (top)"
								on:pagechange={(event) => void loadLaneRuns(event.detail.page)}
							/>
						</div>
					{/if}

					<div class="ev-table-wrap" class:ev-table-wrap--paging={laneRunsLoading}>
						<table class="ev-table">
							<thead>
								<tr>
									<th scope="col">Started</th>
									<th scope="col">Status</th>
									<th scope="col">Duration</th>
									<th scope="col" class="ev-num">Cost</th>
									<th scope="col" class="ev-num">Exit</th>
									<th scope="col">Report</th>
								</tr>
							</thead>
							<tbody>
								{#each laneRuns as run (run.run_id)}
									<tr>
										<td title={formatTimestamp(run.started_at_ms)}>
											{relative(run.started_at_ms)}
										</td>
										<td>
											<Badge
												text={formatRunStatus(run.status)}
												color={runStatusTone(run.status)}
											/>
										</td>
										<td class="ev-num">{formatDuration(run.duration_ms)}</td>
										<td class="ev-num ev-cost">{formatCost(run.cost)}</td>
										<td class="ev-num">{run.exit_code ?? UNKNOWN}</td>
										<td>
											{#if run.report_href}
												<a href={run.report_href} target="_blank" rel="noreferrer noopener">
													Open report
												</a>
											{:else}
												<span class="ev-muted">no report emitted</span>
											{/if}
										</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>

					{#if laneRunsPageCount > 1}
						<div class="ev-pager">
							<ServerPager
								currentPage={laneRunsPage}
								pageCount={laneRunsPageCount}
								startItem={laneRunsStart}
								endItem={laneRunsEnd}
								totalItems={laneRunsTotal}
								loading={laneRunsLoading}
								ariaLabel="Lane run history (bottom)"
								on:pagechange={(event) => void loadLaneRuns(event.detail.page)}
							/>
						</div>
					{/if}
				{/if}
			{:else}
				<EmptyState
					icon="◎"
					title="Pick a lane"
					description="Choose a lane above, or click one in the Overview grid, to see its history and trend."
				/>
			{/if}
		</section>
	{/if}

	{#if activeTab === 'cost'}
		<section class="ev-panel" aria-label="Spend by lane">
			<div class="ev-toolbar ev-toolbar--wrap">
				<label class="ev-field">
					<span>From</span>
					<input type="date" bind:value={costFrom} max={costTo} />
				</label>
				<label class="ev-field">
					<span>To</span>
					<input type="date" bind:value={costTo} min={costFrom} />
				</label>
				<Button
					variant="secondary"
					size="sm"
					label={costLoading ? 'Loading…' : 'Apply'}
					interactive={!costLoading}
					on:click={() => void loadCost()}
				/>
				<p class="ev-count">
					<strong title={aggregateCostTitle(costTotals)}>{formatAggregateCost(costTotals)}</strong>
					<span>across {costScanned} run{costScanned === 1 ? '' : 's'}</span>
					{#if costTotals.unknownCount > 0}
						<span class="ev-count-bad">{costTotals.unknownCount} of unknown cost</span>
					{/if}
				</p>
			</div>

			<!-- An under-reported total is the same lie as a fabricated zero, so
			     say when the scan did not reach the whole range. -->
			{#if costTruncated}
				<p class="ev-notice" role="status">
					Showing the most recent {costScanned} of {costTotalInRange} runs in this range — the totals
					below cover only those. Narrow the range for a complete figure.
				</p>
			{/if}

			{#if costError}
				<div class="ev-error" role="alert">
					<span>Couldn't load spend: {costError}</span>
					<Button variant="outline" size="sm" label="Retry" on:click={() => void loadCost()} />
				</div>
			{/if}

			{#if costLoading && costRows.length === 0}
				<div class="ev-loading"><Spinner size="md" label="Scanning runs…" centered /></div>
			{:else if costRows.length === 0 && !costError}
				<EmptyState
					icon="◇"
					title="No runs in this range"
					description="Nothing ran between these dates, so nothing was spent."
				/>
			{:else if costRows.length > 0}
				{#if costPageCount > 1}
					<div class="ev-pager ev-pager--top">
						<ServerPager
							currentPage={costCurrent}
							pageCount={costPageCount}
							startItem={costStart}
							endItem={costEnd}
							totalItems={costRows.length}
							loading={costLoading}
							ariaLabel="Spend by lane (top)"
							on:pagechange={(event) => (costPage = event.detail.page)}
						/>
					</div>
				{/if}

				<div class="ev-table-wrap" class:ev-table-wrap--paging={costLoading}>
					<table class="ev-table">
						<thead>
							<tr>
								<th scope="col">Lane</th>
								<th scope="col" class="ev-num">Runs</th>
								<th scope="col" class="ev-num">Spend</th>
								<th scope="col" class="ev-num">Unknown</th>
								<th scope="col">Last run</th>
							</tr>
						</thead>
						<tbody>
							{#each costPageRows as row (row.laneId)}
								<tr>
									<td class="ev-cell-lane">
										<button type="button" class="ev-lane-link" on:click={() => openLane(row.laneId)}>
											{row.laneId}
										</button>
									</td>
									<td class="ev-num">{row.runs}</td>
									<td class="ev-num ev-cost" title={aggregateCostTitle(row.aggregate)}>
										{formatAggregateCost(row.aggregate)}
									</td>
									<td class="ev-num">
										{#if row.aggregate.unknownCount > 0}
											<span class="ev-count-bad">{row.aggregate.unknownCount}</span>
										{:else}
											<span class="ev-muted">0</span>
										{/if}
									</td>
									<td title={formatTimestamp(row.lastRunMs)}>{relative(row.lastRunMs)}</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>

				{#if costPageCount > 1}
					<div class="ev-pager">
						<ServerPager
							currentPage={costCurrent}
							pageCount={costPageCount}
							startItem={costStart}
							endItem={costEnd}
							totalItems={costRows.length}
							loading={costLoading}
							ariaLabel="Spend by lane (bottom)"
							on:pagechange={(event) => (costPage = event.detail.page)}
						/>
					</div>
				{/if}
			{/if}
		</section>
	{/if}

	{#if activeTab === 'runs'}
		<section class="ev-panel" aria-label="Run feed">
			<div class="ev-toolbar">
				<label class="ev-field">
					<span>Lane</span>
					<select bind:value={feedLaneFilter} on:change={() => void loadFeed(1)}>
						<option value="">All lanes</option>
						{#each lanes as lane (lane.id)}
							<option value={lane.id}>{lane.id}</option>
						{/each}
					</select>
				</label>
				<p class="ev-count">
					<strong>{feedTotal}</strong> run{feedTotal === 1 ? '' : 's'}
					<span>Newest first</span>
				</p>
			</div>

			{#if feedError}
				<div class="ev-error" role="alert">
					<span>Couldn't load runs: {feedError}</span>
					<Button
						variant="outline"
						size="sm"
						label="Retry"
						on:click={() => void loadFeed(feedPage)}
					/>
				</div>
			{/if}

			{#if feedLoading && feedRuns.length === 0}
				<div class="ev-loading"><Spinner size="md" label="Loading runs…" centered /></div>
			{:else if feedRuns.length === 0 && !feedError}
				<EmptyState
					icon="◌"
					title="No runs yet"
					description="Runs recorded from the Overview grid, or from a `make` invocation the runner observed, appear here."
				/>
			{:else if feedRuns.length > 0}
				{#if feedPageCount > 1}
					<div class="ev-pager ev-pager--top">
						<ServerPager
							currentPage={feedPage}
							pageCount={feedPageCount}
							startItem={feedStart}
							endItem={feedEnd}
							totalItems={feedTotal}
							loading={feedLoading}
							ariaLabel="Eval runs (top)"
							on:pagechange={(event) => void loadFeed(event.detail.page)}
						/>
					</div>
				{/if}

				<div class="ev-table-wrap" class:ev-table-wrap--paging={feedLoading}>
					<table class="ev-table">
						<thead>
							<tr>
								<th scope="col">Started</th>
								<th scope="col">Lane</th>
								<th scope="col">Status</th>
								<th scope="col">Duration</th>
								<th scope="col" class="ev-num">Cost</th>
								<th scope="col">Services</th>
								<th scope="col">Report</th>
							</tr>
						</thead>
						<tbody>
							{#each feedRuns as run (run.run_id)}
								<tr>
									<td title={formatTimestamp(run.started_at_ms)}>{relative(run.started_at_ms)}</td>
									<td class="ev-cell-lane">
										<button type="button" class="ev-lane-link" on:click={() => openLane(run.lane_id)}>
											{run.lane_id}
										</button>
										{#if run.task_id}
											<span class="ev-lane-target" title="Execution that ran it">
												task {run.task_id}
											</span>
										{/if}
									</td>
									<td>
										<Badge text={formatRunStatus(run.status)} color={runStatusTone(run.status)} />
									</td>
									<td class="ev-num">{formatDuration(run.duration_ms)}</td>
									<td class="ev-num ev-cost">{formatCost(run.cost)}</td>
									<td class="ev-cell-requires">
										{#if run.services.length === 0}
											<span class="ev-muted">none</span>
										{:else}
											<ul class="ev-req-list">
												{#each run.services as service (service)}
													<li class="ev-req">{formatRequirement(service)}</li>
												{/each}
											</ul>
										{/if}
									</td>
									<td>
										{#if run.report_href}
											<a href={run.report_href} target="_blank" rel="noreferrer noopener">
												Open report
											</a>
										{:else}
											<span class="ev-muted">no report emitted</span>
										{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>

				{#if feedPageCount > 1}
					<div class="ev-pager">
						<ServerPager
							currentPage={feedPage}
							pageCount={feedPageCount}
							startItem={feedStart}
							endItem={feedEnd}
							totalItems={feedTotal}
							loading={feedLoading}
							ariaLabel="Eval runs (bottom)"
							on:pagechange={(event) => void loadFeed(event.detail.page)}
						/>
					</div>
				{/if}
			{/if}
		</section>
	{/if}
</div>

<style>
	.ev-shell {
		display: flex;
		flex-direction: column;
		gap: 1rem;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.1rem 1.45rem 2rem;
		box-sizing: border-box;
		min-height: 0;
		overflow-y: auto;
	}

	.ev-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
	}

	.ev-heading h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.4rem;
		font-weight: 700;
		letter-spacing: -0.01em;
		color: var(--text-primary);
	}

	.ev-heading p {
		margin: 0.3rem 0 0;
		max-width: 62ch;
		font-size: 0.85rem;
		line-height: 1.5;
		color: var(--text-secondary);
	}

	.ev-header-actions {
		display: flex;
		gap: 0.5rem;
		flex-shrink: 0;
	}

	/* Orphaned annotations. Toned as an error, not a hint: each one is an eval
	   that is invisible everywhere else on this page. */
	.ev-orphans {
		padding: 0.8rem 0.95rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 42%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
	}

	.ev-orphans h2 {
		margin: 0;
		color: var(--color-error, var(--status-failed));
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.92rem;
		font-weight: 700;
	}

	.ev-orphans p {
		margin: 0.3rem 0 0.55rem;
		color: var(--text-secondary);
		font-size: 0.8rem;
		line-height: 1.5;
	}

	.ev-orphans ul {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.ev-orphans li {
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 0.45rem;
		font-size: 0.78rem;
	}

	.ev-orphan-line {
		color: var(--text-muted);
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		white-space: nowrap;
	}

	.ev-orphans code {
		color: var(--text-primary);
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
		overflow-wrap: anywhere;
	}

	.ev-orphan-reason {
		color: var(--text-secondary);
	}

	.ev-tabs {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		width: fit-content;
		padding: 0.2rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--bg-soft) 72%, var(--bg-card));
	}

	.ev-tab {
		min-height: 1.85rem;
		padding: 0.3rem 0.72rem;
		border: 1px solid transparent;
		border-radius: var(--radius-sm, 6px);
		background: transparent;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.76rem;
		font-weight: 700;
		cursor: pointer;
	}

	.ev-tab:hover:not(:disabled) {
		color: var(--text-primary);
		background: color-mix(in srgb, var(--bg-card) 76%, transparent);
	}

	.ev-tab--active {
		border-color: var(--border-soft);
		background: var(--bg-card);
		color: var(--text-primary);
		box-shadow: var(--shadow-xs, 0 1px 2px rgb(0 0 0 / 0.06));
	}

	.ev-tab:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.ev-error {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.6rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-error, var(--status-failed)) 34%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--color-error, var(--status-failed));
		font-size: 0.82rem;
	}

	.ev-notice {
		margin: 0;
		padding: 0.5rem 0.85rem;
		border: 1px solid color-mix(in srgb, var(--color-warning, var(--status-paused)) 38%, transparent);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--color-warning, var(--status-paused)) 12%, transparent);
		color: var(--text-primary);
		font-size: 0.78rem;
		line-height: 1.5;
	}

	.ev-loading {
		padding: 2.5rem 0;
	}

	.ev-panel {
		display: flex;
		flex-direction: column;
		min-width: 0;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg, 12px);
		background: var(--bg-card);
		box-shadow: var(--shadow-sm);
		overflow: hidden;
	}

	.ev-toolbar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.85rem;
		padding: 0.65rem 0.75rem;
		border-bottom: 1px solid var(--border-soft);
		background: color-mix(in srgb, var(--bg-soft) 68%, var(--bg-card));
	}

	.ev-toolbar--wrap {
		flex-wrap: wrap;
		justify-content: flex-start;
	}

	.ev-count {
		display: flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 0.55rem;
		margin: 0;
		margin-left: auto;
		color: var(--text-secondary);
		font-size: 0.76rem;
	}

	.ev-count strong {
		color: var(--text-primary);
		font-variant-numeric: tabular-nums;
	}

	.ev-count span {
		color: var(--text-muted);
	}

	/* Doubled up so it also outranks `.ev-count span` inside a toolbar count. */
	.ev-count .ev-count-bad,
	.ev-count-bad {
		color: var(--color-error, var(--status-failed));
		font-weight: 700;
	}

	.ev-field {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--text-muted);
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.ev-field select,
	.ev-field input {
		min-height: 1.85rem;
		max-width: 18rem;
		padding: 0.25rem 0.48rem;
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: var(--radius-sm, 6px);
		background: var(--input-bg, var(--bg-card));
		color: var(--text-primary);
		font: inherit;
		font-variant-numeric: tabular-nums;
		text-transform: none;
		letter-spacing: normal;
	}

	.ev-field select:focus-visible,
	.ev-field input:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.ev-pager {
		padding: 0.4rem 0.75rem;
		border-top: 1px solid var(--border-soft);
	}

	.ev-pager--top {
		border-top: 0;
		border-bottom: 1px solid var(--border-soft);
	}

	.ev-table-wrap {
		width: 100%;
		overflow-x: auto;
		transition: opacity var(--transition-fast, 0.15s ease);
	}

	.ev-table-wrap--paging {
		opacity: 0.56;
		pointer-events: none;
	}

	.ev-table {
		width: 100%;
		min-width: 46rem;
		border-collapse: collapse;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-body, var(--text-primary));
	}

	.ev-table th,
	.ev-table td {
		padding: 0.65rem 0.8rem;
		border-bottom: 1px solid var(--border-soft);
		text-align: left;
		vertical-align: top;
	}

	.ev-table th {
		background: color-mix(in srgb, var(--bg-soft) 58%, var(--bg-card));
		color: var(--text-secondary);
		font-size: 0.7rem;
		font-weight: 750;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		white-space: nowrap;
	}

	.ev-table tbody tr:last-child td {
		border-bottom: 0;
	}

	.ev-table tbody tr:hover {
		background: color-mix(in srgb, var(--bg-soft) 72%, transparent);
	}

	.ev-row--unparseable {
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 5%, transparent);
	}

	.ev-num {
		text-align: right;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	/* Cost is the number this page is judged on. Monospace so `—` and `$0.00`
	   cannot be mistaken for each other at a glance. */
	.ev-cost {
		font-family: var(--font-mono, monospace);
	}

	.ev-cell-lane {
		min-width: 15rem;
	}

	.ev-lane-link {
		display: block;
		padding: 0;
		border: 0;
		background: none;
		color: var(--text-primary);
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.86rem;
		font-weight: 680;
		line-height: 1.3;
		text-align: left;
		cursor: pointer;
		overflow-wrap: anywhere;
	}

	.ev-lane-link:hover,
	.ev-lane-link:focus-visible {
		color: var(--accent-primary);
		text-decoration: underline;
		text-underline-offset: 0.16em;
	}

	.ev-lane-desc {
		display: block;
		margin-top: 0.15rem;
		color: var(--text-secondary);
		font-size: 0.74rem;
		line-height: 1.4;
	}

	.ev-lane-target {
		display: block;
		margin-top: 0.15rem;
		color: var(--text-faint, var(--text-muted));
		font-family: var(--font-mono, monospace);
		font-size: 0.66rem;
		overflow-wrap: anywhere;
	}

	.ev-parse-error {
		margin: 0.4rem 0 0;
		padding: 0.35rem 0.5rem;
		border-left: 3px solid var(--color-error, var(--status-failed));
		border-radius: var(--radius-xs, 4px);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 10%, transparent);
		color: var(--text-primary);
		font-size: 0.72rem;
		line-height: 1.45;
	}

	.ev-started {
		margin: 0.35rem 0 0;
		color: var(--text-muted);
		font-size: 0.68rem;
	}

	.ev-started code {
		font-family: var(--font-mono, monospace);
	}

	.ev-cell-requires {
		min-width: 9rem;
	}

	.ev-req-list {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.ev-req {
		padding: 0.1rem 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		color: var(--text-secondary);
		font-size: 0.68rem;
		white-space: nowrap;
	}

	.ev-req--missing {
		border-color: color-mix(in srgb, var(--color-error, var(--status-failed)) 45%, transparent);
		background: color-mix(in srgb, var(--color-error, var(--status-failed)) 12%, transparent);
		color: var(--color-error, var(--status-failed));
		font-weight: 700;
	}

	.ev-cell-last {
		white-space: nowrap;
	}

	.ev-when {
		margin-left: 0.4rem;
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}

	.ev-muted {
		color: var(--text-muted);
	}

	.ev-cell-actions {
		text-align: right;
		white-space: nowrap;
	}

	.ev-run-wrap {
		display: inline-flex;
	}

	.ev-run {
		min-height: 1.75rem;
		padding: 0.28rem 0.7rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.74rem;
		font-weight: 700;
		cursor: pointer;
	}

	.ev-run--lg {
		min-height: 2.1rem;
		padding: 0.4rem 1rem;
		font-size: 0.82rem;
	}

	.ev-run:hover:not(:disabled) {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.ev-run:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.ev-run:disabled {
		cursor: not-allowed;
		opacity: 0.45;
	}

	.ev-lane-head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
		padding: 0.85rem 0.9rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.ev-lane-meta {
		min-width: 0;
		flex: 1 1 26rem;
	}

	.ev-lane-meta h2 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.05rem;
		font-weight: 700;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.ev-lane-meta > p {
		margin: 0.25rem 0 0;
		color: var(--text-secondary);
		font-size: 0.82rem;
		line-height: 1.5;
	}

	.ev-meta-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(13rem, 1fr));
		gap: 0.55rem 1rem;
		margin: 0.75rem 0 0;
	}

	.ev-meta-grid dt {
		color: var(--text-muted);
		font-size: 0.65rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.ev-meta-grid dd {
		margin: 0.15rem 0 0;
		color: var(--text-primary);
		font-size: 0.78rem;
		overflow-wrap: anywhere;
	}

	.ev-meta-grid code {
		font-family: var(--font-mono, monospace);
		font-size: 0.72rem;
	}

	.ev-trend {
		display: flex;
		align-items: flex-end;
		gap: 3px;
		height: 3.4rem;
		padding: 0.6rem 0.9rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.ev-trend-bar {
		display: block;
		flex: 1 1 0;
		max-width: 1.4rem;
		min-height: 2px;
		border-radius: 2px 2px 0 0;
		background: var(--text-muted);
	}

	.ev-trend-bar--success {
		background: var(--status-completed, var(--color-success));
	}

	.ev-trend-bar--error {
		background: var(--status-failed, var(--color-error));
	}

	.ev-trend-bar--warning {
		background: var(--status-paused, var(--color-warning));
	}

	.ev-table a {
		color: var(--accent-primary);
		font-size: 0.76rem;
		font-weight: 700;
		text-decoration: none;
		white-space: nowrap;
	}

	.ev-table a:hover,
	.ev-table a:focus-visible {
		text-decoration: underline;
		text-underline-offset: 0.16em;
	}

	.ev-sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	@media (max-width: 720px) {
		.ev-shell {
			padding: 0.9rem 0.85rem 1.6rem;
		}

		.ev-tabs {
			width: 100%;
			overflow-x: auto;
		}
	}
</style>
