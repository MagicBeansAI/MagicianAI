<!--
  /llm — LLM observability page.

  Analytics over the per-scope Parquet `llm_calls` table via the
  /analytics/llm_calls/query(_batch) endpoints. Editorial theme; full DashboardChrome.

  Top filter bar applies to EVERY tab (KPIs + cost/latency widgets + cache
  performance widgets). Group-by dropdown drives the ad-hoc "Cache hit %"
  breakdown widget so the operator can pivot between operation / agent /
  model / task / chat_session / profile / provider without writing SQL.

  THREE TABS, exactly one live at a time. Only Cost loads on arrival, and
  switching unmounts the outgoing panel so its in-flight requests are
  cancelled. The page used to render everything on one load — roughly 35
  queries fired within 6.2s of mount whether or not anyone scrolled that far.

    Cost (default, eager)
      Spend by window (#spend): today / 7d / 30d, each by operation and by
        model, account-wide and independent of the filter bar
      Today vs yesterday (#today, the Today pulse band's deep-link target):
        delta chips + cumulative-spend-by-hour overlay + top 3 models
      L1  KPI row (Spend · Calls · Retry rate · Avg latency); spend over time
      L2  Top agents by spend; top operations by spend

    Usage (lazy)
      L1  Call explorer; per-call metadata log (LLM ops / embeddings / all)
      L2  Embeddings summary: totals, by model, by purpose, over time
      L3  VibeDev project rollup and provider/model breakdown

    Health (lazy)
      Canonical capture health from the governed fact reader
      L1  Cache hit ratio by model; latency p50/p95/p99 by operation
      L2  Cache performance: hit %, cached tokens, cache writes, TTFT; hit %
            over time
      L3  Hit % by operation and by agent; token mix by model; TTFT
            percentiles; ad-hoc group-by breakdown
      L4  Live event stream (filtered SSE)

  `L<n>` is that tab's progressive-reveal level — staged in after the tab
  opens so a panel paints before its heaviest widgets mount. Levels are
  page-scoped state, so a tab Refresh does not replay the stagger.
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import DashboardChrome from '$lib/magician/dashboard/DashboardChrome.svelte';
	import MetricCard from '$lib/magician/components/generative/MetricCard.svelte';
	import BarChart from '$lib/magician/components/generative/BarChart.svelte';
	import LineChart from '$lib/magician/components/generative/LineChart.svelte';
	import Table from '$lib/magician/components/generative/Table.svelte';
	import EventStreamCard from '$lib/realtime/EventStreamCard.svelte';
	import LlmUsageOverview from '$lib/magician/llm/LlmUsageOverview.svelte';
	import LlmCallExplorer from '$lib/magician/llm/LlmCallExplorer.svelte';
	import CallsTable from './CallsTable.svelte';
	import DecisionModelUsage from '$lib/magician/llm/DecisionModelUsage.svelte';
	import { modelCost } from '$lib/llm/decisionModels';
	import {
		buildCallsSql,
		buildEmbeddingByModelSql,
		buildEmbeddingByOperationSql,
		buildEmbeddingCallsSql,
		buildEmbeddingOverTimeSql,
		buildEmbeddingTotalsSql
	} from '$lib/llm/callsQuery';
	import {
		createLiveRewirer,
		setupLiveDataSource,
		type LiveDataSourceController
	} from '$lib/magician/dashboard/useLiveDataSource';
	import { taskStore } from '$lib/stores/taskStore';
	import { vibeDevProjectStore, type VibeDevProject } from '$lib/stores/vibeDevProjectStore';
	import { localDayBoundaries, formatDelta, type LocalDayBoundaries } from '$lib/today/pulseQueries';
	import {
		buildSpendBreakdownQueries,
		parseSpendRows,
		summarizeSpend,
		rollupSpendRows,
		type SpendWindow,
		type SpendRow
	} from '$lib/llm/spendBreakdown';
	import {
		deltaAria,
		formatSpend,
		spendTone,
		withYday,
		type PulseTone
	} from '$lib/today/pulseFormat';
	import { parseNumericDuckDbValue } from '$lib/magician/components/generative/chartUtil';
	import {
		buildVibeDevModelBreakdownSql,
		buildVibeDevProjectAttributions,
		buildVibeDevProjectRollupSql,
		costByProject,
		emptyProjectCost,
		formatCompactNumber,
		formatUsd,
		isVibeDevTask,
		modelLabel,
		totalTokens,
		type VibeDevProjectCost,
		type VibeDevTotalCost
	} from '$lib/vibedev/llmCost';

	let lastRefreshed = Date.now();
	const LLM_LIFETIME_PARTITION_HINT = '/* magician:all_llm_partitions */';

	// No dashboard-theme override on this page. The previous version did
	// `selectedThemeId.set('editorial')` + `applyThemeToCssVariables(...)`
	// in onMount, which writes inline styles on `document.documentElement`
	// for every `--theme-color-*` variable. Those inline styles WIN over
	// the app-level `[data-theme="..."]` selectors in app.css, so the
	// user's chosen app theme was being steamrolled.
	$: permalink = typeof window !== 'undefined' ? window.location.href : null;

	function handleRefresh(): void {
		lastRefreshed = Date.now();
	}

	// ─── Filter state ──────────────────────────────────────────────────
	//
	// Drives every SQL query on the page. `timeRangeDays` is the global
	// window for cumulative widgets (KPIs / cache totals); 24h-scoped
	// widgets (latency timeline, TTFT percentiles, hit-% timeline) clamp
	// to `min(timeRangeDays, 1)` so they always render at hourly
	// resolution. `0` means lifetime/all available telemetry for cumulative
	// widgets. Empty-string filters mean "no constraint on this dimension".
	let timeRangeDays = 7;
	let agentFilter = '';
	let taskFilter = '';
	let operationFilter = '';
	let modelFilter = '';
	let chatSessionFilter = '';
	let groupByDim:
		| 'operation'
		| 'agent_id'
		| 'model'
		| 'task_id'
		| 'chat_session_id'
		| 'profile'
		| 'provider' = 'operation';

	// Distinct-value lists are lazy: populated when a filter receives focus.
	// They scan the last 30d of `llm_calls` and are limited to LIMIT 200 each
	// so a long-lived sink can't flood the dropdowns. Bound to <datalist>
	// elements so the operator can type-to-search OR pick from the list, which
	// beats a fixed <select> for high-cardinality dimensions like task_id.
	let agentOptions: string[] = [];
	let taskOptions: string[] = [];
	let operationOptions: string[] = [];
	let modelOptions: string[] = [];
	let sessionOptions: string[] = [];
	let cleanupFns: Array<() => void> = [];
	let revealTimers: ReturnType<typeof setTimeout>[] = [];
	let filterOptionsLoaded = false;
	let filterOptionsLoading = false;

	// ─── Tabs ──────────────────────────────────────────────────────────
	//
	// Every section used to render on one load: ~35 queries fired within
	// 6.2s of mount whether or not the operator ever scrolled to them.
	// Sections are grouped into three tabs, and EXACTLY ONE TAB IS LIVE AT A
	// TIME — the others are neither rendered nor wired.
	//
	// Switching tabs unmounts the outgoing panel and tears its controllers
	// down, which cancels whatever it had in flight (`destroy()` calls
	// `activeRequest.abort()`). Unmounting is the only lever that reaches the
	// inline `dataSource` widgets: BarChart, LineChart, Table, MetricCard,
	// LlmCallExplorer and EventStreamCard each own their controller
	// privately, so the page cannot abort them any other way. Keeping panels
	// mounted-but-hidden would have left their fetches running and, for the
	// event stream, an SSE connection open on a tab nobody is looking at.
	//
	// GATING IS ENFORCED IN THE SCRIPT AS WELL AS THE MARKUP. Inline widgets
	// are lazy for free — an unrendered component never mounts, so it never
	// queries. The script-level fetchers are not: they run from reactive
	// statements that can't see what's on screen, and every
	// `setupLiveDataSource` controller listens for
	// `magician:dashboard-refresh` on `window`, so anything still wired
	// re-runs on a page-wide refresh regardless of tab. Each is gated on
	// `activeTab` and torn down by `teardownTab` — grep for both.
	type TabId = 'cost' | 'usage' | 'health';
	const TABS: Array<{ id: TabId; label: string; hint: string }> = [
		{ id: 'cost', label: 'Cost', hint: 'Spend by window, operation, and model' },
		{ id: 'usage', label: 'Usage', hint: 'Calls, embeddings, and VibeDev attribution' },
		{ id: 'health', label: 'Health', hint: 'Capture health, cache, and latency' }
	];
	// Cost is the only tab that loads on arrival. `onMount` may retarget this
	// from the URL hash before `pageMounted` opens the gate, so a deep link
	// never costs a throwaway Cost load.
	let activeTab: TabId = 'cost';

	// `taskStore.start()` is REFERENCE COUNTED (`activeConsumers`), so stop()
	// must be called once and only if we actually started it — an unpaired
	// stop() would decrement another consumer's count and tear down the
	// shared realtime bridges for the rest of the app. It feeds VibeDev
	// attribution only, so it waits for the Usage tab.
	let taskStoreStarted = false;

	function ensureUsageStores(): void {
		if (taskStoreStarted) return;
		taskStoreStarted = true;
		taskStore.start();
		void taskStore.loadTasks();
		void vibeDevProjectStore.load();
	}

	// Progressive reveal, now per tab rather than one page-wide ladder.
	// Levels live at page scope on purpose: a refresh re-keys a panel's
	// markup and leaving a tab unmounts it, and keeping the level here stops
	// either from replaying the stagger and making widgets flash back in one
	// at a time.
	let tabLevel: Record<TabId, number> = { cost: 0, usage: 0, health: 0 };
	let stagedTabs: Record<TabId, boolean> = { cost: false, usage: false, health: false };
	// One entry per level, in ms after the tab first opens. Cost is the
	// default tab and its headline (spend cards + today) renders
	// unconditionally, so it only stages the two filtered widget groups.
	const TAB_REVEAL_DELAYS: Record<TabId, number[]> = {
		cost: [150, 900],
		usage: [150, 1_200, 2_400],
		health: [150, 1_400, 3_000, 4_200]
	};

	// `30 DAYS` covers our retention window. We don't refresh on filter
	// changes — the dropdown options should be the universe of values,
	// not the currently-filtered subset.
	const distinctAgentsSql =
		"SELECT DISTINCT agent_id FROM llm_calls WHERE agent_id IS NOT NULL AND agent_id != '' " +
		"AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		'ORDER BY agent_id LIMIT 200';
	const distinctTasksSql =
		"SELECT DISTINCT task_id FROM llm_calls WHERE task_id IS NOT NULL AND task_id != '' " +
		"AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		'ORDER BY task_id LIMIT 200';
	const distinctOperationsSql =
		"SELECT DISTINCT operation FROM llm_calls WHERE operation IS NOT NULL AND operation != '' " +
		"AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		'ORDER BY operation LIMIT 200';
	const distinctModelsSql =
		"SELECT DISTINCT model FROM llm_calls WHERE model IS NOT NULL AND model != '' " +
		"AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		'ORDER BY model LIMIT 200';
	const distinctSessionsSql =
		"SELECT DISTINCT chat_session_id FROM llm_calls WHERE chat_session_id IS NOT NULL AND chat_session_id != '' " +
		"AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		'ORDER BY chat_session_id DESC LIMIT 200';

	function clearRevealTimers(): void {
		revealTimers.forEach((timer) => clearTimeout(timer));
		revealTimers = [];
	}

	// Stage a tab's widgets in once, the first time it is opened. Guarded by
	// `stagedTabs` rather than by `tabLevel > 0` because level 1 only lands
	// after the first delay, so two opens inside that window would otherwise
	// double-schedule.
	function scheduleTabReveal(tab: TabId): void {
		if (stagedTabs[tab]) return;
		stagedTabs = { ...stagedTabs, [tab]: true };
		TAB_REVEAL_DELAYS[tab].forEach((delay, index) => {
			revealTimers.push(
				setTimeout(() => {
					tabLevel = { ...tabLevel, [tab]: Math.max(tabLevel[tab], index + 1) };
				}, delay)
			);
		});
	}

	// Cancel everything the outgoing tab still has in flight. The panel's
	// inline widgets abort themselves when the `{#if activeTab}` block
	// unmounts them; these are the script-level controllers, which outlive
	// the markup and would otherwise keep fetching for a tab that is no
	// longer on screen.
	//
	// Each teardown also clears the "what is currently wired" sentinel it
	// owns, because the reactive blocks below re-wire only on a CHANGE. Leave
	// a stale sentinel behind and returning to the tab renders permanently
	// empty widgets.
	function teardownTab(tab: TabId): void {
		// A refresh queued for a tab we are leaving is dead weight.
		const timer = refreshTimers[tab];
		if (timer) clearTimeout(timer);
		delete refreshTimers[tab];
		tabRefreshPending = { ...tabRefreshPending, [tab]: false };
		tabRefreshQueued = { ...tabRefreshQueued, [tab]: false };

		if (tab === 'cost') {
			for (const controller of spendControllers) controller.destroy();
			spendControllers = [];
			spendWiredDay = 0;
			todaySectionController?.destroy();
			todaySectionController = null;
			todaySectionWiredSql = '';
		} else if (tab === 'usage') {
			destroyCallsControllers();
			callsFilterKey = '';
			callsOffset = 0;
			// `sync(null)`, NOT `destroy()`: the rewirer's destroy() leaves
			// `wiredKey` set, so a later sync with the same SQL matches the
			// stale key, early-returns, and never re-wires.
			vibeDevCostRewirer.sync(null);
			embeddingsRewirer.sync(null);
		} else {
			governedOverviewAbort?.abort();
			governedOverviewLoading = false;
		}
	}

	function openTab(tab: TabId): void {
		if (tab === activeTab) return;
		teardownTab(activeTab);
		activeTab = tab;
		if (tab === 'usage') ensureUsageStores();
		scheduleTabReveal(tab);
	}

	function wireDistinct(sql: string, target: (vals: string[]) => void, onSettled: () => void): void {
		const controller = setupLiveDataSource({
			dataSource: { kind: 'llm_calls_sql', sql },
			onRows: (result) => {
				const vals: string[] = [];
				for (const row of result.records) {
					const first = Object.values(row)[0];
					if (typeof first === 'string' && first.length > 0) vals.push(first);
				}
				target(vals);
			},
			onError: (err) => console.warn('[llm filter options]', err.message),
			onSettled
		});
		cleanupFns.push(() => controller.destroy());
	}

	function ensureFilterOptionsLoaded(): void {
		if (filterOptionsLoaded || filterOptionsLoading) return;
		filterOptionsLoading = true;
		filterOptionsLoaded = true;
		let settled = 0;
		const markSettled = (): void => {
			if (!filterOptionsLoading) return;
			settled += 1;
			if (settled >= 5) filterOptionsLoading = false;
		};
		wireDistinct(distinctAgentsSql, (v) => (agentOptions = v), markSettled);
		wireDistinct(distinctTasksSql, (v) => (taskOptions = v), markSettled);
		wireDistinct(distinctOperationsSql, (v) => (operationOptions = v), markSettled);
		wireDistinct(distinctModelsSql, (v) => (modelOptions = v), markSettled);
		wireDistinct(distinctSessionsSql, (v) => (sessionOptions = v), markSettled);
	}

	// ─── Calls tab (per-call, metadata-only log) ────────────────────────
	//
	// A filterable, paginated view of individual `llm_calls` rows — the
	// content-free counterpart to the aggregate widgets above. Same POST
	// path (`/analytics/llm_calls/query`) and scope handling as
	// `wireDistinct`; `buildCallsSql` (extracted to `$lib/llm/callsQuery`
	// for unit testing) projects only metadata columns — no prompt or
	// response text exists in this relation. `callsFilterKey` mirrors the
	// active filter WHERE so a filter change resets the page to offset 0
	// and replaces rows, while "Load more" bumps the offset and appends.
	//
	// A type toggle picks the source: LLM ops (llm_calls) / Embeddings
	// (llm_embeddings, content-free per-batch metadata) / All (both, merged
	// by timestamp_ms DESC client-side). Each row is tagged with a `kind`
	// (`'llm'` | `'embedding'`) so CallsTable can show a Type column and
	// render gracefully across the differing column sets (embeddings have
	// no output_tokens/ttft_ms/agent_id/llm_call_id; llm_calls have no
	// batch_size).
	type CallsType = 'llm' | 'decision' | 'embedding' | 'all';
	const CALLS_PAGE_SIZE = 50;
	let callsType: CallsType = 'all';
	let callsRows: any[] = [];
	let callsOffset = 0;
	let callsLoading = false;
	let callsFilterKey = '';
	// One live controller at a time per source: destroy the prior one before
	// wiring the next so stale-offset queries don't keep firing on the global
	// `dashboard-refresh` event and controllers don't accumulate across
	// filter/toggle changes and "Load more" presses. `All` needs two.
	let callsController: LiveDataSourceController | null = null;
	let callsEmbeddingController: LiveDataSourceController | null = null;

	function destroyCallsControllers(): void {
		callsController?.destroy();
		callsController = null;
		callsEmbeddingController?.destroy();
		callsEmbeddingController = null;
	}

	function mergeCallsByRecency(a: any[], b: any[]): any[] {
		return [...a, ...b].sort(
			(x, y) => Number(y?.timestamp_ms ?? 0) - Number(x?.timestamp_ms ?? 0)
		);
	}

	function wireCalls(replace: boolean): void {
		callsLoading = true;
		destroyCallsControllers();
		const llmWhere = buildWhere(timeRangeDays, dimensionParts) + (callsType === 'decision' ? " AND provider LIKE 'decision:%'" : callsType === 'llm' ? " AND provider NOT LIKE 'decision:%'" : "");
		const embedWhere = buildEmbeddingWhere(timeRangeDays);

		// `All` fans out to both sources and merges by recency once both
		// settle. Track outstanding fetches so the loading flag clears only
		// after every wired source returns, and buffer this page's rows so a
		// non-replace ("Load more") append stays deduplicated per source set.
		let pending = 0;
		let llmPage: any[] = [];
		let embedPage: any[] = [];
		const wantLlm = callsType === 'llm' || callsType === 'decision' || callsType === 'all';
		const wantEmbed = callsType === 'embedding' || callsType === 'all';

		const settle = (): void => {
			pending -= 1;
			if (pending > 0) return;
			const page = mergeCallsByRecency(llmPage, embedPage);
			callsRows = replace ? page : [...callsRows, ...page];
			callsLoading = false;
		};

		if (wantLlm) {
			pending += 1;
			const sql = buildCallsSql(llmWhere, CALLS_PAGE_SIZE, callsOffset);
			callsController = setupLiveDataSource({
				dataSource: { kind: 'llm_calls_sql', sql },
				onRows: (result) => {
					llmPage = result.records.map((r) => ({ ...r, kind: 'llm' }));
				},
				onError: (err) => console.warn('[llm calls]', err.message),
				onSettled: settle
			});
		}
		if (wantEmbed) {
			pending += 1;
			const sql = buildEmbeddingCallsSql(embedWhere, CALLS_PAGE_SIZE, callsOffset);
			callsEmbeddingController = setupLiveDataSource({
				dataSource: { kind: 'llm_embeddings_sql', sql },
				onRows: (result) => {
					embedPage = result.records.map((r) => ({ ...r, kind: 'embedding' }));
				},
				onError: (err) => console.warn('[llm embeddings]', err.message),
				onSettled: settle
			});
		}
	}

	function loadMoreCalls(): void {
		if (callsLoading) return;
		callsOffset += CALLS_PAGE_SIZE;
		wireCalls(false);
	}

	// Reset to the first page and replace rows whenever the filter WHERE, the
	// type toggle, or the refresh token changes, consistent with how every
	// other widget re-derives from the same filter state. The embeddings WHERE
	// is in the key too so an embeddings/all view re-pulls when its
	// (operation/model/provider-only) filters change.
	$: if (pageMounted && activeTab === 'usage') {
		// `dimensionParts` + the explicit operationFilter/modelFilter args make
		// every filter a tracked dependency of this reactive block, so an
		// Embeddings-only view still re-pulls when its (operation/model) filters
		// change even though those don't alter the llm_calls WHERE below.
		const nextKey =
			`${callsType}::${buildWhere(timeRangeDays, dimensionParts)}` +
			`::${buildEmbeddingWhere(timeRangeDays, operationFilter, modelFilter)}::${lastRefreshed}`;
		if (nextKey !== callsFilterKey) {
			callsFilterKey = nextKey;
			callsOffset = 0;
			wireCalls(true);
		}
	}

	onMount(() => {
		// Resolve the tab BEFORE `pageMounted` opens the gate: every fetcher
		// is gated on `pageMounted && activeTab === …`, and both assignments
		// land in the same synchronous block, so the first reactive flush sees
		// the final tab. A deep link therefore never pays for a throwaway Cost
		// load. `taskStore`/`vibeDevProjectStore` follow the same rule via
		// `ensureUsageStores`.
		//
		// `#usage` / `#health` deep-link a tab. `#spend` and `#today` live in
		// Cost, the default, so the Today pulse band's /llm#today link keeps
		// landing on a rendered element.
		const hash = window.location.hash.slice(1);
		if (hash === 'usage' || hash === 'health') {
			activeTab = hash;
			if (hash === 'usage') ensureUsageStores();
		}
		scheduleTabReveal(activeTab);
		pageMounted = true;
		// SvelteKit's built-in hash scrolling covers client-side navs (the
		// #today section renders unconditionally within the default tab, so
		// the element exists at nav time), but a hard load of /llm#today on
		// this client-rendered page can miss — nudge after first paint.
		// `scroll-margin-top` on the section keeps it clear of the sticky
		// filter bar.
		if (window.location.hash === '#today') {
			requestAnimationFrame(() => {
				document.getElementById('today')?.scrollIntoView({ block: 'start' });
			});
		}
	});

	onDestroy(() => {
		governedOverviewAbort?.abort();
		clearRevealTimers();
		clearRefreshTimers();
		for (const fn of cleanupFns) fn();
		cleanupFns = [];
		todaySectionController?.destroy();
		todaySectionController = null;
		for (const controller of spendControllers) controller.destroy();
		spendControllers = [];
		destroyCallsControllers();
		embeddingsRewirer.destroy();
		vibeDevCostRewirer.destroy();
		// Only if we started it — `start()`/`stop()` are reference counted
		// across the app, so an unpaired stop() would tear down the shared
		// realtime bridges for every other consumer.
		if (taskStoreStarted) taskStore.stop();
	});

	// ─── SQL helpers ───────────────────────────────────────────────────
	//
	// WHERE-clause bodies shared by every widget on the page. Time range
	// is present for bounded ranges; `0` intentionally means lifetime/all
	// available telemetry for cumulative widgets. Filter values are single-quote-escaped before
	// interpolation — defense-in-depth since the backend already requires
	// SELECT-only and runs in an ephemeral in-memory DuckDB, but cheap
	// insurance against an injected value that breaks query syntax.
	//
	// Reactivity: the builders take every filter as an ARGUMENT and the
	// `$:` statements pass the raw filter variables, because Svelte's
	// legacy-mode dependency tracking only sees identifiers in the
	// reactive expression itself — a `buildWhere()` that read the filters
	// inside its body never re-derived, which left the whole filter bar
	// inert after initial load.

	function escapeSqlValue(v: string): string {
		return v.replace(/'/g, "''");
	}

	function buildDimensionParts(
		agent: string,
		task: string,
		operation: string,
		model: string,
		session: string
	): string[] {
		const parts: string[] = [];
		if (agent.trim()) parts.push(`agent_id = '${escapeSqlValue(agent.trim())}'`);
		if (task.trim()) parts.push(`task_id = '${escapeSqlValue(task.trim())}'`);
		if (operation.trim()) parts.push(`operation = '${escapeSqlValue(operation.trim())}'`);
		if (model.trim()) parts.push(`model = '${escapeSqlValue(model.trim())}'`);
		if (session.trim()) parts.push(`chat_session_id = '${escapeSqlValue(session.trim())}'`);
		return parts;
	}

	function buildWhere(days: number, dimensionSql: string[]): string {
		const parts =
			days > 0
				? [
						`timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL ${days} DAYS)`,
						...dimensionSql
					]
				: [`${LLM_LIFETIME_PARTITION_HINT} TRUE`, ...dimensionSql];
		return parts.length > 0 ? parts.join(' AND ') : 'TRUE';
	}

	// Embeddings-safe WHERE. The `llm_embeddings` relation is lightweight —
	// it carries operation/provider/model but has NO agent_id/task_id/
	// chat_session_id (embeddings have no agent/task/session lineage). So the
	// shared `buildDimensionParts` (which references those columns) would
	// break an embeddings query. Apply only the dimensions embeddings
	// actually have: operation + model. Time range applies identically.
	//
	// `_op`/`_model` are unused positional args: reactive callers pass
	// `operationFilter`/`modelFilter` so Svelte's legacy dependency tracker
	// re-derives when they change (the body reads them off the closure, which
	// the tracker can't see). The `wireCalls` call site passes only `days` —
	// it re-wires off `callsFilterKey`, which already includes this string.
	function buildEmbeddingWhere(days: number, _op?: string, _model?: string): string {
		const parts =
			days > 0
				? [`timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL ${days} DAYS)`]
				: ['TRUE'];
		if (operationFilter.trim()) {
			parts.push(`operation = '${escapeSqlValue(operationFilter.trim())}'`);
		}
		if (modelFilter.trim()) {
			parts.push(`model = '${escapeSqlValue(modelFilter.trim())}'`);
		}
		return parts.join(' AND ');
	}

	$: dimensionParts = buildDimensionParts(
		agentFilter,
		taskFilter,
		operationFilter,
		modelFilter,
		chatSessionFilter
	);

	// 24h-scoped widgets clamp to min(timeRangeDays, 1). When the operator
	// picks 1d they get 1d; when they pick 7/14/30d/lifetime, the 24h
	// widgets stay at 24h so the hourly resolution is meaningful.
	$: shortWhere = buildWhere(timeRangeDays <= 0 ? 1 : Math.min(timeRangeDays, 1), dimensionParts);
	$: cumulativeWhere = buildWhere(timeRangeDays, dimensionParts);
	// Embeddings share the cumulative time range but only the operation/model
	// dimensions (see `buildEmbeddingWhere`). The short window clamps to 24h
	// for the hourly over-time chart, matching the spend timeline.
	$: embeddingCumulativeWhere = buildEmbeddingWhere(
		timeRangeDays,
		// tracked deps so Svelte re-derives on filter change (buildEmbeddingWhere
		// reads operationFilter/modelFilter internally, but the reactive block
		// only re-runs when identifiers it references change).
		operationFilter,
		modelFilter
	);
	$: embeddingShortWhere = buildEmbeddingWhere(
		timeRangeDays <= 0 ? 1 : Math.min(timeRangeDays, 1),
		operationFilter,
		modelFilter
	);
	$: selectedRangeLabel = rangeLabel(timeRangeDays);

	$: filterCount = dimensionParts.length;

	function clearAllFilters(): void {
		agentFilter = '';
		taskFilter = '';
		operationFilter = '';
		modelFilter = '';
		chatSessionFilter = '';
	}

	function rangeLabel(days: number): string {
		return days <= 0 ? 'lifetime' : `${days}d`;
	}

	// ─── "Today vs yesterday" section ──────────────────────────────────
	//
	// Fixed two-LOCAL-calendar-day window (boundaries from pulseQueries'
	// `localDayBoundaries` — the same contract as the Today page's pulse
	// band, so the band's chips deep-linking to /llm#today land on the
	// same numbers). The Range picker deliberately does NOT apply (the
	// window is definitionally two days); the dimension filters DO, like
	// every other widget. One UNION ALL query returns both days' hourly
	// buckets plus top-3-today model rows; totals are summed client-side.
	//
	// Wiring: page-level `setupLiveDataSource` (the file's dominant idiom,
	// same as `wireDistinct`) — so bearer authentication and the bubbled
	// `magician:dashboard-refresh` event behave exactly like the sibling
	// panels. Because the boundaries are inlined epoch-ms literals, the
	// SQL is rebuilt reactively (filters, Refresh) and the controller is
	// re-wired only when the string actually changes — a same-day Refresh
	// reuses the wired controller via the bubbled event; a cross-midnight
	// Refresh re-anchors the day boundaries.

	interface TodayVsYesterday {
		spendToday: number;
		spendYesterday: number;
		callsToday: number;
		callsYesterday: number;
		tokensToday: number;
		tokensYesterday: number;
		/** 24 buckets each, indexed by local hour-of-day; unfilled hours 0. */
		todayHourlySpend: number[];
		yesterdayHourlySpend: number[];
		/** Last hour index included in today's curve (current hour at parse
		 *  time) — today's series must END here, not zero-tail to 23. */
		todayEndHour: number;
		topModels: TodayModelComparison[];
	}

	interface TodayModelComparison {
		model: string;
		spendToday: number;
		spendYesterday: number;
		callsToday: number;
		callsYesterday: number;
		tokensToday: number;
		tokensYesterday: number;
	}

	interface TodayChip {
		id: string;
		label: string;
		value: string;
		delta: string;
		tone: PulseTone;
		aria: string;
	}

	let pageMounted = false;
	interface GovernedOverview {
		logical_calls: number;
		provider_attempts: number;
		capture_gaps: number;
		known_missing_fact_revisions: number;
		unclassified_transport_events_lost: number;
		validation_attempted_calls: number;
		valid_contract_calls: number;
		captured_calls: number;
		usage_observed_calls: number;
		cost_observed_calls: number;
		average_queue_wait_ms: number | null;
		average_local_prep_ms: number | null;
		average_provider_execution_ms: number | null;
		average_validation_ms: number | null;
		average_ttft_ms: number | null;
		average_latency_ms: number | null;
	}

	interface GovernedOverviewEnvelope {
		freshness: { source_latest_at_ms: number | null; stale: boolean };
		coverage: { eligible: number | null; observed: number | null; missing: number | null };
		warnings: string[];
		data: GovernedOverview;
	}

	let governedOverview: GovernedOverviewEnvelope | null = null;
	let governedOverviewError: string | null = null;
	let governedOverviewLoading = false;
	let governedOverviewAbort: AbortController | null = null;
	$: governedRangeDays = Math.min(31, Math.max(1, timeRangeDays || 31));

	/**
	 * The governed LLM endpoints carry the real cause in the body
	 * (`{"error": "…"}`). Throwing `HTTP ${status}` discarded it, so a
	 * cross-dataset integrity rejection and a contended DuckDB guard were
	 * indistinguishable from a genuinely malformed request.
	 */
	async function readGovernedErrorMessage(response: Response): Promise<string> {
		try {
			const body = (await response.json()) as { error?: unknown };
			if (typeof body?.error === 'string' && body.error.trim()) return body.error;
		} catch {
			// Non-JSON or empty body — fall through to the status line.
		}
		return `HTTP ${response.status}`;
	}

	async function loadGovernedOverview(days: number): Promise<void> {
		governedOverviewAbort?.abort();
		const controller = new AbortController();
		governedOverviewAbort = controller;
		governedOverviewLoading = true;
		const toMs = Date.now();
		const fromMs = toMs - days * 24 * 60 * 60 * 1000;
		try {
			const response = await fetch(
				`/api/magician/v2/analytics/llm/overview?from_ms=${fromMs}&to_ms=${toMs}`,
				{ signal: controller.signal }
			);
			if (!response.ok) throw new Error(await readGovernedErrorMessage(response));
			governedOverview = (await response.json()) as GovernedOverviewEnvelope;
			governedOverviewError = null;
		} catch (error) {
			if (controller.signal.aborted) return;
			governedOverviewError = error instanceof Error ? error.message : 'Unknown error';
		} finally {
			if (governedOverviewAbort === controller) governedOverviewLoading = false;
		}
	}

	function governedRatio(numerator: number, denominator: number): string {
		return denominator > 0 ? `${((100 * numerator) / denominator).toFixed(1)}%` : '—';
	}

	function governedMs(value: number | null): string {
		if (value == null) return '—';
		return value >= 1000 ? `${(value / 1000).toFixed(2)}s` : `${Math.round(value)}ms`;
	}

	$: if (pageMounted && activeTab === 'health' && lastRefreshed && governedRangeDays) {
		void loadGovernedOverview(governedRangeDays);
	}
	let todaySection: TodayVsYesterday | null = null;
	let todaySectionError: string | null = null;
	let todaySectionController: LiveDataSourceController | null = null;
	let todaySectionWiredSql = '';
	let vibeDevCostLoading = false;
	let vibeDevCostError: string | null = null;
	let vibeDevCostsByProject = new Map<string, VibeDevProjectCost>();

	const vibeDevCostRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				vibeDevCostLoading = true;
			},
			onRows: ({ records }) => {
				vibeDevCostsByProject = costByProject(vibeDevProjects, records);
				vibeDevCostError = null;
			},
			onError: (error) => {
				vibeDevCostError = error.message;
			},
			onSettled: () => {
				vibeDevCostLoading = false;
			}
		})
	);

	// ─── Embeddings summary section ─────────────────────────────────────
	//
	// Aggregate widgets over the content-free `llm_embeddings` relation —
	// the separate-lane counterpart to the LLM-ops aggregates above. The
	// totals card is wired here (parsed client-side into count/vectors/
	// tokens); by-model, by-operation, and over-time widgets bind their SQL
	// straight to chart components. All go through the embeddings endpoint
	// via `kind: 'llm_embeddings_sql'`, and follow the same time range +
	// (operation/model) filter as the rest of the page.
	interface EmbeddingTotals {
		batches: number;
		vectors: number;
		inputTokens: number;
	}
	let embeddingTotals: EmbeddingTotals | null = null;
	let embeddingTotalsError: string | null = null;
	let embeddingTotalsLoading = false;
	let embeddingHasData = false;

	const embeddingsRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				embeddingTotalsLoading = true;
			},
			onRows: ({ records }) => {
				const row = records[0] ?? {};
				embeddingTotals = {
					batches: cellNumber(row.batches),
					vectors: cellNumber(row.vectors),
					inputTokens: cellNumber(row.input_tokens)
				};
				embeddingHasData = embeddingTotals.batches > 0;
				embeddingTotalsError = null;
			},
			onError: (error) => {
				embeddingTotalsError = error.message;
			},
			onSettled: () => {
				embeddingTotalsLoading = false;
			}
		})
	);

	// Exclude chunk bookkeeping, but retain harness estimates: their underlying
	// physical attempts are unavailable and are not recorded separately.
	const REAL_CALL_PREDICATE = "(COALESCE(provider_attempt_count, 1) <> 0 OR response_kind = 'harness_aggregate')";

	function buildTodayVsYesterdaySql(b: LocalDayBoundaries, dimensionSql: string[]): string {
		const filters = dimensionSql.length > 0 ? ` AND ${dimensionSql.join(' AND ')}` : '';
		const today = `timestamp_ms >= ${b.todayStartMs} AND timestamp_ms < ${b.tomorrowStartMs}`;
		const yesterday = `timestamp_ms >= ${b.yesterdayStartMs} AND timestamp_ms < ${b.todayStartMs}`;
		const measures =
			`COALESCE(SUM(cost_usd), 0) AS spend, COUNT(*) AS calls, ` +
			`COALESCE(SUM(input_tokens), 0) + COALESCE(SUM(output_tokens), 0) AS tokens`;
		// Hour buckets are hours-since-LOCAL-midnight (half-hour UTC offsets
		// like IST break UTC-aligned `timestamp_ms / 3600000` bucketing).
		return (
			`WITH scoped AS (` +
			`SELECT *, COALESCE(NULLIF(model, ''), 'unknown') AS model_key ` +
			`FROM llm_calls WHERE timestamp_ms >= ${b.yesterdayStartMs} AND timestamp_ms < ${b.tomorrowStartMs} ` +
			`AND ${REAL_CALL_PREDICATE}${filters}` +
			`), top_today_models AS (` +
			`SELECT model_key FROM scoped WHERE ${today} GROUP BY model_key ` +
			`ORDER BY COUNT(*) DESC, COALESCE(SUM(cost_usd), 0) DESC, model_key LIMIT 3` +
			`) ` +
			`SELECT 'today' AS day, ` +
			`CAST(FLOOR((timestamp_ms - ${b.todayStartMs}) / 3600000.0) AS INTEGER) AS hour, NULL::VARCHAR AS model, ${measures} ` +
			`FROM scoped WHERE ${today} GROUP BY 2 ` +
			`UNION ALL ` +
			`SELECT 'yesterday' AS day, ` +
			`CAST(FLOOR((timestamp_ms - ${b.yesterdayStartMs}) / 3600000.0) AS INTEGER) AS hour, NULL::VARCHAR AS model, ${measures} ` +
			`FROM scoped WHERE ${yesterday} GROUP BY 2 ` +
			`UNION ALL ` +
			`SELECT 'today_model' AS day, NULL::INTEGER AS hour, model_key AS model, ${measures} ` +
			`FROM scoped WHERE ${today} AND model_key IN (SELECT model_key FROM top_today_models) GROUP BY model_key ` +
			`UNION ALL ` +
			`SELECT 'yesterday_model' AS day, NULL::INTEGER AS hour, model_key AS model, ${measures} ` +
			`FROM scoped WHERE ${yesterday} AND model_key IN (SELECT model_key FROM top_today_models) GROUP BY model_key`
		);
	}

	/** Defensive numeric coercion — BIGINT sums can arrive stringified or as DuckDB objects. */
	function cellNumber(value: unknown): number {
		const num = parseNumericDuckDbValue(value);
		return num === undefined ? 0 : num;
	}

	function parseTodayVsYesterday(
		records: Array<Record<string, unknown>>,
		boundaries: LocalDayBoundaries
	): TodayVsYesterday {
		const out: TodayVsYesterday = {
			spendToday: 0,
			spendYesterday: 0,
			callsToday: 0,
			callsYesterday: 0,
			tokensToday: 0,
			tokensYesterday: 0,
			todayHourlySpend: new Array(24).fill(0),
			yesterdayHourlySpend: new Array(24).fill(0),
			todayEndHour: 0,
			topModels: []
		};
		const models = new Map<string, TodayModelComparison>();
		const modelRow = (model: string): TodayModelComparison => {
			const key = model.trim() || 'unknown';
			let row = models.get(key);
			if (!row) {
				row = {
					model: key,
					spendToday: 0,
					spendYesterday: 0,
					callsToday: 0,
					callsYesterday: 0,
					tokensToday: 0,
					tokensYesterday: 0
				};
				models.set(key, row);
			}
			return row;
		};
		for (const r of records) {
			const day = String(r.day ?? '');
			if (
				day !== 'today' &&
				day !== 'yesterday' &&
				day !== 'today_model' &&
				day !== 'yesterday_model'
			) {
				continue;
			}
			const hour = cellNumber(r.hour);
			const spend = cellNumber(r.spend);
			const calls = cellNumber(r.calls);
			const tokens = cellNumber(r.tokens);
			if (day === 'today' || day === 'yesterday') {
				const buckets = day === 'today' ? out.todayHourlySpend : out.yesterdayHourlySpend;
				if (Number.isInteger(hour) && hour >= 0 && hour < 24) buckets[hour] += spend;
			}
			if (day === 'today') {
				out.spendToday += spend;
				out.callsToday += calls;
				out.tokensToday += tokens;
			} else if (day === 'yesterday') {
				out.spendYesterday += spend;
				out.callsYesterday += calls;
				out.tokensYesterday += tokens;
			} else if (day === 'today_model') {
				const row = modelRow(String(r.model ?? 'unknown'));
				row.spendToday += spend;
				row.callsToday += calls;
				row.tokensToday += tokens;
			} else if (day === 'yesterday_model') {
				const row = modelRow(String(r.model ?? 'unknown'));
				row.spendYesterday += spend;
				row.callsYesterday += calls;
				row.tokensYesterday += tokens;
			}
		}
		out.topModels = Array.from(models.values())
			.filter((row) => row.callsToday > 0)
			.sort(
				(a, b) =>
					b.callsToday - a.callsToday ||
					b.spendToday - a.spendToday ||
					a.model.localeCompare(b.model)
			)
			.slice(0, 3);
		out.todayEndHour = Math.min(
			23,
			Math.max(0, Math.floor((Date.now() - boundaries.todayStartMs) / 3_600_000))
		);
		return out;
	}

	function rewireTodaySection(boundaries: LocalDayBoundaries, sql: string): void {
		todaySectionController?.destroy();
		todaySectionWiredSql = sql;
		todaySectionController = setupLiveDataSource({
			dataSource: { kind: 'llm_calls_sql', sql },
			onRows: ({ records }) => {
				todaySection = parseTodayVsYesterday(records, boundaries);
				todaySectionError = null;
			},
			// Keep last-good data on a failed refresh; the error line only
			// shows when there's nothing to render at all.
			onError: (err) => {
				todaySectionError = err.message;
			}
		});
	}

	// `lastRefreshed` is in the dependency set on purpose: the Refresh
	// button re-anchors the day boundaries (matters across midnight; a
	// same-day press produces an identical SQL string → no rewire, and the
	// bubbled dashboard-refresh event re-runs the wired controller).
	$: if (pageMounted && activeTab === 'cost' && lastRefreshed) {
		const boundaries = localDayBoundaries(new Date());
		const sql = buildTodayVsYesterdaySql(boundaries, dimensionParts);
		if (sql !== todaySectionWiredSql) rewireTodaySection(boundaries, sql);
	}

	// ─── Account-wide spend breakdown (Today / 7d / 30d) ───────────────────
	//
	// A fixed cost headline at the top of the page: total $ per window plus a
	// by-operation and by-model breakdown. Deliberately INDEPENDENT of the
	// filter bar (it is the "where did the money go" overview) — the filters
	// keep driving the exploratory widgets below. The six queries
	// ({today,7d,30d} x {operation,model}) auto-batch into one /query_batch
	// request. "Today" re-anchors its local-day boundaries across midnight;
	// 7d/30d slide with now() on every refresh. Pure SQL + parsing live in
	// `$lib/llm/spendBreakdown` (unit-tested).

	const SPEND_WINDOWS: Array<{ window: SpendWindow; label: string }> = [
		{ window: 'today', label: 'Today' },
		{ window: '7d', label: 'Last 7 days' },
		{ window: '30d', label: 'Last 30 days' }
	];
	const SPEND_TOP_N = 6;

	let spendRowsByKey: Record<string, SpendRow[]> = {};
	let spendErrorByKey: Record<string, string | null> = {};
	let spendControllers: LiveDataSourceController[] = [];
	let spendWiredDay = 0;
	let spendLoadedOnce = false;

	function rewireSpendBreakdown(boundaries: LocalDayBoundaries): void {
		for (const controller of spendControllers) controller.destroy();
		spendControllers = [];
		for (const spec of buildSpendBreakdownQueries(boundaries)) {
			const key = `${spec.window}:${spec.dimension}`;
			spendControllers.push(
				setupLiveDataSource({
					dataSource: { kind: 'llm_calls_sql', sql: spec.sql },
					// Keep last-good rows on a failed refresh; a window only shows
					// its error line when it has nothing at all to render.
					onRows: ({ records }) => {
						spendRowsByKey = { ...spendRowsByKey, [key]: parseSpendRows(records) };
						spendErrorByKey = { ...spendErrorByKey, [key]: null };
						spendLoadedOnce = true;
					},
					onError: (err) => {
						spendErrorByKey = { ...spendErrorByKey, [key]: err.message };
						spendLoadedOnce = true;
					}
				})
			);
		}
	}

	// Re-anchor only when the local day changes; a same-day Refresh reuses the
	// wired controllers via the bubbled dashboard-refresh event, and 7d/30d use
	// now() so they always slide forward.
	$: if (pageMounted && activeTab === 'cost' && lastRefreshed) {
		const boundaries = localDayBoundaries(new Date());
		if (boundaries.todayStartMs !== spendWiredDay) {
			spendWiredDay = boundaries.todayStartMs;
			rewireSpendBreakdown(boundaries);
		}
	}

	// ─── Per-tab refresh ───────────────────────────────────────────────
	//
	// Scoped on purpose. The obvious implementation — bump `lastRefreshed` —
	// would be wrong: it is a tracked dependency of the calls, governed,
	// today, and spend reactive blocks, so refreshing Cost would re-run
	// Health's queries too, which is the load this restructure exists to
	// remove. The same goes for dispatching `magician:dashboard-refresh`,
	// which every live controller on the page listens for on `window`.
	//
	// So a tab refresh does two scoped things instead:
	//   1. re-runs that tab's own script-level controllers directly, and
	//   2. bumps that tab's token, which re-keys its panel so the widgets
	//      that bind `dataSource` inline remount and refetch.
	// DashboardChrome's own Refresh stays the deliberate refresh-everything
	// control, and is the only thing that still moves `lastRefreshed`.
	//
	// Debounced on the LEADING edge, with a cooldown: the first press runs
	// immediately and the button then locks for `REFRESH_COOLDOWN_MS`.
	// Presses during the cooldown are coalesced into exactly one trailing run
	// so a press is never silently dropped, however many times it lands.
	//
	// A trailing debounce would be the wrong shape here. A refresh button
	// wants to react to the press, and delaying the single-press case — which
	// is nearly every press — buys nothing; the burst protection comes from
	// the cooldown, not from the delay.
	const REFRESH_COOLDOWN_MS = 1_200;
	let tabRefreshToken: Record<TabId, number> = { cost: 0, usage: 0, health: 0 };
	let tabRefreshPending: Record<TabId, boolean> = { cost: false, usage: false, health: false };
	let tabRefreshQueued: Record<TabId, boolean> = { cost: false, usage: false, health: false };
	let refreshTimers: Partial<Record<TabId, ReturnType<typeof setTimeout>>> = {};

	function clearRefreshTimers(): void {
		for (const timer of Object.values(refreshTimers)) {
			if (timer) clearTimeout(timer);
		}
		refreshTimers = {};
	}

	function requestTabRefresh(tab: TabId): void {
		if (tabRefreshPending[tab]) {
			tabRefreshQueued = { ...tabRefreshQueued, [tab]: true };
			return;
		}
		runTabRefresh(tab);
		tabRefreshPending = { ...tabRefreshPending, [tab]: true };
		refreshTimers[tab] = setTimeout(() => {
			delete refreshTimers[tab];
			tabRefreshPending = { ...tabRefreshPending, [tab]: false };
			if (!tabRefreshQueued[tab]) return;
			tabRefreshQueued = { ...tabRefreshQueued, [tab]: false };
			requestTabRefresh(tab);
		}, REFRESH_COOLDOWN_MS);
	}

	function runTabRefresh(tab: TabId): void {
		if (tab === 'cost') {
			// Re-anchor the local day first: across midnight the wired SQL is
			// stale, and rewiring refetches on its own.
			const boundaries = localDayBoundaries(new Date());
			if (boundaries.todayStartMs !== spendWiredDay) {
				spendWiredDay = boundaries.todayStartMs;
				rewireSpendBreakdown(boundaries);
			} else {
				for (const controller of spendControllers) void controller.refresh();
			}
			const sql = buildTodayVsYesterdaySql(boundaries, dimensionParts);
			if (sql !== todaySectionWiredSql) rewireTodaySection(boundaries, sql);
			else void todaySectionController?.refresh();
		} else if (tab === 'usage') {
			// Back to page one: a refresh that kept a deep offset would show a
			// stale slice of a list that has moved.
			callsOffset = 0;
			wireCalls(true);
			void vibeDevCostRewirer.controller?.refresh();
			void embeddingsRewirer.controller?.refresh();
		} else {
			void loadGovernedOverview(governedRangeDays);
		}
		tabRefreshToken = { ...tabRefreshToken, [tab]: tabRefreshToken[tab] + 1 };
	}

	interface SpendRollup {
		shown: SpendRow[];
		moreCount: number;
		moreSpend: number;
	}
	interface SpendWindowView {
		window: SpendWindow;
		label: string;
		total: number;
		calls: number;
		byOperation: SpendRollup;
		byModel: SpendRollup;
		hardError: string | null;
	}

	$: spendViews = SPEND_WINDOWS.map(({ window, label }): SpendWindowView => {
		const opRows = spendRowsByKey[`${window}:operation`] ?? [];
		const modelRows = spendRowsByKey[`${window}:model`] ?? [];
		const opErr = spendErrorByKey[`${window}:operation`] ?? null;
		const modelErr = spendErrorByKey[`${window}:model`] ?? null;
		// operation- and model-sums cover the same paid calls, so either gives
		// the window total; fall back to model rows if the operation query erred.
		const summary = summarizeSpend(opRows.length ? opRows : modelRows);
		return {
			window,
			label,
			total: summary.totalSpend,
			calls: summary.totalCalls,
			byOperation: rollupSpendRows(opRows, SPEND_TOP_N),
			byModel: rollupSpendRows(modelRows, SPEND_TOP_N),
			// A window is a hard error only when BOTH dimensions failed and there
			// is no last-good data to keep showing.
			hardError:
				opErr && modelErr && opRows.length === 0 && modelRows.length === 0 ? opErr : null
		};
	});

	// Percent width for a row's proportional bar, relative to the window total
	// (min 2% so a nonzero row is always visible; guards divide-by-zero).
	function spendBarPct(spend: number, total: number): number {
		return total > 0 ? Math.max(2, Math.min(100, (spend * 100) / total)) : 0;
	}

	// Keep sub-cent Decision Model charges visible in spend summaries.
	function formatSpendRow(spend: number): string {
		return spend > 0 && spend < 0.01 ? modelCost(spend) : formatSpend(spend);
	}

	const compactTokens = new Intl.NumberFormat(undefined, {
		notation: 'compact',
		maximumFractionDigits: 1
	});

	function buildTodayChips(s: TodayVsYesterday): TodayChip[] {
		const spendValue = formatSpend(s.spendToday);
		const spendYday = formatSpend(s.spendYesterday);
		const callsDelta = formatDelta(s.callsToday, s.callsYesterday, 'count');
		const tokensDelta = formatDelta(s.tokensToday, s.tokensYesterday, 'count');
		return [
			{
				id: 'spend',
				label: 'Spend today',
				value: spendValue,
				delta: `vs ${spendYday} yday`,
				// Inverted polarity — more spend than yesterday tints bad.
				tone: spendTone(s.spendToday, s.spendYesterday),
				aria: `Spend: ${spendValue} today, versus ${spendYday} yesterday`
			},
			{
				id: 'calls',
				label: 'Calls today',
				value: s.callsToday.toLocaleString(),
				delta: withYday(callsDelta),
				// Neutral on purpose: fewer calls isn't "good" the way lower
				// spend is — a quiet day, a cached day, and a broken day all
				// look identical. Same stance for tokens below.
				tone: 'neutral',
				aria: `Calls: ${s.callsToday.toLocaleString()} today${deltaAria(callsDelta)}`
			},
			{
				id: 'tokens',
				label: 'Tokens today',
				value: compactTokens.format(s.tokensToday),
				delta: withYday(tokensDelta),
				tone: 'neutral',
				aria: `Tokens: ${compactTokens.format(s.tokensToday)} today${deltaAria(tokensDelta)}`
			}
		];
	}

	$: todayChips = todaySection ? buildTodayChips(todaySection) : [];

	function modelShareLabel(model: TodayModelComparison, totalCalls: number): string {
		if (totalCalls <= 0) return '0%';
		const pct = (model.callsToday * 100) / totalCalls;
		return `${pct >= 10 ? Math.round(pct) : pct.toFixed(1)}%`;
	}

	function modelDeltaLabel(model: TodayModelComparison): string {
		return withYday(formatDelta(model.callsToday, model.callsYesterday, 'count')) || 'flat';
	}

	function cumulativePoints(buckets: number[], endHour: number): Array<{ x: number; y: number }> {
		const pts: Array<{ x: number; y: number }> = [];
		let sum = 0;
		for (let h = 0; h <= endHour && h < buckets.length; h++) {
			sum += buckets[h];
			pts.push({ x: h, y: sum });
		}
		return pts;
	}

	// Two same-hour-axis series: yesterday runs the full day, today's ENDS
	// at the current hour — no zero-tail pretending the day is over. An
	// all-quiet pair renders LineChart's "No data" state instead of two
	// flat zero lines.
	$: todayChartSeries =
		todaySection && (todaySection.callsToday > 0 || todaySection.callsYesterday > 0)
			? [
					{
						name: 'Today',
						points: cumulativePoints(todaySection.todayHourlySpend, todaySection.todayEndHour)
					},
					{
						name: 'Yesterday',
						points: cumulativePoints(todaySection.yesterdayHourlySpend, 23)
					}
				]
			: [];

	// ─── KPI + chart queries ───────────────────────────────────────────
	//
	// Every SQL string is reactive on filter state. The retry-rate and
	// avg-latency widgets used to be hardcoded at 24h; they now follow
	// the global time range so wider/lifetime views of those KPIs are
	// meaningful when the operator picks a wider window.

	// Hourly spend timeline always at 24h resolution; longer windows
	// keep the same 24-bucket chart so the visual stays comparable
	// across time-range changes.
	$: spendTimelineSql =
		`SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, SUM(cost_usd) AS spend ` +
		`FROM llm_calls WHERE ${shortWhere} GROUP BY 1 ORDER BY 1`;

	$: topAgentsSql =
		`SELECT agent_id, SUM(cost_usd) AS spend FROM llm_calls ` +
		`WHERE ${cumulativeWhere} AND agent_id IS NOT NULL ` +
		`GROUP BY agent_id ORDER BY spend DESC LIMIT 10`;

	$: topOperationsSql =
		`SELECT operation, SUM(cost_usd) AS spend FROM llm_calls ` +
		`WHERE ${cumulativeWhere} AND operation IS NOT NULL AND operation != '' ` +
		`GROUP BY operation ORDER BY spend DESC LIMIT 10`;

	$: cacheHitsByModelSql =
		`SELECT model, SUM(cache_read_tokens) * 1.0 / NULLIF(SUM(CASE WHEN cache_read_tokens IS NOT NULL THEN input_tokens END), 0) AS hit_ratio ` +
		`FROM llm_calls WHERE ${cumulativeWhere} AND input_tokens > 0 ` +
		`GROUP BY model HAVING SUM(input_tokens) > 100 ORDER BY hit_ratio DESC LIMIT 10`;

	$: latencyTableSql =
		`SELECT operation, COUNT(*) AS calls, ` +
		`  quantile_cont(latency_ms, 0.5) AS p50_ms, ` +
		`  quantile_cont(latency_ms, 0.95) AS p95_ms, ` +
		`  quantile_cont(latency_ms, 0.99) AS p99_ms ` +
		`FROM llm_calls WHERE ${shortWhere} AND operation IS NOT NULL ` +
		`GROUP BY operation ORDER BY p95_ms DESC LIMIT 20`;

	// ─── Cache performance queries ─────────────────────────────────────
	// All reactive on the same filter state.

	$: cacheHitOverallSql =
		`SELECT SUM(cache_read_tokens) * 100.0 / NULLIF(SUM(CASE WHEN cache_read_tokens IS NOT NULL THEN input_tokens END), 0) ` +
		`FROM llm_calls WHERE ${cumulativeWhere} AND success = true`;

	$: cacheReadTokensSql = `SELECT SUM(cache_read_tokens) FROM llm_calls WHERE ${cumulativeWhere} AND success = true`;

	$: cacheWriteTokensSql = `SELECT SUM(cache_creation_tokens) FROM llm_calls WHERE ${cumulativeWhere} AND success = true`;

	$: avgTtftSql =
		`SELECT ROUND(AVG(ttft_ms), 0) FROM llm_calls ` +
		`WHERE ${shortWhere} AND ttft_ms IS NOT NULL AND success = true`;

	$: cacheHitTimelineSql =
		`SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, ` +
		`  SUM(cache_read_tokens) * 100.0 / NULLIF(SUM(CASE WHEN cache_read_tokens IS NOT NULL THEN input_tokens END), 0) AS hit_pct ` +
		`FROM llm_calls WHERE ${shortWhere} AND success = true AND input_tokens > 0 ` +
		`GROUP BY 1 ORDER BY 1`;

	$: cacheHitByOperationSql =
		`SELECT operation, ` +
		`  ROUND(SUM(cache_read_tokens) * 100.0 / NULLIF(SUM(CASE WHEN cache_read_tokens IS NOT NULL THEN input_tokens END), 0), 1) AS hit_pct ` +
		`FROM llm_calls WHERE ${cumulativeWhere} AND success = true AND operation IS NOT NULL AND operation != '' ` +
		`GROUP BY operation HAVING SUM(input_tokens) > 1000 ORDER BY hit_pct DESC LIMIT 15`;

	$: cacheHitByAgentSql =
		`SELECT agent_id, ` +
		`  ROUND(SUM(cache_read_tokens) * 100.0 / NULLIF(SUM(CASE WHEN cache_read_tokens IS NOT NULL THEN input_tokens END), 0), 1) AS hit_pct ` +
		`FROM llm_calls WHERE ${cumulativeWhere} AND success = true AND agent_id IS NOT NULL ` +
		`GROUP BY agent_id HAVING SUM(input_tokens) > 1000 ORDER BY hit_pct DESC LIMIT 15`;

	$: tokenBreakdownSql =
		`SELECT model, ` +
		`  SUM(input_tokens - cache_read_tokens - cache_creation_tokens) AS uncached_input, ` +
		`  SUM(cache_read_tokens) AS cache_read, ` +
		`  SUM(cache_creation_tokens) AS cache_write, ` +
		`  SUM(CASE WHEN cache_read_tokens IS NULL OR cache_creation_tokens IS NULL THEN input_tokens END) AS cache_unknown_input, ` +
		`  SUM(output_tokens) AS output ` +
		`FROM llm_calls WHERE ${cumulativeWhere} AND success = true AND model IS NOT NULL AND model != '' ` +
		`GROUP BY model HAVING SUM(input_tokens) > 1000 ORDER BY SUM(input_tokens) DESC LIMIT 12`;

	$: ttftTableSql =
		`SELECT operation, COUNT(*) AS calls, ` +
		`  quantile_cont(ttft_ms, 0.5) AS p50_ms, ` +
		`  quantile_cont(ttft_ms, 0.95) AS p95_ms, ` +
		`  quantile_cont(ttft_ms, 0.99) AS p99_ms ` +
		`FROM llm_calls WHERE ${shortWhere} AND ttft_ms IS NOT NULL AND operation IS NOT NULL ` +
		`GROUP BY operation ORDER BY p95_ms DESC LIMIT 20`;

	$: vibeDevProjects = $vibeDevProjectStore.projects;
	$: vibeDevTasks = [
		...$taskStore.tasks.filter(isVibeDevTask),
		...$taskStore.vibedevInternalTasks.filter(isVibeDevTask)
	];
	$: vibeDevAttributions = buildVibeDevProjectAttributions(vibeDevProjects, vibeDevTasks);
	$: vibeDevProjectRollupSql = buildVibeDevProjectRollupSql(
		vibeDevAttributions,
		timeRangeDays,
		dimensionParts
	);
	$: vibeDevModelBreakdownSql = buildVibeDevModelBreakdownSql(
		vibeDevAttributions,
		timeRangeDays,
		dimensionParts
	);
	$: if (pageMounted && activeTab === 'usage') {
		vibeDevCostRewirer.sync(
			vibeDevProjectRollupSql ? { kind: 'llm_calls_sql', sql: vibeDevProjectRollupSql } : null
		);
	}

	// Embeddings summary SQL — all over `llm_embeddings` via the embeddings
	// endpoint. Totals go through the rewirer; the chart/table widgets bind
	// their SQL directly to the components (kind: 'llm_embeddings_sql').
	$: embeddingTotalsSql = buildEmbeddingTotalsSql(embeddingCumulativeWhere);
	$: embeddingByModelSql = buildEmbeddingByModelSql(embeddingCumulativeWhere);
	$: embeddingByOperationSql = buildEmbeddingByOperationSql(embeddingCumulativeWhere);
	$: embeddingOverTimeSql = buildEmbeddingOverTimeSql(embeddingShortWhere);
	$: if (pageMounted && activeTab === 'usage') {
		embeddingsRewirer.sync({ kind: 'llm_embeddings_sql', sql: embeddingTotalsSql });
	}
	$: vibeDevProjectRows = buildVibeDevProjectRows(vibeDevProjects, vibeDevCostsByProject);
	$: vibeDevTotal = summarizeVibeDevCosts(vibeDevProjectRows);

	// Ad-hoc group-by widget. `groupByDim` is constrained to a known set
	// of column names (no user-supplied identifier), so direct
	// interpolation is safe — the typed union above is the allow-list.
	// The HAVING threshold scales with the dimension's expected
	// cardinality: high-cardinality dims (task_id, chat_session_id) use
	// `> 100` to avoid hiding most rows; low-cardinality dims
	// (operation, model) keep `> 1000` to suppress one-off noise.
	$: groupByThreshold =
		groupByDim === 'task_id' ||
		groupByDim === 'chat_session_id' ||
		groupByDim === 'agent_id'
			? 100
			: 1000;
	$: cacheHitByGroupSql =
		`SELECT ${groupByDim} AS bucket, ` +
		`  ROUND(SUM(cache_read_tokens) * 100.0 / NULLIF(SUM(CASE WHEN cache_read_tokens IS NOT NULL THEN input_tokens END), 0), 1) AS hit_pct, ` +
		`  SUM(input_tokens) AS total_input, ` +
		`  SUM(cache_read_tokens) AS total_cached ` +
		`FROM llm_calls WHERE ${cumulativeWhere} AND success = true AND ${groupByDim} IS NOT NULL AND CAST(${groupByDim} AS VARCHAR) != '' ` +
		`GROUP BY ${groupByDim} HAVING SUM(input_tokens) > ${groupByThreshold} ` +
		`ORDER BY hit_pct DESC LIMIT 20`;

	function groupByLabel(dim: string): string {
		switch (dim) {
			case 'operation':
				return 'Operation';
			case 'agent_id':
				return 'Agent';
			case 'model':
				return 'Model';
			case 'task_id':
				return 'Task';
			case 'chat_session_id':
				return 'Chat session';
			case 'profile':
				return 'Profile';
			case 'provider':
				return 'Provider';
			default:
				return dim;
		}
	}

	function buildVibeDevProjectRows(
		projects: VibeDevProject[],
		costs: Map<string, VibeDevProjectCost>
	): Array<{ project: VibeDevProject; cost: VibeDevProjectCost }> {
		return projects
			.map((project) => ({
				project,
				cost: costs.get(project.project_id) ?? emptyProjectCost(project.project_id)
			}))
			.sort(
				(a, b) =>
					b.cost.costUsd - a.cost.costUsd ||
					b.cost.calls - a.cost.calls ||
					b.project.updated_at_ms - a.project.updated_at_ms
			);
	}

	function summarizeVibeDevCosts(
		rows: Array<{ project: VibeDevProject; cost: VibeDevProjectCost }>
	): VibeDevTotalCost {
		return rows.reduce<VibeDevTotalCost>(
			(total, { cost }) => ({
				projects: total.projects + (cost.calls > 0 ? 1 : 0),
				calls: total.calls + cost.calls,
				costUsd: total.costUsd + cost.costUsd,
				inputTokens: total.inputTokens + cost.inputTokens,
				outputTokens: total.outputTokens + cost.outputTokens,
				reasoningTokens: total.reasoningTokens + cost.reasoningTokens,
				cacheReadTokens: total.cacheReadTokens + cost.cacheReadTokens,
				cacheCreationTokens: total.cacheCreationTokens + cost.cacheCreationTokens
			}),
			{
				projects: 0,
				calls: 0,
				costUsd: 0,
				inputTokens: 0,
				outputTokens: 0,
				reasoningTokens: 0,
				cacheReadTokens: 0,
				cacheCreationTokens: 0
			}
		);
	}
</script>

<svelte:head>
	<title>LLM observability</title>
</svelte:head>

<div class="llm-page">
	<DashboardChrome
		title="LLM observability"
		summary="LLM and Decision Model calls, costs, tokens, latency, and cache usage."
		{lastRefreshed}
		{permalink}
		publishedBy="system"
		on:refresh={handleRefresh}
	>
		<!-- Filter bar — applies to every query on the page. Time range +
		     5 dimension filters. Filters use <datalist> so the operator
		     can type or pick from the universe of values observed in
		     the last 30 days. The "Clear" button appears only when at
		     least one dimension filter is set. -->
		<section class="llm-filter-bar" aria-label="Filters">
			<div class="llm-filter-field">
				<label class="llm-filter-label" for="llm-filter-time">Range</label>
				<select id="llm-filter-time" bind:value={timeRangeDays} class="llm-filter-select">
					<option value={1}>Last 24h</option>
					<option value={7}>Last 7 days</option>
					<option value={14}>Last 14 days</option>
					<option value={30}>Last 30 days</option>
					<option value={0}>Lifetime</option>
				</select>
			</div>

			<div class="llm-filter-field">
				<label class="llm-filter-label" for="llm-filter-agent">Agent</label>
				<input
					id="llm-filter-agent"
					list="llm-filter-agent-options"
					bind:value={agentFilter}
					class="llm-filter-input"
					placeholder="any"
					on:focus={ensureFilterOptionsLoaded}
				/>
				<datalist id="llm-filter-agent-options">
					{#each agentOptions as opt (opt)}
						<option value={opt}>{opt}</option>
					{/each}
				</datalist>
			</div>

			<div class="llm-filter-field">
				<label class="llm-filter-label" for="llm-filter-task">Task</label>
				<input
					id="llm-filter-task"
					list="llm-filter-task-options"
					bind:value={taskFilter}
					class="llm-filter-input"
					placeholder="any"
					on:focus={ensureFilterOptionsLoaded}
				/>
				<datalist id="llm-filter-task-options">
					{#each taskOptions as opt (opt)}
						<option value={opt}>{opt}</option>
					{/each}
				</datalist>
			</div>

			<div class="llm-filter-field">
				<label class="llm-filter-label" for="llm-filter-op">Operation</label>
				<input
					id="llm-filter-op"
					list="llm-filter-op-options"
					bind:value={operationFilter}
					class="llm-filter-input"
					placeholder="any"
					on:focus={ensureFilterOptionsLoaded}
				/>
				<datalist id="llm-filter-op-options">
					{#each operationOptions as opt (opt)}
						<option value={opt}>{opt}</option>
					{/each}
				</datalist>
			</div>

			<div class="llm-filter-field">
				<label class="llm-filter-label" for="llm-filter-model">Model</label>
				<input
					id="llm-filter-model"
					list="llm-filter-model-options"
					bind:value={modelFilter}
					class="llm-filter-input"
					placeholder="any"
					on:focus={ensureFilterOptionsLoaded}
				/>
				<datalist id="llm-filter-model-options">
					{#each modelOptions as opt (opt)}
						<option value={opt}>{opt}</option>
					{/each}
				</datalist>
			</div>

			<div class="llm-filter-field">
				<label class="llm-filter-label" for="llm-filter-session">Chat session</label>
				<input
					id="llm-filter-session"
					list="llm-filter-session-options"
					bind:value={chatSessionFilter}
					class="llm-filter-input"
					placeholder="any"
					on:focus={ensureFilterOptionsLoaded}
				/>
				<datalist id="llm-filter-session-options">
					{#each sessionOptions as opt (opt)}
						<option value={opt}>{opt}</option>
					{/each}
				</datalist>
			</div>

			{#if filterCount > 0}
				<button type="button" class="llm-filter-clear" on:click={clearAllFilters}>
					Clear {filterCount} filter{filterCount === 1 ? '' : 's'}
				</button>
			{/if}
		</section>

		<!-- Tabs. The filter bar above still applies to whichever panel is
		     live; these only choose which one that is. Exactly one panel is
		     mounted at a time — switching unmounts the outgoing one, which is
		     what aborts the requests its inline widgets have in flight, since
		     those widgets own their controllers privately. Script-level
		     controllers are torn down alongside it by `teardownTab`. Each
		     panel carries its own debounced Refresh; DashboardChrome's
		     Refresh above refreshes the live panel. -->
		<nav class="llm-tabs" aria-label="LLM sections">
			{#each TABS as tab (tab.id)}
				<button
					id={`llm-tab-${tab.id}`}
					type="button"
					class="llm-tab"
					class:llm-tab--active={activeTab === tab.id}
					aria-pressed={activeTab === tab.id}
					aria-controls={activeTab === tab.id ? `llm-panel-${tab.id}` : undefined}
					title={tab.hint}
					on:click={() => openTab(tab.id)}
				>
					{tab.label}
				</button>
			{/each}
		</nav>

		{#if activeTab === 'cost'}
			<div
				id="llm-panel-cost"
				class="llm-panel"
				aria-labelledby="llm-tab-cost"
			>
				<div class="llm-panel-bar">
					<p class="llm-panel-hint">Where the money went. Account-wide spend, then filtered trend and top spenders.</p>
					<button
						type="button"
						class="llm-tab-refresh"
						on:click={() => requestTabRefresh('cost')}
						disabled={tabRefreshPending.cost}
					>
						{tabRefreshPending.cost ? 'Refreshing…' : 'Refresh'}
					</button>
				</div>
				{#key tabRefreshToken.cost}
					<section id="spend" class="llm-spend" aria-labelledby="llm-spend-title">
						<div class="llm-section-header llm-spend-header">
							<div>
								<h2 class="llm-section-title" id="llm-spend-title">Spend</h2>
								<p class="llm-section-summary">
									Recorded LLM and Decision Model cost by time window — account-wide, independent of the filters below.
								</p>
							</div>
						</div>
						<div class="llm-spend-grid">
							{#each spendViews as view (view.window)}
								<div class="llm-spend-card">
									<div class="llm-spend-card-head">
										<span class="llm-spend-window">{view.label}</span>
										{#if !spendLoadedOnce}
											<span class="llm-spend-total llm-spend-total--skeleton">—</span>
										{:else}
											<span class="llm-spend-total">{formatSpendRow(view.total)}</span>
											<span class="llm-spend-calls"
												>{view.calls.toLocaleString()} call{view.calls === 1 ? '' : 's'}</span
											>
										{/if}
									</div>

									{#if !spendLoadedOnce}
										<div class="llm-spend-dims">
											<div class="llm-spend-dim">
												<span class="llm-spend-dim-title">By operation</span>
												<span class="llm-spend-skeleton-rows" aria-hidden="true"></span>
											</div>
											<div class="llm-spend-dim">
												<span class="llm-spend-dim-title">By model</span>
												<span class="llm-spend-skeleton-rows" aria-hidden="true"></span>
											</div>
										</div>
									{:else if view.hardError}
										<p class="llm-spend-error">Couldn’t load spend for this window.</p>
									{:else if view.total === 0}
										<p class="llm-spend-empty">$0.00 · no paid calls</p>
									{:else}
										<div class="llm-spend-dims">
											{#each [{ title: 'By operation', roll: view.byOperation }, { title: 'By model', roll: view.byModel }] as dim (dim.title)}
												<div class="llm-spend-dim">
													<span class="llm-spend-dim-title">{dim.title}</span>
													{#if dim.roll.shown.length === 0}
														<span class="llm-spend-dim-empty">—</span>
													{:else}
														<ul class="llm-spend-list">
															{#each dim.roll.shown as row (row.label)}
																<li class="llm-spend-row">
																	<span class="llm-spend-row-label" title={row.label}>{row.label}</span>
																	<span class="llm-spend-row-amount">{formatSpendRow(row.spend)}</span>
																	<span class="llm-spend-bar" aria-hidden="true">
																		<span
																			class="llm-spend-bar-fill"
																			style={`width:${spendBarPct(row.spend, view.total)}%`}
																		></span>
																	</span>
																</li>
															{/each}
															{#if dim.roll.moreCount > 0}
																<li class="llm-spend-row llm-spend-row--more">
																	<span class="llm-spend-row-label">+{dim.roll.moreCount} more</span>
																	<span class="llm-spend-row-amount">{formatSpendRow(dim.roll.moreSpend)}</span>
																	<span class="llm-spend-bar" aria-hidden="true"></span>
																</li>
															{/if}
														</ul>
													{/if}
												</div>
											{/each}
										</div>
									{/if}
								</div>
							{/each}
						</div>
					</section>

					<DecisionModelUsage where={buildWhere(timeRangeDays, dimensionParts)} rangeLabel={selectedRangeLabel} />

					<!-- Today vs yesterday — fixed two-local-day comparison, deep-link
					     target of the Today page's pulse band (/llm#today). Renders
					     unconditionally (not staged) so the anchor exists at nav time. -->
					<section class="llm-today" id="today" aria-label="Today vs yesterday">
						<div class="llm-section-header llm-today-header">
							<h2 class="llm-section-title">Today vs yesterday</h2>
							<p class="llm-section-summary">
								Local calendar days. Dimension filters above apply; the Range picker doesn't (the
								window is definitionally two days). Today's curve stops at the current hour; model
								rows are ranked by today's call volume.
							</p>
						</div>

						{#if todaySection}
							<div class="llm-kpis">
								{#each todayChips as chip (chip.id)}
									<div class="llm-today-chip" role="group" aria-label={chip.aria}>
										<div class="llm-today-chip-value">{chip.value}</div>
										<div class="llm-today-chip-label">{chip.label}</div>
										{#if chip.delta}
											<div
												class="llm-today-chip-delta"
												class:llm-today-chip-delta--good={chip.tone === 'good'}
												class:llm-today-chip-delta--bad={chip.tone === 'bad'}
											>
												{chip.delta}
											</div>
										{/if}
									</div>
								{/each}
							</div>

							{#if todaySection.topModels.length > 0}
								<div class="llm-widget llm-top-models" role="region" aria-labelledby="llm-top-models-title">
									<h3 class="llm-widget-title" id="llm-top-models-title">Top models today</h3>
									<div class="llm-top-model-list">
										{#each todaySection.topModels as model, idx (model.model)}
											<div class="llm-top-model-row">
												<div class="llm-top-model-name">
													<span class="llm-top-model-rank">{idx + 1}</span>
													<span class="llm-top-model-text" title={model.model}>{model.model}</span>
												</div>
												<div class="llm-top-model-metric">
													<span class="llm-top-model-value">{model.callsToday.toLocaleString()}</span>
													<span class="llm-top-model-label">calls</span>
												</div>
												<div class="llm-top-model-metric">
													<span class="llm-top-model-value">{modelShareLabel(model, todaySection.callsToday)}</span>
													<span class="llm-top-model-label">share</span>
												</div>
												<div class="llm-top-model-metric">
													<span class="llm-top-model-value">{formatSpend(model.spendToday)}</span>
													<span class="llm-top-model-label">spend</span>
												</div>
												<div class="llm-top-model-metric">
													<span class="llm-top-model-value">{modelDeltaLabel(model)}</span>
													<span class="llm-top-model-label">vs yday</span>
												</div>
											</div>
										{/each}
									</div>
								</div>
							{/if}

							<div class="llm-widget">
								<h3 class="llm-widget-title">Cumulative spend by hour — today vs yesterday</h3>
								<LineChart series={todayChartSeries} />
							</div>
						{:else if todaySectionError}
							<p class="llm-loading-status">Today vs yesterday unavailable — {todaySectionError}</p>
						{:else}
							<p class="llm-loading-status">Loading today's comparison...</p>
						{/if}
					</section>

					{#if tabLevel.cost >= 1}
						<!-- KPI row -->
						<LlmUsageOverview
							where={cumulativeWhere}
							rangeLabel={selectedRangeLabel}
							ariaLabel={`LLM usage overview for ${selectedRangeLabel}`}
						/>

						<!-- Spend timeline (full width) -->
						<section class="llm-widget">
							<h3 class="llm-widget-title">Spend over time (last 24h, hourly)</h3>
							<LineChart
								dataSource={{ kind: 'llm_calls_sql', sql: spendTimelineSql }}
								xField="bucket_ms"
								yField="spend"
							/>
						</section>
					{:else}
						<p class="llm-loading-status">Loading core observability metrics...</p>
					{/if}

					{#if tabLevel.cost >= 2}
						<!-- Two BarCharts side by side -->
						<section class="llm-pair">
							<div class="llm-widget">
								<h3 class="llm-widget-title">Top agents by spend ({selectedRangeLabel})</h3>
								<BarChart
									horizontal={true}
									dataSource={{ kind: 'llm_calls_sql', sql: topAgentsSql }}
									xField="agent_id"
									yField="spend"
								/>
							</div>
							<div class="llm-widget">
								<h3 class="llm-widget-title">Top operations by spend ({selectedRangeLabel})</h3>
								<BarChart
									horizontal={true}
									dataSource={{ kind: 'llm_calls_sql', sql: topOperationsSql }}
									xField="operation"
									yField="spend"
								/>
							</div>
						</section>

					{/if}
				{/key}
			</div>
		{/if}

		{#if activeTab === 'usage'}
			<div
				id="llm-panel-usage"
				class="llm-panel"
				aria-labelledby="llm-tab-usage"
			>
				<div class="llm-panel-bar">
					<p class="llm-panel-hint">What ran. Individual calls, embedding batches, and per-project attribution.</p>
					<button
						type="button"
						class="llm-tab-refresh"
						on:click={() => requestTabRefresh('usage')}
						disabled={tabRefreshPending.usage}
					>
						{tabRefreshPending.usage ? 'Refreshing…' : 'Refresh'}
					</button>
				</div>
				{#key tabRefreshToken.usage}

					{#if tabLevel.usage >= 1}
					<LlmCallExplorer rangeDays={governedRangeDays} refreshToken={lastRefreshed} />

					<!-- Calls — per-call, metadata-only log over the llm_calls relation.
					     Driven by the same filter bar as every widget on the page; the
					     table is content-free (no prompt/response text exists here).
					     "Load more" paginates deeper without disturbing the filters. -->
					<section class="llm-calls" aria-labelledby="llm-calls-title">
						<div class="llm-section-header llm-today-header llm-calls-header">
							<div>
								<h2 class="llm-section-title" id="llm-calls-title">Calls</h2>
								<p class="llm-section-summary">
									Individual LLM calls and embedding batches in the selected window — metadata only
									(no prompt, response, or vector content). Range and dimension filters above apply
									(embeddings honor operation + model); the most recent come first.
								</p>
							</div>
							<div
								class="llm-seg"
								role="group"
								aria-label="Call type"
							>
								{#each [{ id: 'llm', label: 'LLM ops' }, { id: 'decision', label: 'Decision Models' }, { id: 'embedding', label: 'Embeddings' }, { id: 'all', label: 'All' }] as opt (opt.id)}
									<button
										type="button"
										class="llm-seg-btn"
										class:llm-seg-btn--active={callsType === opt.id}
										aria-pressed={callsType === opt.id}
										on:click={() => (callsType = opt.id as CallsType)}
									>
										{opt.label}
									</button>
								{/each}
							</div>
						</div>
						<CallsTable rows={callsRows} loading={callsLoading} onLoadMore={loadMoreCalls} />
					</section>

					{/if}

					{#if tabLevel.usage >= 2}
					<!-- Embeddings — aggregate summary over the content-free llm_embeddings
					     relation (the separate embedding lane; not in llm_calls). Count,
					     tokens, by-model, by-operation (purpose), and vectors-over-time.
					     Follows the same time range + operation/model filters. Friendly
					     empty state until embedding capture produces data. -->
					<section class="llm-embeddings" aria-labelledby="llm-embeddings-title">
						<div class="llm-section-header">
							<h2 class="llm-section-title" id="llm-embeddings-title">Embeddings</h2>
							<p class="llm-section-summary">
								Local embedding batches ({selectedRangeLabel}) — memory-index, resurfacing, and
								procedure-index vectors. Metadata only (no vector content); local embeds cost $0.
								Operation and model filters above apply.
							</p>
						</div>

						{#if embeddingHasData}
							<section class="llm-kpis">
								<div class="llm-today-chip" role="group" aria-label="Embedding batches">
									<div class="llm-today-chip-value">{(embeddingTotals?.batches ?? 0).toLocaleString()}</div>
									<div class="llm-today-chip-label">Batches ({selectedRangeLabel})</div>
									<div class="llm-today-chip-delta">
										{(embeddingTotals?.vectors ?? 0).toLocaleString()} vectors
									</div>
								</div>
								<div class="llm-today-chip" role="group" aria-label="Embedding vectors">
									<div class="llm-today-chip-value">{compactTokens.format(embeddingTotals?.vectors ?? 0)}</div>
									<div class="llm-today-chip-label">Vectors ({selectedRangeLabel})</div>
									<div class="llm-today-chip-delta">
										{embeddingTotalsLoading ? 'refreshing' : 'across all purposes'}
									</div>
								</div>
								<div class="llm-today-chip" role="group" aria-label="Embedding input tokens">
									<div class="llm-today-chip-value">{compactTokens.format(embeddingTotals?.inputTokens ?? 0)}</div>
									<div class="llm-today-chip-label">Input tokens ({selectedRangeLabel})</div>
									<div class="llm-today-chip-delta">estimated where provider omits usage</div>
								</div>
							</section>

							<section class="llm-pair">
								<div class="llm-widget">
									<h3 class="llm-widget-title">Embed tokens by model ({selectedRangeLabel})</h3>
									<BarChart
										horizontal={true}
										dataSource={{ kind: 'llm_embeddings_sql', sql: embeddingByModelSql }}
										xField="model"
										yField="input_tokens"
									/>
								</div>
								<div class="llm-widget">
									<h3 class="llm-widget-title">Embed tokens by purpose ({selectedRangeLabel})</h3>
									<BarChart
										horizontal={true}
										dataSource={{ kind: 'llm_embeddings_sql', sql: embeddingByOperationSql }}
										xField="operation"
										yField="input_tokens"
									/>
								</div>
							</section>

							<section class="llm-widget">
								<h3 class="llm-widget-title">Vectors embedded over time (24h, hourly)</h3>
								<LineChart
									dataSource={{ kind: 'llm_embeddings_sql', sql: embeddingOverTimeSql }}
									xField="bucket_ms"
									yField="vectors"
								/>
							</section>
						{:else if embeddingTotalsError}
							<p class="llm-loading-status">Embeddings unavailable — {embeddingTotalsError}</p>
						{:else if embeddingTotalsLoading}
							<p class="llm-loading-status">Loading embedding summary…</p>
						{:else}
							<p class="llm-loading-status">
								No embedding batches recorded yet for this scope and window. Once memory-index,
								resurfacing, or procedure-index embedding runs execute, their metadata will appear
								here.
							</p>
						{/if}
					</section>

					{/if}

					{#if tabLevel.usage >= 3}
						<section class="llm-section-header">
							<h2 class="llm-section-title">VibeDev rollup</h2>
							<p class="llm-section-summary">
								Project-scoped LLM usage across loaded VibeDev projects. Attribution uses each
								project's chat session plus durable VibeDev run task ids, with loaded run
								descriptions as a legacy backfill.
							</p>
						</section>

						{#if vibeDevProjectRollupSql}
							<section class="llm-kpis">
								<div class="llm-today-chip" role="group" aria-label="VibeDev spend">
									<div class="llm-today-chip-value">{formatUsd(vibeDevTotal.costUsd)}</div>
									<div class="llm-today-chip-label">VibeDev spend ({selectedRangeLabel})</div>
									<div class="llm-today-chip-delta">
										{vibeDevTotal.projects.toLocaleString()} project{vibeDevTotal.projects === 1 ? '' : 's'}
										with calls
									</div>
								</div>
								<div class="llm-today-chip" role="group" aria-label="VibeDev calls">
									<div class="llm-today-chip-value">{vibeDevTotal.calls.toLocaleString()}</div>
									<div class="llm-today-chip-label">VibeDev calls ({selectedRangeLabel})</div>
									<div class="llm-today-chip-delta">
										{vibeDevCostLoading ? 'refreshing' : `${vibeDevAttributions.length.toLocaleString()} tracked projects`}
									</div>
								</div>
								<div class="llm-today-chip" role="group" aria-label="VibeDev tokens">
									<div class="llm-today-chip-value">{formatCompactNumber(totalTokens(vibeDevTotal))}</div>
									<div class="llm-today-chip-label">VibeDev tokens ({selectedRangeLabel})</div>
									<div class="llm-today-chip-delta">
										In {formatCompactNumber(vibeDevTotal.inputTokens)} · Out {formatCompactNumber(vibeDevTotal.outputTokens)}
									</div>
								</div>
							</section>

							<section class="llm-pair">
								<div class="llm-widget">
									<h3 class="llm-widget-title">VibeDev projects by LLM spend ({selectedRangeLabel})</h3>
									{#if vibeDevCostError}
										<p class="llm-loading-status">VibeDev project rollup unavailable — {vibeDevCostError}</p>
									{:else if vibeDevProjectRows.length === 0}
										<p class="llm-loading-status">No VibeDev projects loaded for this scope.</p>
									{:else}
										<div class="llm-vibedev-table">
											<div class="llm-vibedev-row llm-vibedev-row--head">
												<span>Project</span>
												<span>Spend</span>
												<span>Calls</span>
												<span>Tokens</span>
												<span>Top model</span>
											</div>
											{#each vibeDevProjectRows as row (row.project.project_id)}
												<div class="llm-vibedev-row">
													<span class="llm-vibedev-project" title={row.project.project_id}>
														{row.project.name}{row.project.archived ? ' (archived)' : ''}
													</span>
													<span>{formatUsd(row.cost.costUsd)}</span>
													<span>{row.cost.calls.toLocaleString()}</span>
													<span>
														{formatCompactNumber(totalTokens(row.cost))}
														<small>in {formatCompactNumber(row.cost.inputTokens)} / out {formatCompactNumber(row.cost.outputTokens)}</small>
													</span>
													<span title={modelLabel(row.cost)}>{modelLabel(row.cost)}</span>
												</div>
											{/each}
										</div>
									{/if}
								</div>
								<div class="llm-widget">
									<h3 class="llm-widget-title">VibeDev provider/model spend ({selectedRangeLabel})</h3>
									{#if vibeDevModelBreakdownSql}
										<Table dataSource={{ kind: 'llm_calls_sql', sql: vibeDevModelBreakdownSql }} />
									{:else}
										<p class="llm-loading-status">No VibeDev attribution keys are loaded yet.</p>
									{/if}
								</div>
							</section>
						{:else}
							<p class="llm-loading-status">Loading VibeDev project attribution...</p>
						{/if}

					{/if}
				{/key}
			</div>
		{/if}

		{#if activeTab === 'health'}
			<div
				id="llm-panel-health"
				class="llm-panel"
				aria-labelledby="llm-tab-health"
			>
				<div class="llm-panel-bar">
					<p class="llm-panel-hint">Whether the numbers can be trusted, and what they cost in latency.</p>
					<button
						type="button"
						class="llm-tab-refresh"
						on:click={() => requestTabRefresh('health')}
						disabled={tabRefreshPending.health}
					>
						{tabRefreshPending.health ? 'Refreshing…' : 'Refresh'}
					</button>
				</div>
				{#key tabRefreshToken.health}

					<section class="llm-governed" aria-labelledby="llm-governed-title">
						<div class="llm-section-header llm-governed-header">
							<div>
								<h2 class="llm-section-title" id="llm-governed-title">Canonical capture health</h2>
								<p class="llm-section-summary">
									Logical calls, physical attempts, timing decomposition, validation, and missing
									telemetry from the governed fact reader. Range applies; dimension filters remain in
									the exploratory panels below.
								</p>
							</div>
							{#if governedOverview}
								<span class="llm-freshness" class:llm-freshness--stale={governedOverview.freshness.stale}>
									{governedOverview.freshness.stale ? 'Stale' : 'Fresh'}
								</span>
							{/if}
						</div>

						{#if governedOverview}
							<div class="llm-governed-grid">
								<div class="llm-governed-card">
									<strong>{governedOverview.data.logical_calls.toLocaleString()}</strong>
									<span>Logical calls</span>
									<small>
										{governedRatio(governedOverview.data.usage_observed_calls, governedOverview.data.logical_calls)} usage observed ·
										{governedRatio(governedOverview.data.cost_observed_calls, governedOverview.data.logical_calls)} cost observed
									</small>
								</div>
								<div class="llm-governed-card">
									<strong>{governedOverview.data.provider_attempts.toLocaleString()}</strong>
									<span>Physical attempts</span>
									<small>{governedRatio(governedOverview.data.provider_attempts, governedOverview.data.logical_calls)} attempt load</small>
								</div>
								<div class="llm-governed-card">
									<strong>{governedRatio(governedOverview.data.valid_contract_calls, governedOverview.data.validation_attempted_calls)}</strong>
									<span>Contract-valid</span>
									<small>{governedOverview.data.valid_contract_calls.toLocaleString()} / {governedOverview.data.validation_attempted_calls.toLocaleString()} attempted</small>
								</div>
								<div class="llm-governed-card">
									<strong>{governedRatio(governedOverview.coverage.observed ?? 0, governedOverview.coverage.eligible ?? 0)}</strong>
									<span>Telemetry observed</span>
									<small>
										{governedOverview.data.known_missing_fact_revisions.toLocaleString()} known missing fact revisions ·
										{governedOverview.data.capture_gaps.toLocaleString()} classified gap units
									</small>
									{#if governedOverview.data.unclassified_transport_events_lost > 0}
										<small>{governedOverview.data.unclassified_transport_events_lost.toLocaleString()} transport envelopes unclassified</small>
									{/if}
								</div>
							</div>

							<div class="llm-timing-strip" aria-label="Average timing decomposition">
								<div><span>Queue</span><strong>{governedMs(governedOverview.data.average_queue_wait_ms)}</strong></div>
								<div><span>Local prep</span><strong>{governedMs(governedOverview.data.average_local_prep_ms)}</strong></div>
								<div><span>Provider</span><strong>{governedMs(governedOverview.data.average_provider_execution_ms)}</strong></div>
								<div><span>First token</span><strong>{governedMs(governedOverview.data.average_ttft_ms)}</strong></div>
								<div><span>Validation</span><strong>{governedMs(governedOverview.data.average_validation_ms)}</strong></div>
								<div><span>End to end</span><strong>{governedMs(governedOverview.data.average_latency_ms)}</strong></div>
							</div>
							{#if governedOverview.warnings.length > 0}
								<p class="llm-governed-warning">{governedOverview.warnings.join(' · ')}</p>
							{/if}
						{:else if governedOverviewError}
							<p class="llm-loading-status">Canonical capture health unavailable — {governedOverviewError}</p>
						{:else}
							<p class="llm-loading-status">{governedOverviewLoading ? 'Loading canonical capture health…' : 'No canonical facts yet.'}</p>
						{/if}
					</section>


					{#if tabLevel.health >= 1}
						<!-- Cache hits + latency table -->
						<section class="llm-pair">
							<div class="llm-widget">
								<h3 class="llm-widget-title">Cache hit ratio by model ({selectedRangeLabel})</h3>
								<BarChart
									horizontal={true}
									dataSource={{ kind: 'llm_calls_sql', sql: cacheHitsByModelSql }}
									xField="model"
									yField="hit_ratio"
								/>
							</div>
							<div class="llm-widget">
								<h3 class="llm-widget-title">Latency by operation (24h)</h3>
								<Table dataSource={{ kind: 'llm_calls_sql', sql: latencyTableSql }} />
							</div>
						</section>
					{/if}

					{#if tabLevel.health >= 2}
						<!-- Cache performance — dedicated section, after the cost / latency
						     story is established above. KPIs surface the overall cache health,
						     timeline + per-operation + per-agent breakdowns reveal regressions,
						     the model-token-mix bar shows which providers are caching at all,
						     and the TTFT-by-operation table closes the loop on the user-felt
						     latency win that caching enables. The ad-hoc group-by widget at
						     the bottom lets the operator pivot the breakdown along any
						     dimension carried by llm_calls. -->
						<section class="llm-section-header">
							<h2 class="llm-section-title">Cache performance</h2>
							<p class="llm-section-summary">
								Prompt-cache hit rate across the selected window. `cache_hit_pct = cache_read / input` —
								same metric as the per-turn chip on chat bubbles and the
								`chat_session_cache_summary` DuckDB view. TTFT is captured for streaming-instrumented
								callers (chat-inline today; autonomous decision is non-streaming so first-byte ≈ total).
							</p>
						</section>

						<section class="llm-kpis">
							<MetricCard
								label={`Cache hit % (${selectedRangeLabel})`}
								formatAs="percent"
								dataSource={{ kind: 'llm_calls_sql', sql: cacheHitOverallSql }}
							/>
							<MetricCard
								label={`Cached tokens (${selectedRangeLabel})`}
								formatAs="number"
								dataSource={{ kind: 'llm_calls_sql', sql: cacheReadTokensSql }}
							/>
							<MetricCard
								label={`Cache writes (${selectedRangeLabel})`}
								formatAs="number"
								dataSource={{ kind: 'llm_calls_sql', sql: cacheWriteTokensSql }}
							/>
							<MetricCard
								label="Avg TTFT (24h)"
								formatAs="number"
								dataSource={{ kind: 'llm_calls_sql', sql: avgTtftSql }}
							/>
						</section>

						<section class="llm-widget">
							<h3 class="llm-widget-title">Cache hit % over time (24h, hourly)</h3>
							<LineChart
								dataSource={{ kind: 'llm_calls_sql', sql: cacheHitTimelineSql }}
								xField="bucket_ms"
								yField="hit_pct"
							/>
						</section>
					{/if}

					{#if tabLevel.health >= 3}
						<section class="llm-pair">
							<div class="llm-widget">
								<h3 class="llm-widget-title">Cache hit % by operation ({selectedRangeLabel})</h3>
								<BarChart
									horizontal={true}
									dataSource={{ kind: 'llm_calls_sql', sql: cacheHitByOperationSql }}
									xField="operation"
									yField="hit_pct"
								/>
							</div>
							<div class="llm-widget">
								<h3 class="llm-widget-title">Cache hit % by agent ({selectedRangeLabel})</h3>
								<BarChart
									horizontal={true}
									dataSource={{ kind: 'llm_calls_sql', sql: cacheHitByAgentSql }}
									xField="agent_id"
									yField="hit_pct"
								/>
							</div>
						</section>

						<section class="llm-pair">
							<div class="llm-widget">
								<h3 class="llm-widget-title">Token mix by model ({selectedRangeLabel})</h3>
								<Table dataSource={{ kind: 'llm_calls_sql', sql: tokenBreakdownSql }} />
							</div>
							<div class="llm-widget">
								<h3 class="llm-widget-title">TTFT by operation (24h)</h3>
								<Table dataSource={{ kind: 'llm_calls_sql', sql: ttftTableSql }} />
							</div>
						</section>

						<!-- Ad-hoc group-by breakdown. Operator picks the dimension
						     (operation/agent/model/task/chat_session/profile/provider)
						     and the widget renders cache hit % per bucket within the
						     selected dimension. Same global filters apply. -->
						<section class="llm-widget">
							<div class="llm-widget-header">
								<h3 class="llm-widget-title">Cache hit % grouped by {groupByLabel(groupByDim).toLowerCase()} ({selectedRangeLabel})</h3>
								<div class="llm-filter-field llm-filter-field--inline">
									<label class="llm-filter-label" for="llm-groupby">Group by</label>
									<select id="llm-groupby" bind:value={groupByDim} class="llm-filter-select">
										<option value="operation">Operation</option>
										<option value="agent_id">Agent</option>
										<option value="model">Model</option>
										<option value="task_id">Task</option>
										<option value="chat_session_id">Chat session</option>
										<option value="profile">Profile</option>
										<option value="provider">Provider</option>
									</select>
								</div>
							</div>
							<Table dataSource={{ kind: 'llm_calls_sql', sql: cacheHitByGroupSql }} />
						</section>
					{/if}

					{#if tabLevel.health >= 4}
						<!-- Live event stream -->
						<section class="llm-widget">
							<EventStreamCard
								title="Live LLM events"
								density="compact"
								maxRows={50}
								defaultCategories={['llm', 'tool']}
								showTitle={true}
								showFilters={false}
								showPeek={true}
							/>
						</section>
					{/if}

				{/key}
			</div>
		{/if}
	</DashboardChrome>
</div>

<style>
	/* DashboardChrome's `max-width: var(--theme-container-max-width, 1320px)`
	   falls back to 1320px only if the variable is unset. Other dashboard
	   pages call `applyTheme` which writes a smaller value (often 960px)
	   onto `document.documentElement` and the override leaks across
	   navigation. Pin the chrome to the app shell's 1320px here so this
	   page is always full-width regardless of prior dashboard visits. */
	.llm-page :global(.dashboard-chrome) {
		max-width: 1320px;
	}

	.llm-kpis {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
		gap: 16px;
	}

	.llm-pair {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(360px, 1fr));
		gap: 24px;
	}

	.llm-widget {
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 20px 24px;
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 12px;
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}

	.llm-widget-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
	}

	.llm-widget-title {
		margin: 0;
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.875rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--theme-color-foreground-muted, #6B7280);
	}

	.llm-loading-status {
		margin: -4px 0 0;
		color: var(--theme-color-foreground-muted, #6B7280);
		font-family: var(--theme-font-body);
		font-size: 0.86rem;
	}

	/* Section header for "Cache performance" — separates the cost/latency
	 * widgets above from the cache-focused widgets below so the page
	 * reads as two stacked story arcs. Uses theme tokens so it recolors
	 * with the active theme. */
	.llm-section-header {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding-top: 12px;
		border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}
	.llm-section-title {
		margin: 0;
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 1.125rem;
		font-weight: 700;
		color: var(--theme-color-foreground, #111827);
	}
	.llm-section-summary {
		margin: 0;
		font-family: var(--theme-font-body);
		font-size: 0.875rem;
		line-height: 1.4;
		color: var(--theme-color-foreground-muted, #6B7280);
	}

	/* Account-wide spend breakdown — the fixed cost headline (Today / 7d / 30d).
	 * `scroll-margin-top` keeps the #spend anchor clear of the sticky filter
	 * bar, matching .llm-today. Theme tokens so it recolors with the theme. */
	.llm-spend {
		display: flex;
		flex-direction: column;
		gap: 16px;
		scroll-margin-top: 96px;
	}
	.llm-spend-grid {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 16px;
	}
	@media (max-width: 900px) {
		.llm-spend-grid {
			grid-template-columns: 1fr;
		}
	}
	.llm-spend-card {
		display: flex;
		flex-direction: column;
		gap: 14px;
		min-width: 0;
		padding: 18px 20px;
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 12px;
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}
	.llm-spend-card-head {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.llm-spend-window {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.78rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--theme-color-foreground-muted, #6b7280);
	}
	.llm-spend-total {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 1.75rem;
		font-weight: 700;
		line-height: 1.1;
		font-variant-numeric: tabular-nums;
		color: var(--theme-color-foreground, #111827);
	}
	.llm-spend-total--skeleton {
		color: var(--theme-color-foreground-muted, #9ca3af);
	}
	.llm-spend-calls {
		font-family: var(--theme-font-body);
		font-size: 0.8rem;
		color: var(--theme-color-foreground-muted, #6b7280);
	}
	.llm-spend-dims {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 16px;
	}
	@media (max-width: 480px) {
		.llm-spend-dims {
			grid-template-columns: 1fr;
		}
	}
	.llm-spend-dim {
		display: flex;
		flex-direction: column;
		gap: 8px;
		min-width: 0;
	}
	.llm-spend-dim-title {
		font-family: var(--theme-font-body);
		font-size: 0.72rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, #9ca3af);
	}
	.llm-spend-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 7px;
	}
	.llm-spend-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		grid-template-rows: auto auto;
		column-gap: 8px;
		row-gap: 3px;
		align-items: baseline;
	}
	.llm-spend-row-label {
		grid-column: 1;
		grid-row: 1;
		font-family: var(--theme-font-body);
		font-size: 0.82rem;
		color: var(--theme-color-foreground, #111827);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.llm-spend-row-amount {
		grid-column: 2;
		grid-row: 1;
		font-size: 0.82rem;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		color: var(--theme-color-foreground, #111827);
	}
	.llm-spend-bar {
		grid-column: 1 / -1;
		grid-row: 2;
		height: 4px;
		border-radius: 999px;
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.06));
		overflow: hidden;
	}
	.llm-spend-bar-fill {
		display: block;
		height: 100%;
		border-radius: 999px;
		background: var(--theme-color-accent, #6366f1);
	}
	.llm-spend-row--more .llm-spend-row-label,
	.llm-spend-row--more .llm-spend-row-amount {
		font-weight: 500;
		color: var(--theme-color-foreground-muted, #6b7280);
	}
	.llm-spend-dim-empty,
	.llm-spend-empty,
	.llm-spend-error {
		margin: 0;
		font-family: var(--theme-font-body);
		font-size: 0.82rem;
		color: var(--theme-color-foreground-muted, #6b7280);
	}
	.llm-spend-error {
		color: var(--color-error, #dc2626);
	}
	.llm-spend-skeleton-rows {
		display: block;
		height: 54px;
		border-radius: 8px;
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.05));
	}

	/* Today-vs-yesterday section — the /llm#today deep-link target. One
	 * flex column so the header, chips, and chart keep a tight internal
	 * rhythm inside the chrome's 24px child gap. `scroll-margin-top`
	 * keeps the anchored section clear of the sticky filter bar. */
	.llm-today {
		display: flex;
		flex-direction: column;
		gap: 16px;
		scroll-margin-top: 96px;
	}

	/* Calls section — same flex-column rhythm as the today section so the
	 * header and the per-call table keep a tight internal gap inside the
	 * chrome's 24px child spacing. */
	.llm-calls,
	.llm-embeddings {
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	/* Calls header carries the type toggle to the right of the title/summary. */
	.llm-calls-header {
		flex-direction: row;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
		flex-wrap: wrap;
	}

	/* Segmented control — LLM ops / Embeddings / All. Same token grammar as
	 * the filter inputs; the active segment fills with the surface color and
	 * gains an accent underline so it reads as pressed. */
	.llm-seg {
		display: inline-flex;
		flex: 0 0 auto;
		padding: 2px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		border-radius: 8px;
		background-color: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.04));
	}

	.llm-seg-btn {
		padding: 6px 14px;
		border: none;
		border-radius: 6px;
		background: transparent;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-family: var(--theme-font-body);
		font-size: 0.8rem;
		font-weight: 650;
		cursor: pointer;
	}

	.llm-seg-btn:hover {
		color: var(--theme-color-foreground, #111827);
	}

	.llm-seg-btn--active {
		background-color: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, #111827);
		box-shadow: 0 1px 2px var(--theme-color-shadow, rgba(0, 0, 0, 0.08));
	}

	.llm-seg-btn:focus-visible {
		outline: 2px solid var(--theme-color-accent, #4ecdc4);
		outline-offset: 1px;
	}

	/* Top-of-page variant of the section header: no separator rule. */
	.llm-today-header {
		border-top: none;
		padding-top: 0;
	}

	/* Delta chips — same surface idiom as .llm-widget, with the pulse
	 * band's delta-tint grammar (color-mix into the muted text color;
	 * spend polarity inverted upstream in pulseFormat). */
	.llm-today-chip {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding: 16px 20px;
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 12px;
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}

	.llm-today-chip-value {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 1.5rem;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
		color: var(--theme-color-foreground, #111827);
	}

	.llm-today-chip-label {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.7rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--theme-color-foreground-muted, #6B7280);
	}

	.llm-governed {
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 18px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 14px;
		background: var(--theme-color-surface, #fff);
	}

	.llm-governed-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
	}

	.llm-freshness {
		flex: 0 0 auto;
		padding: 4px 9px;
		border-radius: 999px;
		background: color-mix(in srgb, var(--color-success, #15803d) 13%, transparent);
		color: var(--color-success, #15803d);
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	.llm-freshness--stale {
		background: color-mix(in srgb, var(--color-warning, #b45309) 14%, transparent);
		color: var(--color-warning, #b45309);
	}

	.llm-governed-grid {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: 10px;
	}

	.llm-governed-card {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 0;
		padding: 14px;
		border-radius: 10px;
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.035));
	}

	.llm-governed-card strong {
		font-size: clamp(1.25rem, 2vw, 1.8rem);
		font-variant-numeric: tabular-nums;
		color: var(--theme-color-foreground, #111827);
	}

	.llm-governed-card span,
	.llm-timing-strip span {
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, #6b7280);
	}

	.llm-governed-card small {
		color: var(--theme-color-foreground-muted, #6b7280);
		font-variant-numeric: tabular-nums;
	}

	.llm-timing-strip {
		display: grid;
		grid-template-columns: repeat(6, minmax(0, 1fr));
		gap: 1px;
		overflow: hidden;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 10px;
		background: var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}

	.llm-timing-strip > div {
		display: flex;
		flex-direction: column;
		gap: 5px;
		padding: 11px 12px;
		background: var(--theme-color-surface, #fff);
	}

	.llm-timing-strip strong {
		font-size: 0.95rem;
		font-variant-numeric: tabular-nums;
		color: var(--theme-color-foreground, #111827);
	}

	.llm-governed-warning {
		margin: 0;
		font-size: 0.8rem;
		color: var(--color-warning, #b45309);
	}

	.llm-today-chip-delta {
		font-family: var(--theme-font-body);
		font-size: 0.8rem;
		font-variant-numeric: tabular-nums;
		color: var(--text-muted, var(--theme-color-foreground-muted, #6B7280));
	}

	.llm-today-chip-delta--good {
		color: color-mix(in srgb, var(--color-success) 72%, var(--text-muted));
	}

	.llm-today-chip-delta--bad {
		color: color-mix(in srgb, var(--color-error) 72%, var(--text-muted));
	}

	.llm-top-models {
		gap: 10px;
	}

	.llm-top-model-list {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.llm-top-model-row {
		display: grid;
		grid-template-columns: minmax(0, 1.8fr) repeat(4, minmax(5.5rem, 0.7fr));
		gap: 12px;
		align-items: center;
		padding: 10px 0;
		border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
	}

	.llm-top-model-row:first-child {
		border-top: none;
	}

	.llm-top-model-name {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
	}

	.llm-top-model-rank {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.35rem;
		height: 1.35rem;
		flex: 0 0 auto;
		border-radius: 999px;
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.04));
		color: var(--theme-color-foreground-muted, #6B7280);
		font-family: var(--theme-font-body);
		font-size: 0.72rem;
		font-weight: 700;
	}

	.llm-top-model-text {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.9rem;
		font-weight: 600;
	}

	.llm-top-model-metric {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: 2px;
		min-width: 0;
	}

	.llm-top-model-value {
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.86rem;
		font-variant-numeric: tabular-nums;
		line-height: 1.2;
		white-space: nowrap;
	}

	.llm-top-model-label {
		color: var(--theme-color-foreground-muted, #6B7280);
		font-family: var(--theme-font-body);
		font-size: 0.7rem;
		line-height: 1.2;
		text-transform: uppercase;
		letter-spacing: 0.06em;
	}

	/* Tabs: the page's top-level grouping (Cost / Usage / Health). Styled as
	 * an underline rail rather than the pill `.llm-seg` used for in-section
	 * toggles, so the page-level switch reads as a different order of control
	 * than the widget-level ones below it. */
	.llm-tabs {
		display: flex;
		flex-wrap: wrap;
		gap: 4px;
		border-bottom: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
	}

	.llm-tab {
		position: relative;
		padding: 10px 18px;
		border: none;
		border-bottom: 2px solid transparent;
		background: transparent;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-family: var(--theme-font-body);
		font-size: 0.92rem;
		font-weight: 650;
		cursor: pointer;
	}

	.llm-tab:hover {
		color: var(--theme-color-foreground, #111827);
	}

	.llm-tab--active {
		border-bottom-color: var(--theme-color-accent, #2563eb);
		color: var(--theme-color-foreground, #111827);
	}

	.llm-tab:focus-visible {
		outline: 2px solid var(--theme-color-accent, #2563eb);
		outline-offset: -2px;
	}

	/* The panel has to re-establish the layout its sections used to get for
	 * free. They were direct children of DashboardChrome's `.chrome-body`,
	 * which is a column flex container with `--theme-block-gap` between
	 * children; wrapping them in a panel made the panel the single flex child
	 * and collapsed every inter-section gap inside it. Same container, same
	 * token, so the rhythm matches the rest of the app and follows the theme.
	 *
	 * Trade-off worth knowing: `.chrome-body > *` also carries the staggered
	 * `block-rise` entrance, so the sections no longer rise in one by one —
	 * the panel rises as a single block instead. That reads better with tabs
	 * than re-staggering on every switch would. */
	.llm-panel {
		display: flex;
		flex-direction: column;
		gap: var(--theme-block-gap, 32px);
	}

	/* No margin-bottom: the panel's flex `gap` already spaces this off the
	 * first section. */
	.llm-panel-bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}

	.llm-panel-hint {
		margin: 0;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-family: var(--theme-font-body);
		font-size: 0.82rem;
	}

	.llm-tab-refresh {
		flex: 0 0 auto;
		padding: 6px 14px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		background-color: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
	}

	.llm-tab-refresh:hover:not(:disabled) {
		border-color: var(--theme-color-accent, #2563eb);
	}

	/* Held through the debounce window: the click registered, the query
	 * hasn't gone out yet. Dimmed rather than hidden so the button doesn't
	 * shift the row. */
	.llm-tab-refresh:disabled {
		opacity: 0.6;
		cursor: progress;
	}

	/* Filter bar: horizontal row of label+input pairs that wraps onto
	 * additional rows when the viewport narrows. Sticks to the top of
	 * the scroll area so it stays visible as the operator scrolls
	 * through the page below. Theme tokens drive every color so the
	 * bar recolors with the active theme. */
	.llm-filter-bar {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: 14px 18px;
		padding: 14px 18px;
		background-color: var(--theme-color-surface-muted, var(--theme-color-surface, #fff));
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 12px;
		position: sticky;
		top: 0;
		z-index: 10;
	}

	.llm-filter-field {
		display: flex;
		flex-direction: column;
		gap: 4px;
		min-width: 140px;
	}

	.llm-filter-field--inline {
		flex-direction: row;
		align-items: center;
		gap: 8px;
		min-width: 0;
	}

	.llm-filter-label {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.7rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: var(--theme-color-foreground-muted, #6B7280);
	}

	.llm-filter-select,
	.llm-filter-input {
		padding: 6px 10px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		background-color: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.875rem;
		line-height: 1.2;
	}

	.llm-filter-input::placeholder {
		color: var(--theme-color-foreground-muted, #9CA3AF);
		opacity: 0.7;
	}

	.llm-filter-select:focus,
	.llm-filter-input:focus {
		outline: 2px solid var(--theme-color-accent, #4ecdc4);
		outline-offset: 1px;
	}

	.llm-filter-clear {
		margin-left: auto;
		align-self: flex-end;
		padding: 6px 12px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		border-radius: 6px;
		background-color: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
	}

	.llm-filter-clear:hover {
		background-color: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.04));
	}

	.llm-vibedev-table {
		display: flex;
		flex-direction: column;
		min-width: 0;
		overflow-x: auto;
	}

	.llm-vibedev-row {
		display: grid;
		grid-template-columns: minmax(11rem, 1.35fr) minmax(5rem, 0.55fr) minmax(4.5rem, 0.45fr) minmax(9rem, 0.8fr) minmax(9rem, 1fr);
		gap: 12px;
		align-items: center;
		padding: 9px 0;
		border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.83rem;
		font-variant-numeric: tabular-nums;
		min-width: 760px;
	}

	.llm-vibedev-row:first-child {
		border-top: none;
	}

	.llm-vibedev-row--head {
		color: var(--theme-color-foreground-muted, #6B7280);
		font-size: 0.68rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}

	.llm-vibedev-row > span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.llm-vibedev-project {
		font-weight: 650;
	}

	.llm-vibedev-row small {
		display: block;
		margin-top: 2px;
		color: var(--theme-color-foreground-muted, #6B7280);
		font-size: 0.72rem;
	}

	@media (max-width: 720px) {
		.llm-governed-grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.llm-timing-strip {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}

		.llm-top-model-list {
			gap: 10px;
		}

		.llm-top-model-row {
			grid-template-columns: 1fr 1fr;
			gap: 10px 14px;
			padding: 12px 0;
		}

		.llm-top-model-name {
			grid-column: 1 / -1;
		}

		.llm-top-model-metric {
			align-items: flex-start;
		}
	}
</style>
