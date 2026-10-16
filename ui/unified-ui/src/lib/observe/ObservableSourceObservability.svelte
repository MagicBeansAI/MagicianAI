<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		fetchObservationRunHistory,
		fetchObservationSourceObservability,
		type ObservationRunRecord,
		type ObservationRunRecordPage,
		type ObservationSourceObservabilityPage,
		type ObservationSourceObservabilitySummary
	} from './sourceApi';

	const PAGE_SIZE = 5;
	const RUN_PAGE_SIZE = 5;
	const SKELETON_ROWS = Array.from({ length: PAGE_SIZE });

	let page: ObservationSourceObservabilityPage | null = null;
	let cursor: string | null = null;
	let cursorHistory: Array<string | null> = [];
	let loading = true;
	let refreshing = false;
	let error: string | null = null;
	let selectedId: string | null = null;
	let runPage: ObservationRunRecordPage | null = null;
	let runCursor: string | null = null;
	let runCursorHistory: Array<string | null> = [];
	let runsLoading = false;
	let runsError: string | null = null;
	let timer: ReturnType<typeof setInterval> | null = null;
	let mounted = false;
	let loadedScopeKey = '';
	let requestId = 0;
	let runRequestId = 0;

	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: selectedSource =
		page?.items.find((source) => source.subscription_id === selectedId) ?? null;
	$: if (mounted && scopeKey !== loadedScopeKey) {
		loadedScopeKey = scopeKey;
		reset();
		void loadPage(null, []);
	}

	export function refresh(): void {
		void loadPage(cursor, cursorHistory, true);
		if (selectedId) void loadRuns(selectedId, runCursor, runCursorHistory);
	}

	function reset(): void {
		requestId += 1;
		runRequestId += 1;
		page = null;
		cursor = null;
		cursorHistory = [];
		selectedId = null;
		runPage = null;
		runCursor = null;
		runCursorHistory = [];
		error = null;
		runsError = null;
		loading = true;
	}

	function message(error: unknown): string {
		return error instanceof Error ? error.message : String(error);
	}

	function errorCode(error: unknown): string | undefined {
		return (error as Error & { code?: string }).code;
	}

	async function loadPage(
		nextCursor: string | null,
		nextHistory: Array<string | null>,
		isRefresh = false
	): Promise<void> {
		const currentRequest = ++requestId;
		if (isRefresh && page) refreshing = true;
		else loading = true;
		error = null;
		try {
			const response = await fetchObservationSourceObservability({
				cursor: nextCursor,
				limit: PAGE_SIZE
			});
			if (!Array.isArray(response.items) || !response.totals || !response.runtime_metrics) {
				throw new Error('Web source stats require the current Magician backend build');
			}
			if (currentRequest !== requestId) return;
			page = response;
			cursor = nextCursor;
			cursorHistory = nextHistory;
			if (selectedId && !response.items.some((item) => item.subscription_id === selectedId)) {
				selectedId = null;
				runPage = null;
			}
		} catch (reason) {
			if (currentRequest !== requestId) return;
			if (errorCode(reason) === 'stale_cursor' && nextCursor) {
				await loadPage(null, [], isRefresh);
				return;
			}
			error = message(reason);
		} finally {
			if (currentRequest === requestId) {
				loading = false;
				refreshing = false;
			}
		}
	}

	async function loadRuns(
		subscriptionId: string,
		nextCursor: string | null = null,
		nextHistory: Array<string | null> = []
	): Promise<void> {
		const currentRequest = ++runRequestId;
		runsLoading = true;
		runsError = null;
		try {
			const response = await fetchObservationRunHistory(subscriptionId, {
				cursor: nextCursor,
				limit: RUN_PAGE_SIZE
			});
			if (currentRequest !== runRequestId || selectedId !== subscriptionId) return;
			runPage = response;
			runCursor = nextCursor;
			runCursorHistory = nextHistory;
		} catch (reason) {
			if (currentRequest !== runRequestId) return;
			if (errorCode(reason) === 'stale_cursor' && nextCursor) {
				await loadRuns(subscriptionId, null, []);
				return;
			}
			runsError = message(reason);
		} finally {
			if (currentRequest === runRequestId) runsLoading = false;
		}
	}

	function toggleRuns(source: ObservationSourceObservabilitySummary): void {
		if (selectedId === source.subscription_id) {
			selectedId = null;
			runRequestId += 1;
			runPage = null;
			return;
		}
		selectedId = source.subscription_id;
		runPage = null;
		runCursor = null;
		runCursorHistory = [];
		void loadRuns(source.subscription_id);
	}

	function nextPage(): void {
		if (!page?.next_cursor || loading) return;
		void loadPage(page.next_cursor, [...cursorHistory, cursor]);
	}

	function previousPage(): void {
		if (!cursorHistory.length || loading) return;
		const history = [...cursorHistory];
		const previous = history.pop() ?? null;
		void loadPage(previous, history);
	}

	function nextRunPage(): void {
		if (!selectedId || !runPage?.next_cursor || runsLoading) return;
		void loadRuns(selectedId, runPage.next_cursor, [...runCursorHistory, runCursor]);
	}

	function previousRunPage(): void {
		if (!selectedId || !runCursorHistory.length || runsLoading) return;
		const history = [...runCursorHistory];
		const previous = history.pop() ?? null;
		void loadRuns(selectedId, previous, history);
	}

	function status(source: ObservationSourceObservabilitySummary): {
		label: string;
		className: string;
	} {
		if (!source.enabled) return { label: 'Paused', className: 'paused' };
		if (!source.last_run) return { label: 'Waiting', className: 'waiting' };
		if (source.last_run.status === 'succeeded') return { label: 'Healthy', className: 'healthy' };
		if (source.last_run.status === 'cancelled') return { label: 'Interrupted', className: 'waiting' };
		return { label: 'Needs attention', className: 'failed' };
	}

	function runStatus(run: ObservationRunRecord): string {
		return run.status === 'succeeded'
			? run.not_modified_targets > 0 && run.modified_targets === 0
				? 'Unchanged'
				: 'Succeeded'
			: run.status === 'cancelled'
				? 'Interrupted'
				: 'Failed';
	}

	function localTime(value?: number | null): string {
		if (!value) return 'Not checked';
		return new Intl.DateTimeFormat(undefined, {
			month: 'short',
			day: 'numeric',
			hour: 'numeric',
			minute: '2-digit'
		}).format(new Date(value));
	}

	function relativeTime(value?: number | null): string {
		if (!value) return 'waiting for first run';
		const seconds = Math.round((value - Date.now()) / 1000);
		const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
		if (Math.abs(seconds) < 90) return formatter.format(seconds, 'second');
		const minutes = Math.round(seconds / 60);
		if (Math.abs(minutes) < 90) return formatter.format(minutes, 'minute');
		const hours = Math.round(minutes / 60);
		if (Math.abs(hours) < 48) return formatter.format(hours, 'hour');
		return formatter.format(Math.round(hours / 24), 'day');
	}

	function duration(value: number): string {
		if (value < 1_000) return `${value} ms`;
		if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)} s`;
		return `${(value / 60_000).toFixed(1)} min`;
	}

	function averageDuration(source: ObservationSourceObservabilitySummary): string {
		return source.runs > 0 ? duration(Math.round(source.total_latency_ms / source.runs)) : '—';
	}

	function bytes(value: number): string {
		if (value < 1_024) return `${value} B`;
		if (value < 1_048_576) return `${(value / 1_024).toFixed(1)} KB`;
		return `${(value / 1_048_576).toFixed(1)} MB`;
	}

	function successRate(): string {
		const totals = page?.totals;
		if (!totals || totals.runs === 0) return '—';
		return `${Math.round((totals.succeeded / totals.runs) * 100)}%`;
	}

	onMount(() => {
		mounted = true;
		loadedScopeKey = scopeKey;
		void loadPage(null, []);
		timer = setInterval(refresh, 60_000);
	});

	onDestroy(() => {
		mounted = false;
		requestId += 1;
		runRequestId += 1;
		if (timer) clearInterval(timer);
	});
</script>

<section class="web-source-stats" aria-labelledby="web-source-stats-title">
	<header class="web-source-stats__head">
		<div>
			<span class="eyebrow">Scheduled observations</span>
			<h2 id="web-source-stats-title">Observable sources</h2>
			<p>Per-source health, selection flow, processing, cache behavior, and Worth a look admission.</p>
		</div>
		<button
			type="button"
			class="icon-button"
			title="Refresh web source stats"
			aria-label="Refresh web source stats"
			disabled={loading || refreshing}
			on:click={refresh}
		>
			<Icon name="rotate-ccw" size={15} />
		</button>
	</header>

	{#if loading && !page}
		<div class="summary-strip summary-strip--loading" aria-hidden="true">
			{#each Array.from({ length: 5 }) as _}<span></span>{/each}
		</div>
		<div class="source-table source-table--loading" aria-busy="true" aria-label="Loading web source stats">
			{#each SKELETON_ROWS as _}
				<div class="skeleton-row">
					<span class="skeleton-block wide"></span><span class="skeleton-block"></span>
					<span class="skeleton-block medium"></span><span class="skeleton-block wide"></span>
					<span class="skeleton-block"></span><span class="skeleton-block"></span>
				</div>
			{/each}
		</div>
	{:else if error && !page}
		<div class="notice notice--error">
			<div><strong>Web source stats are unavailable</strong><span>{error}</span></div>
			<button type="button" on:click={() => void loadPage(null, [])}>Retry</button>
		</div>
	{:else if page}
		<div class="summary-strip" aria-label="Web source summary">
			<div><strong>{page.totals.enabled}</strong><span>Listening</span></div>
			<div><strong>{page.totals.healthy}</strong><span>Healthy</span></div>
			<div><strong>{successRate()}</strong><span>Run success</span></div>
			<div class:warning={page.handoff_backlog > 0}>
				<strong>{page.handoff_backlog}</strong><span>Awaiting processing</span>
			</div>
			<div><strong>{page.totals.candidates_selected}</strong><span>Selected</span></div>
		</div>
		<div class="runtime-line">
			<span>Current process</span>
			<strong>{page.runtime_metrics.runs_started} runs</strong>
			<span>{page.runtime_metrics.runs_throttled} throttled</span>
			<span>{page.runtime_metrics.policy_denials} policy denied</span>
			<span>{page.runtime_metrics.enrichment_processed ?? 0} processed</span>
			<span>{page.failed_handoffs ?? 0} quarantined</span>
			<span>{page.run_history_retained} run receipts retained</span>
		</div>

		{#if error}
			<div class="inline-error">Refresh failed: {error}</div>
		{/if}

		{#if page.items.length === 0}
			<div class="empty-state">
				<Icon name="eye" size={18} />
				<div><strong>No sources are listening</strong><span>Add one from the Observe page.</span></div>
			</div>
		{:else}
			<div class="table-shell" class:is-refreshing={refreshing}>
				<table>
					<thead>
						<tr>
							<th>Source</th>
							<th>State</th>
							<th>Last check</th>
							<th>Discover → select → process</th>
							<th>Cache</th>
							<th>Avg time</th>
							<th><span class="sr-only">Run history</span></th>
						</tr>
					</thead>
					<tbody>
						{#each page.items as source (source.subscription_id)}
							{@const sourceStatus = status(source)}
							<tr class:selected={selectedId === source.subscription_id}>
								<td>
									<strong>{source.display_name}</strong>
									<span>{source.category} · {source.action_id}</span>
								</td>
								<td>
									<span class="status {sourceStatus.className}"><i></i>{sourceStatus.label}</span>
									<span>Next {relativeTime(source.next_run_at_ms)}</span>
								</td>
								<td>
									<strong>{localTime(source.last_run?.finished_at_ms)}</strong>
									<span>{relativeTime(source.last_run?.finished_at_ms)}</span>
								</td>
								<td>
									<div class="flow" aria-label={`${source.candidates_discovered} discovered, ${source.candidates_selected} selected, ${source.enrichment_processed ?? 0} processed`}>
										<strong>{source.candidates_discovered}</strong><i></i>
										<strong>{source.candidates_selected}</strong><i></i>
										<strong>{source.enrichment_processed ?? 0}</strong>
									</div>
									<span>{source.enrichment_handoffs} queued · {source.enrichment_failed ?? 0} failed</span>
								</td>
								<td>
									<strong>{source.not_modified_targets} unchanged</strong>
									<span>{source.modified_targets} fetched · {bytes(source.response_bytes)}</span>
								</td>
								<td><strong>{averageDuration(source)}</strong><span>{source.runs} runs</span></td>
								<td>
									<button
										type="button"
										class="row-action"
										class:active={selectedId === source.subscription_id}
										aria-expanded={selectedId === source.subscription_id}
										aria-label={`Show ${source.display_name} run history`}
										on:click={() => toggleRuns(source)}
									>
										<Icon name="chevron-right" size={14} />
									</button>
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}

		{#if page.total > 0}
			<nav class="pager" aria-label="Observable source pages">
				<span>{cursorHistory.length * PAGE_SIZE + 1}–{Math.min((cursorHistory.length + 1) * PAGE_SIZE, page.total)} of {page.total}</span>
				<div>
					<button type="button" aria-label="Previous sources" disabled={!cursorHistory.length || loading} on:click={previousPage}><Icon name="chevron-left" size={14} /></button>
					<span>Page {cursorHistory.length + 1}</span>
					<button type="button" aria-label="Next sources" disabled={!page.next_cursor || loading} on:click={nextPage}><Icon name="chevron-right" size={14} /></button>
				</div>
			</nav>
		{/if}

		{#if selectedId}
			<section class="run-history" aria-labelledby="run-history-title">
				<header>
					<div><span>Run history</span><h3 id="run-history-title">{selectedSource?.display_name ?? 'Selected source'}</h3></div>
					{#if selectedSource?.last_error_class}<code>{selectedSource.last_error_class}</code>{/if}
				</header>
				{#if runsLoading && !runPage}
					<div class="run-loading" aria-label="Loading run history"><span></span><span></span><span></span></div>
				{:else if runsError}
					<div class="inline-error">Run history failed: {runsError}</div>
				{:else if runPage?.items.length === 0}
					<p class="run-empty">No completed runs are recorded yet.</p>
				{:else if runPage}
					<div class="run-list">
						{#each runPage.items as run (run.run_id)}
							<div class="run-row">
								<span class="run-state {run.status}"><i></i>{runStatus(run)}</span>
								<div><strong>{localTime(run.finished_at_ms)}</strong><span>{run.trigger}</span></div>
								<div><strong>{run.discovered} → {run.selected} → {run.handed_off}</strong><span>discovered · selected · handed off</span></div>
								<div><strong>{run.not_modified_targets} unchanged</strong><span>{run.modified_targets} fetched · {bytes(run.response_bytes)}</span></div>
								<div><strong>{duration(run.duration_ms)}</strong><span>{run.error_class ?? 'completed'}</span></div>
							</div>
						{/each}
					</div>
					<nav class="pager pager--runs" aria-label="Run history pages">
						<span>{runCursorHistory.length * RUN_PAGE_SIZE + 1}–{Math.min((runCursorHistory.length + 1) * RUN_PAGE_SIZE, runPage.total)} of {runPage.total}</span>
						<div>
							<button type="button" aria-label="Previous runs" disabled={!runCursorHistory.length || runsLoading} on:click={previousRunPage}><Icon name="chevron-left" size={14} /></button>
							<span>Page {runCursorHistory.length + 1}</span>
							<button type="button" aria-label="Next runs" disabled={!runPage.next_cursor || runsLoading} on:click={nextRunPage}><Icon name="chevron-right" size={14} /></button>
						</div>
					</nav>
				{/if}
			</section>
		{/if}
	{/if}
</section>

<style>
	.web-source-stats {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
		padding: 1.15rem 0 0.2rem;
		border-top: 1px solid var(--border-soft);
		color: var(--text-primary);
	}
	.web-source-stats__head {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}
	.web-source-stats__head h2,
	.run-history h3 {
		margin: 0;
		font-size: 1.05rem;
		letter-spacing: 0;
	}
	.web-source-stats__head p {
		margin: 0.18rem 0 0;
		font-size: 0.78rem;
		color: var(--text-muted);
	}
	.eyebrow,
	.run-history header span {
		display: block;
		margin-bottom: 0.12rem;
		font-size: 0.65rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--text-muted);
	}
	.icon-button,
	.row-action,
	.pager button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
	}
	.icon-button {
		width: 2rem;
		height: 2rem;
		padding: 0;
		border-radius: 7px;
	}
	.icon-button:hover:not(:disabled),
	.row-action:hover,
	.pager button:hover:not(:disabled) {
		border-color: var(--border-default);
		color: var(--text-primary);
		background: var(--bg-soft);
	}
	button:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 2px;
	}
	button:disabled {
		opacity: 0.45;
		cursor: default;
	}
	.summary-strip {
		display: grid;
		grid-template-columns: repeat(5, minmax(0, 1fr));
		min-height: 4.25rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
		overflow: hidden;
	}
	.summary-strip > div {
		display: flex;
		flex-direction: column;
		justify-content: center;
		gap: 0.08rem;
		padding: 0.7rem 0.9rem;
		border-right: 1px solid var(--border-soft);
	}
	.summary-strip > div:last-child { border-right: 0; }
	.summary-strip strong {
		font-size: 1.22rem;
		font-variant-numeric: tabular-nums;
	}
	.summary-strip span,
	.table-shell td > span,
	.run-row div span {
		font-size: 0.66rem;
		color: var(--text-muted);
	}
	.summary-strip .warning strong { color: var(--color-warning); }
	.runtime-line {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: 0.3rem 0.75rem;
		min-height: 1rem;
		font-size: 0.66rem;
		color: var(--text-muted);
	}
	.runtime-line > span:first-child {
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
	.runtime-line strong { color: var(--text-secondary); }
	.summary-strip--loading { padding: 0; }
	.summary-strip--loading > span {
		margin: 0.8rem;
		border-radius: 6px;
		background: color-mix(in srgb, var(--bg-soft) 84%, var(--text-muted) 8%);
	}
	.table-shell {
		overflow-x: auto;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
		transition: opacity 140ms ease;
	}
	.table-shell.is-refreshing { opacity: 0.68; }
	table {
		width: 100%;
		min-width: 980px;
		border-collapse: collapse;
		table-layout: fixed;
	}
	th {
		padding: 0.55rem 0.65rem;
		border-bottom: 1px solid var(--border-soft);
		background: var(--bg-soft);
		color: var(--text-muted);
		font-size: 0.64rem;
		font-weight: 700;
		text-align: left;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}
	th:nth-child(1) { width: 20%; }
	th:nth-child(2) { width: 12%; }
	th:nth-child(3) { width: 14%; }
	th:nth-child(4) { width: 22%; }
	th:nth-child(5) { width: 15%; }
	th:nth-child(6) { width: 11%; }
	th:nth-child(7) { width: 6%; }
	td {
		padding: 0.72rem 0.65rem;
		border-bottom: 1px solid var(--border-soft);
		vertical-align: middle;
		font-size: 0.76rem;
		color: var(--text-primary);
	}
	tbody tr:last-child td { border-bottom: 0; }
	tbody tr { transition: background 120ms ease; }
	tbody tr:hover,
	tbody tr.selected { background: color-mix(in srgb, var(--accent-primary) 6%, var(--bg-card)); }
	td > strong,
	td > span { display: block; min-width: 0; }
	td > strong {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-weight: 650;
	}
	td > span {
		margin-top: 0.12rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.status,
	.run-state {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-size: 0.7rem;
		font-weight: 650;
		white-space: nowrap;
	}
	.status i,
	.run-state i {
		width: 0.43rem;
		height: 0.43rem;
		border-radius: 50%;
		background: var(--text-muted);
	}
	.status.healthy i,
	.run-state.succeeded i { background: var(--color-success); }
	.status.failed i,
	.run-state.failed i { background: var(--color-danger, var(--color-error)); }
	.status.waiting i,
	.run-state.cancelled i { background: var(--color-warning); }
	.status.paused { color: var(--text-muted); }
	.flow {
		display: flex;
		align-items: center;
		gap: 0.38rem;
	}
	.flow strong {
		min-width: 1.5rem;
		font-variant-numeric: tabular-nums;
	}
	.flow i {
		width: 1.25rem;
		height: 1px;
		background: var(--border-default);
	}
	.row-action {
		width: 1.75rem;
		height: 1.75rem;
		padding: 0;
		border-radius: 6px;
		transition: transform 140ms ease, background 140ms ease;
	}
	.row-action.active {
		transform: rotate(90deg);
		background: var(--bg-soft);
		color: var(--text-primary);
	}
	.pager {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.7rem;
		min-height: 2rem;
		font-size: 0.7rem;
		color: var(--text-muted);
	}
	.pager > div {
		display: flex;
		align-items: center;
		gap: 0.35rem;
	}
	.pager button {
		width: 1.75rem;
		height: 1.75rem;
		padding: 0;
		border-radius: 6px;
	}
	.pager > div > span { min-width: 3.5rem; text-align: center; }
	.run-history {
		padding: 0.9rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: color-mix(in srgb, var(--bg-soft) 58%, var(--bg-card));
	}
	.run-history > header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 0.65rem;
	}
	.run-history h3 { font-size: 0.88rem; }
	.run-history code {
		max-width: 24rem;
		overflow: hidden;
		text-overflow: ellipsis;
		padding: 0.2rem 0.35rem;
		border: 1px solid var(--border-soft);
		border-radius: 5px;
		background: var(--bg-card);
		color: var(--color-danger, var(--color-error));
		font-size: 0.66rem;
	}
	.run-list {
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-card);
		overflow: hidden;
	}
	.run-row {
		display: grid;
		grid-template-columns: 7.5rem 10rem minmax(13rem, 1fr) minmax(10rem, 0.8fr) 8rem;
		align-items: center;
		gap: 0.7rem;
		min-height: 3.25rem;
		padding: 0.55rem 0.65rem;
		border-bottom: 1px solid var(--border-soft);
	}
	.run-row:last-child { border-bottom: 0; }
	.run-row div { min-width: 0; }
	.run-row div strong,
	.run-row div span {
		display: block;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.run-row div strong { font-size: 0.72rem; font-weight: 650; }
	.pager--runs { margin-top: 0.55rem; }
	.empty-state,
	.notice {
		display: flex;
		align-items: center;
		gap: 0.7rem;
		min-height: 5rem;
		padding: 0.8rem 1rem;
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
		color: var(--text-secondary);
	}
	.empty-state div,
	.notice div { display: flex; flex-direction: column; gap: 0.12rem; }
	.empty-state strong,
	.notice strong { color: var(--text-primary); font-size: 0.8rem; }
	.empty-state span,
	.notice span { font-size: 0.72rem; color: var(--text-muted); }
	.notice--error { border-color: color-mix(in srgb, var(--color-danger, var(--color-error)) 35%, var(--border-soft)); }
	.notice button {
		margin-left: auto;
		padding: 0.35rem 0.6rem;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		background: var(--bg-soft);
		color: var(--text-primary);
		cursor: pointer;
	}
	.inline-error {
		padding: 0.48rem 0.65rem;
		border-left: 3px solid var(--color-danger, var(--color-error));
		background: color-mix(in srgb, var(--color-danger, var(--color-error)) 7%, var(--bg-card));
		color: var(--text-secondary);
		font-size: 0.72rem;
	}
	.run-empty {
		margin: 0;
		padding: 1rem;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-card);
		color: var(--text-muted);
		font-size: 0.74rem;
	}
	.source-table--loading {
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		background: var(--bg-card);
		overflow: hidden;
	}
	.skeleton-row {
		display: grid;
		grid-template-columns: 1.4fr 0.7fr 0.9fr 1.2fr 0.8fr 0.55fr;
		align-items: center;
		gap: 1rem;
		min-height: 3.65rem;
		padding: 0 0.7rem;
		border-bottom: 1px solid var(--border-soft);
	}
	.skeleton-row:last-child { border-bottom: 0; }
	.skeleton-block,
	.run-loading span {
		display: block;
		height: 0.68rem;
		border-radius: 5px;
		background: color-mix(in srgb, var(--bg-soft) 84%, var(--text-muted) 8%);
		animation: pulse 1.35s ease-in-out infinite alternate;
	}
	.skeleton-block.wide { width: 82%; }
	.skeleton-block.medium { width: 64%; }
	.run-loading { display: grid; gap: 0.5rem; padding: 0.8rem; }
	.run-loading span { height: 2.2rem; }
	@keyframes pulse { to { opacity: 0.48; } }
	.sr-only {
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
	@media (prefers-reduced-motion: reduce) {
		.table-shell,
		.row-action,
		tbody tr { transition: none; }
		.skeleton-block,
		.run-loading span { animation: none; }
	}
</style>
