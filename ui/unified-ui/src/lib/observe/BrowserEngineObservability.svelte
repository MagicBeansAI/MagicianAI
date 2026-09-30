<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import {
		fetchBrowserEngineUsage,
		type BrowserEngineOutcome,
		type BrowserEngineSummary,
		type BrowserEngineUsagePage
	} from './browserEngineAnalytics';

	const PAGE_SIZE_OPTIONS = [10, 25, 50, 100];
	let result: BrowserEngineUsagePage = {
		items: [],
		total_count: 0,
		limit: 25,
		offset: 0,
		has_more: false,
		summary: []
	};
	let page = 0;
	let pageSize = 25;
	let engine = '';
	let outcome: BrowserEngineOutcome = 'all';
	let loading = true;
	let error = '';
	let requestId = 0;
	let controller: AbortController | null = null;
	let knownEngines: string[] = [];
	let mounted = false;
	let loadedScopeKey = '';

	$: pageCount = Math.max(1, Math.ceil(result.total_count / pageSize));
	$: currentPage = Math.min(pageCount, page + 1);
	$: startItem = result.total_count === 0 ? 0 : page * pageSize + 1;
	$: endItem = result.total_count === 0 ? 0 : Math.min(result.total_count, startItem + result.items.length - 1);
	$: scopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;
	$: if (mounted && scopeKey !== loadedScopeKey) {
		loadedScopeKey = scopeKey;
		page = 0;
		knownEngines = [];
		void load();
	}

	function mergeKnownEngines(summary: BrowserEngineSummary[]): void {
		knownEngines = Array.from(new Set([...knownEngines, ...summary.map((row) => row.engine)])).sort();
	}

	async function load(): Promise<void> {
		const id = ++requestId;
		controller?.abort();
		const currentController = new AbortController();
		controller = currentController;
		loading = true;
		error = '';
		try {
			const next = await fetchBrowserEngineUsage({
				page,
				pageSize,
				engine,
				outcome,
				signal: currentController.signal
			});
			if (id !== requestId) return;
			result = next;
			mergeKnownEngines(next.summary);
		} catch (reason) {
			if (id !== requestId || currentController.signal.aborted) return;
			error = reason instanceof Error ? reason.message : String(reason);
		} finally {
			if (id === requestId) loading = false;
		}
	}

	export function refresh(): void {
		void load();
	}

	function changePage(next: number): void {
		const nextIndex = Math.max(0, Math.min(pageCount - 1, next - 1));
		if (nextIndex === page) return;
		page = nextIndex;
		void load();
	}

	function changePageSize(next: number): void {
		if (!PAGE_SIZE_OPTIONS.includes(next) || next === pageSize) return;
		pageSize = next;
		page = 0;
		void load();
	}

	function changeEngine(next: string): void {
		if (next === engine) return;
		engine = next;
		page = 0;
		void load();
	}

	function changeOutcome(next: BrowserEngineOutcome): void {
		if (next === outcome) return;
		outcome = next;
		page = 0;
		void load();
	}

	function dateTime(timestamp: number): string {
		if (!Number.isFinite(timestamp)) return 'Unknown';
		return new Intl.DateTimeFormat(undefined, {
			month: 'short',
			day: 'numeric',
			hour: 'numeric',
			minute: '2-digit',
			second: '2-digit'
		}).format(new Date(timestamp));
	}

	function engineLabel(value: string): string {
		return value.replaceAll('_', ' ').replaceAll('-', ' ');
	}

	function latency(value: number): string {
		return value < 1_000 ? `${Math.round(value)} ms` : `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)} s`;
	}

	onMount(() => {
		mounted = true;
		loadedScopeKey = scopeKey;
		void load();
	});
	onDestroy(() => controller?.abort());
</script>

<section class="browser-engine-panel" aria-labelledby="browser-engine-title">
	<div class="panel-head">
		<div>
			<h2 id="browser-engine-title">Browser engine activity</h2>
			<p>Real command attempts, links, work ownership, fallbacks, and outcomes.</p>
		</div>
		<button
			type="button"
			class="icon-button"
			on:click={() => void load()}
			disabled={loading}
			title="Refresh browser activity"
			aria-label="Refresh browser activity"
		>
			<Icon name="rotate-ccw" size={15} class={loading ? 'spinning' : ''} />
		</button>
	</div>

	{#if result.summary.length > 0}
		<div class="summary-grid" aria-label="Browser engine success summary">
			{#each result.summary as row (row.engine)}
				<div class="summary-card">
					<span>{engineLabel(row.engine)}</span>
					<strong>{Math.round(row.success_rate * 100)}%</strong>
					<small>{row.successes} succeeded · {row.failures} failed · {latency(row.average_elapsed_ms)} avg</small>
				</div>
			{/each}
		</div>
	{/if}

	<div class="table-controls">
		<div class="filters">
			<label>
				<span>Engine</span>
				<select value={engine} on:change={(event) => changeEngine((event.currentTarget as HTMLSelectElement).value)} disabled={loading}>
					<option value="">All engines</option>
					{#each knownEngines as value (value)}<option value={value}>{engineLabel(value)}</option>{/each}
				</select>
			</label>
			<label>
				<span>Outcome</span>
				<select value={outcome} on:change={(event) => changeOutcome((event.currentTarget as HTMLSelectElement).value as BrowserEngineOutcome)} disabled={loading}>
					<option value="all">All outcomes</option>
					<option value="success">Succeeded</option>
					<option value="failure">Failed</option>
				</select>
			</label>
			<label>
				<span>Rows</span>
				<select value={pageSize} on:change={(event) => changePageSize(Number((event.currentTarget as HTMLSelectElement).value))} disabled={loading}>
					{#each PAGE_SIZE_OPTIONS as value (value)}<option value={value}>{value}</option>{/each}
				</select>
			</label>
		</div>
		<ServerPager
			{currentPage}
			{pageCount}
			{startItem}
			{endItem}
			totalItems={result.total_count}
			{loading}
			ariaLabel="Browser engine activity pagination"
			on:pagechange={(event) => changePage(event.detail.page)}
		/>
	</div>

	{#if error}
		<p class="error" role="alert">{error}</p>
	{:else if !loading && result.items.length === 0}
		<p class="empty">No browser engine attempts match these filters yet.</p>
	{:else}
		<div class:loading class="table-wrap" aria-busy={loading}>
			<table>
				<thead><tr><th>When</th><th>Engine</th><th>Link and work</th><th>Operation</th><th>Outcome</th><th>Latency</th></tr></thead>
				<tbody>
					{#each result.items as row (row.id)}
						<tr>
							<td class="when">{dateTime(row.occurred_at_ms)}</td>
							<td>
								<strong class="engine">{engineLabel(row.engine)}</strong>
								{#if row.fallback_from}<small>fallback from {engineLabel(row.fallback_from)}</small>{/if}
								<small>{engineLabel(row.connection_mode)}</small>
							</td>
							<td class="link-work">
								{#if row.url}<a href={row.url} target="_blank" rel="noreferrer" title={row.url}>{row.url}</a>{:else}<span>No page URL</span>{/if}
								<small title={row.work_id}>{engineLabel(row.work_kind)} · {row.work_id}</small>
							</td>
							<td><code>{row.operation}</code></td>
							<td>
								<span class:success={row.success} class:failure={!row.success} class="outcome">{row.success ? 'Succeeded' : 'Failed'}</span>
								{#if row.error_class}<small>{engineLabel(row.error_class)}</small>{/if}
							</td>
							<td class="latency">{latency(row.elapsed_ms)}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</section>

<style>
	.browser-engine-panel { padding: 1.15rem; border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 16px); background: var(--bg-card); }
	.panel-head, .table-controls { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; }
	h2 { margin: 0; font-size: 1rem; }
	.icon-button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 2rem;
		height: 2rem;
		padding: 0;
		border: 1px solid var(--border-soft);
		border-radius: 7px;
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
		transition: border-color 0.15s ease, color 0.15s ease, background 0.15s ease, transform 0.15s ease;
	}
	.icon-button:hover:not(:disabled) {
		border-color: var(--border-default);
		color: var(--text-primary);
		background: var(--bg-soft);
		transform: translateY(-1px);
	}
	.icon-button:disabled {
		opacity: 0.55;
		cursor: default;
	}
	:global(.spinning) {
		animation: spin 900ms linear infinite;
	}
	@keyframes spin {
		to {
			transform: rotate(-360deg);
		}
	}
	.summary-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: .6rem; margin: 1rem 0; }
	.summary-card { display: grid; grid-template-columns: 1fr auto; gap: .2rem .5rem; padding: .7rem .8rem; border-radius: 12px; background: var(--bg-soft); }
	.summary-card span { color: var(--text-secondary); font-size: .78rem; text-transform: capitalize; }
	.summary-card strong { font-variant-numeric: tabular-nums; }
	.summary-card small { grid-column: 1 / -1; color: var(--text-muted); }
	.table-controls { align-items: center; margin: .85rem 0 .55rem; }
	.filters { display: flex; flex-wrap: wrap; gap: .55rem; }
	.filters label { display: flex; align-items: center; gap: .35rem; color: var(--text-muted); font-size: .72rem; }
	select { border: 1px solid var(--border-soft); border-radius: 8px; padding: .32rem .5rem; background: var(--bg-card); color: var(--text-primary); }
	.table-wrap { overflow-x: auto; transition: opacity .15s ease; }
	.table-wrap.loading { opacity: .55; }
	table { width: 100%; border-collapse: collapse; font-size: .78rem; }
	th { padding: .55rem .5rem; border-bottom: 1px solid var(--border-soft); color: var(--text-muted); font-weight: 600; text-align: left; white-space: nowrap; }
	td { padding: .65rem .5rem; border-bottom: 1px solid color-mix(in srgb, var(--border-soft) 65%, transparent); vertical-align: top; }
	td small { display: block; margin-top: .18rem; color: var(--text-muted); }
	.engine { text-transform: capitalize; }
	.when, .latency { white-space: nowrap; font-variant-numeric: tabular-nums; }
	.link-work { min-width: 240px; max-width: 420px; }
	.link-work a { display: block; overflow: hidden; color: var(--accent-primary); text-overflow: ellipsis; white-space: nowrap; }
	.link-work span { color: var(--text-muted); }
	.link-work small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	code { color: var(--text-secondary); }
	.outcome { display: inline-flex; border-radius: 999px; padding: .18rem .45rem; font-weight: 600; }
	.outcome.success { background: color-mix(in srgb, var(--color-success) 14%, transparent); color: var(--color-success); }
	.outcome.failure { background: color-mix(in srgb, var(--color-danger, #b42318) 14%, transparent); color: var(--color-danger, #b42318); }
	.error, .empty { margin: 1rem 0 0; color: var(--text-muted); }
	.error { color: var(--color-danger, #b42318); }
	@media (max-width: 760px) { .panel-head, .table-controls { align-items: stretch; flex-direction: column; } }
</style>
