<script lang="ts">
	/**
	 * TASKS — the deck's right rail: what the system is doing, and everything
	 * it has been asked to do.
	 *
	 * Top: the LIVE execution (real progress fraction, step label and ordinal
	 * from `taskStore.executingTask`). Below: a task BROWSER — USER · INTERNAL
	 * tabs with a second row of lanes:
	 * - USER lanes mirror the Tasks page chips exactly (all/inbox/today/
	 *   overdue/running/completed) and reuse its `computeFilteredTasks`
	 *   predicate over the live task-store pool — one lane definition, no
	 *   drift, paged client-side because due-date lanes cannot be evaluated
	 *   server-side;
	 * - INTERNAL is true server pagination on `/v3/tasks/internal` by status.
	 * (`/v3/tasks` itself also supports server paging now — kept for other
	 * consumers and for plain-status listings.)
	 *
	 * Every row is a spotlight toggle for the stage's run graph; the ⌕ on the
	 * row brings that task's graph to the FRONT directly.
	 */
	import { onDestroy, onMount } from 'svelte';
	import { browser } from '$app/environment';

	import { computeFilteredTasks, type Task, type ExecutionState, type TaskFilter } from '$lib/stores/taskStore';
	import { fmtAge } from './deck';

	export let executing: ExecutionState | null = null;
	/** Live task-store pool — drives the LIVE count and the tally. */
	export let tasks: Task[] = [];
	export let completedToday = 0;
	export let nowMs = Date.now();
	/** Row click: toggle this task's spotlight on the stage graph. */
	export let onSelectTask: (taskId: string, meta?: { title?: string; status?: string }) => void =
		() => {};
	/** ⌕ on a row: bring this task's run graph to the FRONT. */
	export let onOpenGraph: (taskId: string, meta?: { title?: string; status?: string }) => void =
		() => {};
	export let selectedTaskId: string | null = null;

	$: active = tasks.filter((t) => t.status === 'running' || t.status === 'paused');
	$: failedRecent = tasks.filter((t) => t.status === 'failed').slice(0, 3);
	$: progressPct = executing ? Math.round(Math.min(1, Math.max(0, executing.progress)) * 100) : 0;

	// ── the browser ──────────────────────────────────────────────────────
	interface BrowseRow {
		id: string;
		title: string;
		status: string;
		agent: string;
		updatedMs: number;
	}

	const PAGE = 9;
	const SOURCES = [
		{ key: 'user', label: 'USER' },
		{ key: 'internal', label: 'INTERNAL' }
	] as const;
	/** USER lanes mirror the Tasks page's FILTER_OPTIONS exactly, and the
	 *  membership logic IS the page's: `computeFilteredTasks` from the task
	 *  store — one predicate, no drift. */
	const USER_LANES = [
		{ key: 'all', label: 'ALL' },
		{ key: 'inbox', label: 'INBOX' },
		{ key: 'today', label: 'TODAY' },
		{ key: 'overdue', label: 'OVERDUE' },
		{ key: 'running', label: 'RUNNING' },
		{ key: 'completed', label: 'COMPLETED' }
	] as const;
	/** INTERNAL segments are storage statuses — inbox/today/overdue are due-
	 *  date lanes and internal runs have none. */
	const INTERNAL_SEGMENTS = [
		{ key: 'all', label: 'ALL' },
		{ key: 'running', label: 'RUNNING' },
		{ key: 'ready', label: 'READY' },
		{ key: 'pending', label: 'PENDING' },
		{ key: 'completed', label: 'COMPLETED' },
		{ key: 'failed', label: 'FAILED' }
	] as const;

	let source: (typeof SOURCES)[number]['key'] = 'user';
	let userLane: (typeof USER_LANES)[number]['key'] = 'all';
	let segment: (typeof INTERNAL_SEGMENTS)[number]['key'] = 'all';
	let offset = 0;
	let rows: BrowseRow[] = [];
	let total = 0;
	let loading = false;
	let loadError: string | null = null;
	let refreshTimer: ReturnType<typeof setInterval> | null = null;

	// Guard object, untracked — a reactive load key read+written in one `$:`
	// block is the self-invalidation loop this deck has been burned by twice.
	const loadGuard = { key: '', generation: 0 };

	function norm(t: Record<string, unknown>): BrowseRow {
		const raw = (t.updated_at ?? t.created_at) as string | number | null | undefined;
		const ms =
			typeof raw === 'number'
				? raw > 1e12
					? raw
					: raw * 1000
				: Date.parse(String(raw ?? '')) || 0;
		return {
			id: String(t.id ?? ''),
			title: String(t.title || t.description || t.id || ''),
			status: String(t.status ?? ''),
			agent: String(t.agent_id ?? ''),
			updatedMs: ms
		};
	}

	function taskToRow(t: Task): BrowseRow {
		return {
			id: t.id,
			title: t.title || t.description || t.id,
			status: t.status,
			agent: t.agentName ?? '',
			updatedMs: Date.parse(String(t.updatedAt ?? t.createdAt ?? '')) || 0
		};
	}

	// USER tab: pure derivation over the live task-store pool — the same pool
	// and the same predicate the Tasks page renders, so lane membership can
	// never disagree between the two surfaces. No fetch of its own.
	$: userPool =
		source === 'user'
			? computeFilteredTasks(tasks, userLane as TaskFilter, '', new Set()).map(taskToRow)
			: [];
	$: if (source === 'user') {
		total = userPool.length;
		rows = userPool.slice(offset, offset + PAGE);
		loadError = null;
	}

	// INTERNAL tab: true server pagination (`/v3/tasks/internal`).
	async function loadInternal(force = false): Promise<void> {
		const key = `internal|${segment}|${offset}`;
		if (!force && key === loadGuard.key) return;
		loadGuard.key = key;
		const generation = ++loadGuard.generation;
		loading = true;
		loadError = null;
		try {
			const params = new URLSearchParams({
				limit: String(PAGE),
				offset: String(offset),
				sort: 'updated_at',
				order: 'desc'
			});
			if (segment !== 'all') params.set('status', segment);
			const res = await fetch(`/api/magician/v3/tasks/internal?${params}`, {
				signal: AbortSignal.timeout(12_000)
			});
			if (!res.ok) throw new Error(String(res.status));
			const body = await res.json();
			if (generation !== loadGuard.generation) return;
			rows = (Array.isArray(body.tasks) ? body.tasks : []).map(norm);
			total = Number(body.pagination?.total ?? rows.length);
		} catch {
			if (generation !== loadGuard.generation) return;
			rows = [];
			total = 0;
			loadError = 'tasks unavailable';
		} finally {
			if (generation === loadGuard.generation) loading = false;
		}
	}

	$: if (browser && source === 'internal' && (segment || offset >= 0)) void loadInternal();

	function setSource(next: (typeof SOURCES)[number]['key']): void {
		if (source === next) return;
		source = next;
		offset = 0;
		loadGuard.key = '';
	}

	function setUserLane(next: (typeof USER_LANES)[number]['key']): void {
		if (userLane === next) return;
		userLane = next;
		offset = 0;
	}

	function setSegment(next: (typeof INTERNAL_SEGMENTS)[number]['key']): void {
		if (segment === next) return;
		segment = next;
		offset = 0;
	}

	$: pageStart = total === 0 ? 0 : offset + 1;
	$: pageEnd = Math.min(offset + PAGE, total);
	$: hasPrev = offset > 0;
	$: hasNext = offset + PAGE < total;

	onMount(() => {
		if (!browser) return;
		// Internal pool moves under the deck; refresh quietly. (The USER tab
		// tracks the task store, which refreshes itself.)
		refreshTimer = setInterval(() => {
			if (source === 'internal') void loadInternal(true);
		}, 30_000);
	});

	onDestroy(() => {
		if (refreshTimer) clearInterval(refreshTimer);
	});

	function rowAge(row: BrowseRow): string {
		return row.updatedMs > 0 ? fmtAge(row.updatedMs, nowMs) : '';
	}
</script>

<section class="rail" aria-label="Tasks">
	<h2 class="rail-title"><span class="tick" aria-hidden="true"></span>TASKS
		<span class="count" data-zero={active.length === 0}>{active.length} LIVE</span>
	</h2>

	{#if executing}
		<div class="exec">
			<div class="exec-head">
				<span class="exec-label">EXECUTING</span>
				<span class="exec-steps">{executing.currentStepIndex + 1}/{executing.totalSteps || '?'}</span>
			</div>
			<p class="exec-step">{executing.currentStep || 'working…'}</p>
			<div class="bar" role="progressbar" aria-valuenow={progressPct} aria-valuemin="0" aria-valuemax="100">
				<i style:width="{progressPct}%"></i>
			</div>
			<span class="exec-pct">{progressPct}%</span>
		</div>
	{/if}

	<!-- Row 1: source TABS. Row 2: the lanes — USER mirrors the Tasks page's
	     chips exactly; INTERNAL shows storage statuses. -->
	<div class="lane-tabs" role="tablist" aria-label="Task source">
		{#each SOURCES as s (s.key)}
			<button class="lane-tab" data-on={source === s.key} role="tab" aria-selected={source === s.key}
				on:click={() => setSource(s.key)}>{s.label}</button>
		{/each}
	</div>
	<div class="segs" role="tablist" aria-label="Task lane">
		{#if source === 'user'}
			{#each USER_LANES as s (s.key)}
				<button class="seg seg--status" data-on={userLane === s.key} role="tab" aria-selected={userLane === s.key}
					on:click={() => setUserLane(s.key)}>{s.label}</button>
			{/each}
		{:else}
			{#each INTERNAL_SEGMENTS as s (s.key)}
				<button class="seg seg--status" data-on={segment === s.key} role="tab" aria-selected={segment === s.key}
					on:click={() => setSegment(s.key)}>{s.label}</button>
			{/each}
		{/if}
	</div>

	{#if loadError}
		<p class="note note--bad">{loadError}</p>
	{:else if rows.length === 0 && !loading}
		<p class="note">no {(source === 'user' ? userLane : segment) === 'all' ? '' : (source === 'user' ? userLane : segment) + ' '}tasks{source === 'internal' ? ' (internal)' : ''}</p>
	{/if}

	<ol class="list" data-loading={loading}>
		{#each rows as row (row.id)}
			<!-- The li stays a plain list item (a11y: a non-interactive element
			     must not carry an interactive role); the row DIV inside is the
			     button. `display: contents` keeps the layout identical. -->
			<li class="row-item">
				<div
					class="row"
					data-status={row.status}
					data-selected={row.id === selectedTaskId}
					role="button"
					tabindex="0"
					title="Spotlight this run on the stage"
					on:click={() => onSelectTask(row.id, { title: row.title, status: row.status })}
					on:keydown={(e) =>
						(e.key === 'Enter' || e.key === ' ') &&
						onSelectTask(row.id, { title: row.title, status: row.status })}
				>
					<i class="dot" aria-hidden="true"></i>
					<span class="title">{row.title}</span>
					<span class="meta">{row.status.slice(0, 4).toUpperCase()}{#if rowAge(row)}·{rowAge(row)}{/if}</span>
					<button
						class="graph-btn"
						title="View run graph"
						aria-label="View run graph for {row.title}"
						on:click|stopPropagation={() => onOpenGraph(row.id, { title: row.title, status: row.status })}
					>⌕</button>
				</div>
			</li>
		{/each}
	</ol>

	<div class="pager">
		<button class="seg" disabled={!hasPrev} on:click={() => (offset = Math.max(0, offset - PAGE))} aria-label="Previous page">‹</button>
		<span class="pager-label">{pageStart}–{pageEnd} of {total}</span>
		<button class="seg" disabled={!hasNext} on:click={() => (offset = offset + PAGE)} aria-label="Next page">›</button>
	</div>

	<footer class="tally">
		<div class="t">
			<span class="t-num">{completedToday}</span>
			<span class="t-label">DONE TODAY</span>
		</div>
		<div class="t" data-bad={failedRecent.length > 0}>
			<span class="t-num">{failedRecent.length}</span>
			<span class="t-label">FAILED</span>
		</div>
	</footer>
</section>

<style>
	.rail {
		grid-area: flight;
		/* Above the deck's scanline film — see `.scan` in +page.svelte. */
		z-index: 1;
		display: flex;
		flex-direction: column;
		border-left: 1px solid var(--deck-line);
		min-height: 0;
		background: linear-gradient(270deg, color-mix(in srgb, var(--deck-glow) 3%, transparent), transparent 40%);
	}
	.rail-title {
		display: flex; align-items: center; gap: 8px;
		margin: 0; padding: 14px 16px 10px;
		font: 600 11px/1 var(--font-display);
		letter-spacing: 0.3em;
		color: var(--deck-dim);
	}
	.tick { width: 14px; height: 2px; background: var(--deck-glow); }
	.count {
		margin-left: auto;
		font: 700 9px/1 var(--font-data);
		letter-spacing: 0.1em;
		color: var(--deck-glow);
		padding: 3px 7px;
		border: 1px solid var(--deck-line);
		white-space: nowrap;
	}
	.count[data-zero='true'] { color: var(--deck-dim); }

	.exec {
		margin: 2px 14px 10px;
		border: 1px solid var(--deck-line);
		border-left: 3px solid var(--deck-glow);
		padding: 10px 12px;
		background: color-mix(in srgb, var(--deck-glow) 5%, transparent);
	}
	.exec-head { display: flex; justify-content: space-between; margin-bottom: 4px; }
	.exec-label { font: 700 9px/1 var(--font-data); letter-spacing: 0.22em; color: var(--deck-glow); }
	.exec-steps { font: 500 10px/1 var(--font-data); color: var(--deck-dim); font-variant-numeric: tabular-nums; }
	.exec-step {
		margin: 0 0 8px;
		font: 400 12px/1.4 var(--font-body, inherit);
		color: var(--deck-text);
		white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
	}
	.bar { height: 4px; background: color-mix(in srgb, var(--deck-glow) 14%, transparent); overflow: hidden; }
	.bar i {
		display: block; height: 100%;
		background: var(--deck-glow);
		box-shadow: 0 0 8px var(--deck-glow);
		transition: width 600ms cubic-bezier(0.25, 1, 0.4, 1);
	}
	.exec-pct { display: block; margin-top: 4px; text-align: right; font: 600 10px/1 var(--font-data); color: var(--deck-glow); font-variant-numeric: tabular-nums; }

	/* ── source tabs ────────────────────────────────────────────────────────
	   NOT `.tabs`/`.tab`: daisyUI ships components under those names (fixed
	   ~2.5rem height, display:flex) and they inflated these to 43px — the
	   `.stat` collision all over again. Deck-local names are immune. */
	.lane-tabs {
		display: flex;
		margin: 0 14px 6px;
		border: 1px solid var(--deck-line);
	}
	.lane-tab {
		flex: 1;
		background: none;
		border: none;
		border-bottom: 2px solid transparent;
		padding: 4px 0 3px;
		cursor: pointer;
		font: 700 8.5px/1 var(--font-display);
		letter-spacing: 0.22em;
		color: var(--deck-dim);
	}
	.lane-tab + .lane-tab { border-left: 1px solid var(--deck-line); }
	.lane-tab:hover { color: var(--deck-glow); }
	.lane-tab[data-on='true'] {
		color: var(--deck-glow);
		border-bottom-color: var(--deck-glow);
		background: color-mix(in srgb, var(--deck-glow) 6%, transparent);
	}

	/* ── segments: text, not chrome ─────────────────────────────────────── */
	.segs {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 4px 9px;
		padding: 0 16px 8px;
	}
	.seg {
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		font: 600 9px/1.4 var(--font-data);
		letter-spacing: 0.14em;
		color: var(--deck-dim);
	}
	.seg:hover:not(:disabled) { color: var(--deck-glow); }
	.seg[data-on='true'] { color: var(--deck-glow); }
	.seg:disabled { opacity: 0.3; cursor: default; }
	.seg--status { letter-spacing: 0.1em; }

	.note {
		margin: 6px 16px;
		font: 400 10px/1.4 var(--font-data);
		letter-spacing: 0.08em;
		color: var(--deck-dim);
	}
	.note--bad { color: var(--sev-err); }

	.list {
		list-style: none;
		margin: 0; padding: 0 14px;
		overflow-y: auto;
		display: flex; flex-direction: column; gap: 5px;
		min-height: 0;
		flex: 1;
		scrollbar-width: thin;
		transition: opacity 150ms ease;
	}
	.list[data-loading='true'] { opacity: 0.55; }
	.row-item { display: contents; }
	.row {
		display: flex; align-items: center; gap: 8px;
		padding: 7px 8px 7px 10px;
		border: 1px solid color-mix(in srgb, var(--deck-glow) 12%, transparent);
		cursor: pointer;
	}
	.row:hover { border-color: color-mix(in srgb, var(--deck-glow) 40%, transparent); }
	.row[data-selected='true'] {
		border-color: var(--deck-glow);
		background: color-mix(in srgb, var(--deck-glow) 7%, transparent);
	}
	.dot { width: 6px; height: 6px; border-radius: 50%; background: var(--deck-dim); flex: none; }
	.row[data-status='running'] .dot { background: var(--deck-glow); animation: dot-live 1.2s ease-in-out infinite; }
	.row[data-status='paused'] .dot { background: var(--sev-warn); }
	.row[data-status='completed'] .dot { background: var(--sev-ok); }
	.row[data-status='failed'] .dot { background: var(--sev-err); }
	@keyframes dot-live { 50% { box-shadow: 0 0 8px var(--deck-glow); } }
	.title {
		flex: 1; min-width: 0;
		font: 400 11.5px/1.3 var(--font-body, inherit);
		color: var(--deck-text);
		white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
	}
	.meta { font: 500 8.5px/1 var(--font-data); letter-spacing: 0.08em; color: var(--deck-dim); white-space: nowrap; }

	/* The per-row door to the run graph. Text-sized, glows on hover. */
	.graph-btn {
		background: none;
		border: none;
		padding: 0 2px;
		cursor: pointer;
		font: 600 13px/1 var(--font-data);
		color: var(--deck-dim);
		flex: none;
	}
	.graph-btn:hover { color: var(--deck-glow); }

	.pager {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 14px;
		padding: 8px 16px;
	}
	.pager .seg { font-size: 12px; }
	.pager-label {
		font: 500 9px/1 var(--font-data);
		letter-spacing: 0.1em;
		color: var(--deck-dim);
		font-variant-numeric: tabular-nums;
	}

	.tally {
		display: flex; gap: 1px;
		margin-top: auto;
		border-top: 1px solid var(--deck-line);
	}
	.t {
		flex: 1;
		padding: 12px 0 14px;
		text-align: center;
	}
	.t + .t { border-left: 1px solid var(--deck-line); }
	.t-num {
		display: block;
		font: 700 22px/1 var(--font-data);
		font-variant-numeric: tabular-nums;
		color: var(--deck-text);
	}
	.t[data-bad='true'] .t-num { color: var(--sev-err); }
	.t-label { font: 600 8px/1 var(--font-data); letter-spacing: 0.24em; color: var(--deck-dim); }
</style>
