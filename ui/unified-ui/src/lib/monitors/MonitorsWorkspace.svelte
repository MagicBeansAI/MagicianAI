<!--
  /tasks?type=monitors — the canonical monitor management surface
  (Phase 4, plan §5.3 + §9.1).

  Compact row list (state + health badges, cadence summary, last scan)
  backed by the CURSOR-paginated GET /monitors under the SHARED `ServerPager`
  — the same control the two other tabs on this route use. Movement is still
  by cursor; the envelope's `total` supplies only the page count and
  skip-to-last, and when the server sends none the surface falls back to the
  cursor-only "Load more" (see `pagination.ts`). Plus a state filter, a
  fixed-size create/edit composer with review-before-activate
  (`MonitorComposer`), and the four-tab detail (`MonitorDetailPanel`).

  URL contract (deep-linkable, matches the tasks-page idiom):
    ?type=monitors[&state=active|paused][&selected=task_…[&update=mu_…]][&compose=1]
-->
<script lang="ts">
	import { browser } from '$app/environment';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import Badge from '$lib/magician/components/native/Badge.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';
	import Card from '$lib/magician/components/native/Card.svelte';
	import EmptyState from '$lib/magician/components/native/EmptyState.svelte';
	import Select from '$lib/magician/components/native/Select.svelte';
	import Skeleton from '$lib/magician/components/native/Skeleton.svelte';
	import Spinner from '$lib/magician/components/native/Spinner.svelte';
	import { TASKS_ROUTE } from '$lib/magician/tasks/taskRoutes';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import type { MonitorDetailV1, MonitorListItemV1 } from '$lib/types/monitor';
	import { listMonitors, type MonitorStateFilter } from './api';
	import MonitorComposer from './MonitorComposer.svelte';
	import MonitorDetailPanel from './MonitorDetailPanel.svelte';
	import {
		createMonitorPager,
		MONITORS_PAGE_SIZES,
		monitorPagerView,
		type MonitorPagerSnapshot
	} from './pagination';
	import {
		emptyMonitorForm,
		formFromSpec,
		healthBadge,
		runStatusBadge,
		type MonitorFormValue
	} from './specForm';

	const FILTERS: Array<{ id: MonitorStateFilter | 'all'; label: string }> = [
		{ id: 'all', label: 'All' },
		{ id: 'active', label: 'Active' },
		{ id: 'paused', label: 'Paused' }
	];

	const pager = createMonitorPager(listMonitors, (next) => (list = next));
	let list: MonitorPagerSnapshot = pager.snapshot();
	let loadedFilter: MonitorStateFilter | 'all' | null = null;

	/**
	 * The five numbers both pagers render, or `null` when the server sent no
	 * `total` — then there is no honest page count and the surface stays on the
	 * cursor-only "Load more". `null` is the ONLY thing that switches the
	 * control; an absent total never renders as a zero-page pager.
	 *
	 * Derived once and spread into both copies (above and below the list) for
	 * the same reason the tasks tab does it: two hand-written prop lists would
	 * agree today and drift the first time one is edited, and the failure would
	 * be one control asserting a page the other denies.
	 */
	$: pagerView = monitorPagerView(list);
	/**
	 * The size selector appears from the SMALLEST page size, not the current
	 * one: with 30 monitors at a page size of 50 there is nothing to page, but
	 * dropping to 25 creates two pages — a selector gated on the pager's own
	 * condition would hide the one control that could produce it.
	 */
	$: showPageSize = pagerView !== null && (list.total ?? 0) > MONITORS_PAGE_SIZES[0];
	$: showPager = pagerView !== null && pagerView.pageCount > 1;

	type ComposerState =
		| { open: false }
		| { open: true; mode: 'create'; form: MonitorFormValue }
		| { open: true; mode: 'edit'; form: MonitorFormValue; taskId: string };
	let composer: ComposerState = { open: false };

	// ── URL state ───────────────────────────────────────────────────────────
	$: params = $page.url.searchParams;
	$: stateFilter = ((): MonitorStateFilter | 'all' => {
		const raw = params.get('state');
		return raw === 'active' || raw === 'paused' ? raw : 'all';
	})();
	$: selectedTaskId = (params.get('selected') ?? '').trim() || null;
	$: selectedUpdateId = (params.get('update') ?? '').trim() || null;

	// First load + filter changes (client-only; static SPA build).
	$: if (browser && stateFilter !== loadedFilter) {
		loadedFilter = stateFilter;
		void pager.reset(stateFilter === 'all' ? null : stateFilter);
	}

	// Palette / deep-link compose entry: open the composer once, then strip
	// the marker param so refresh/back don't re-open it.
	$: if (browser && params.has('compose') && !composer.open) {
		composer = { open: true, mode: 'create', form: emptyMonitorForm() };
		void navigate({ compose: null }, true);
	}

	async function navigate(
		changes: Record<string, string | null>,
		replaceState = false
	): Promise<void> {
		const next = new URLSearchParams($page.url.searchParams);
		next.set('type', 'monitors');
		for (const [key, value] of Object.entries(changes)) {
			if (value === null) next.delete(key);
			else next.set(key, value);
		}
		await goto(`${TASKS_ROUTE}?${next.toString()}`, { replaceState, noScroll: true });
	}

	function openMonitor(taskId: string): void {
		void navigate({ selected: taskId, update: null });
	}

	function backToList(): void {
		void navigate({ selected: null, update: null });
	}

	function setFilter(filter: MonitorStateFilter | 'all'): void {
		void navigate({ state: filter === 'all' ? null : filter }, true);
	}

	function openCreateComposer(): void {
		composer = { open: true, mode: 'create', form: emptyMonitorForm() };
	}

	function openEditComposer(detail: MonitorDetailV1): void {
		composer = {
			open: true,
			mode: 'edit',
			form: formFromSpec(detail.title, detail.spec, detail.schedule ?? null),
			taskId: detail.task_id
		};
	}

	function closeComposer(): void {
		composer = { open: false };
	}

	function handleSaved(event: CustomEvent<{ taskId: string; monitorRevision: number }>): void {
		const savedTaskId = event.detail.taskId;
		composer = { open: false };
		reloadInPlace();
		// Land on the saved monitor (create AND edit) — the detail reloads
		// itself when `selected` (or its revision-bearing data) changes.
		void navigate({ selected: savedTaskId, update: null });
	}

	/**
	 * A monitor was deleted from its detail panel. The list reconciles through
	 * the shared removal policy rather than starting over: dropping the row and
	 * its share of the total, re-reading the page the reader is on, and stepping
	 * back only if that page came back empty.
	 */
	function handleDeleted(event: CustomEvent<{ taskId: string }>): void {
		void pager.removeRow(event.detail.taskId);
		backToList();
	}

	/**
	 * A monitor CHANGED under the reader — paused, resumed, saved. Re-read the
	 * page they are on and leave them on it. `refresh` was doing this job, and
	 * it lands on page one: pausing a monitor from page three handed page one
	 * back, which is a mutation moving the reader for no reason of its own.
	 */
	function reloadInPlace(): void {
		void pager.reloadCurrentPage();
	}

	/** Start over from page one — the explicit Refresh, and the error Retry. */
	function refresh(): void {
		void pager.reset(stateFilter === 'all' ? null : stateFilter);
	}

	function gotoMonitorPage(pageNumber: number): void {
		void pager.goToPage(pageNumber);
	}

	function setMonitorsPageSize(size: number): void {
		void pager.setPageSize(size);
	}

	function lastScanLabel(row: MonitorListItemV1): string {
		if (row.last_run_status === 'never_ran') return 'Never ran';
		if (!row.last_run_at) return runStatusBadge(row.last_run_status).text;
		const parsed = Date.parse(row.last_run_at);
		const when = Number.isFinite(parsed) ? new Date(parsed).toLocaleString() : row.last_run_at;
		return `${runStatusBadge(row.last_run_status).text} · ${when}`;
	}
</script>

<div class="mon-shell">
	{#if selectedTaskId}
		{#if composer.open && composer.mode === 'edit'}
			<MonitorComposer
				mode="edit"
				taskId={composer.taskId}
				form={composer.form}
				on:close={closeComposer}
				on:saved={handleSaved}
			/>
		{:else}
			<MonitorDetailPanel
				taskId={selectedTaskId}
				initialUpdateId={selectedUpdateId}
				on:back={backToList}
				on:deleted={handleDeleted}
				on:changed={reloadInPlace}
				on:edit={(event) => openEditComposer(event.detail.detail)}
			/>
		{/if}
	{:else}
		<header class="mon-header">
			<div class="mon-heading">
				<h1>Monitors</h1>
				<p>
					Recurring watches over pages, sites, and searches — quiet unless something
					material changes.
				</p>
			</div>
			<div class="mon-header__actions">
				<Button variant="secondary" size="sm" label="Refresh" on:click={refresh} />
				<Button variant="primary" size="sm" label="New monitor" on:click={openCreateComposer} />
			</div>
		</header>

		<nav class="mon-filters" aria-label="Monitor state filter">
			{#each FILTERS as filter (filter.id)}
				<button
					type="button"
					class="mon-filter"
					class:mon-filter--active={stateFilter === filter.id}
					aria-pressed={stateFilter === filter.id}
					on:click={() => setFilter(filter.id)}
				>
					{filter.label}
				</button>
			{/each}
		</nav>

		{#if composer.open && composer.mode === 'create'}
			<MonitorComposer
				mode="create"
				form={composer.form}
				on:close={closeComposer}
				on:saved={handleSaved}
			/>
		{/if}

		{#if list.error}
			<div class="mon-error" role="alert">
				<span>Couldn't load monitors: {list.error}</span>
				<Button variant="outline" size="sm" label="Retry" on:click={refresh} />
			</div>
		{/if}

		{#if list.initialLoading}
			<!-- Fixed-size skeleton rows: no empty-state flash before first data. -->
			<div class="mon-list" aria-hidden="true">
				{#each Array.from({ length: 4 }, (_, i) => i) as i (i)}
					<Skeleton variant="rect" height="4.4rem" />
				{/each}
			</div>
		{:else if list.items.length === 0 && !list.error}
			<EmptyState
				icon="◉"
				title={stateFilter === 'all' ? 'No monitors yet' : `No ${stateFilter} monitors`}
				description={stateFilter === 'all'
					? 'Create one to watch a page, a site, or a search on a schedule.'
					: 'Switch the filter to see the rest.'}
				actionLabel={stateFilter === 'all' ? 'New monitor' : ''}
				on:action={openCreateComposer}
			/>
		{:else if list.items.length > 0}
			<!--
				**A pager above the list as well as below it**, which is what the two
				other tabs on this route already have. On a full page the bottom one
				sits under 25 rows, so paging from the top means scrolling to a control
				you cannot see to get back where you already were. Both come from
				`pagerView`, so they cannot disagree.
			-->
			{#if showPageSize || showPager}
				<div class="mon-pager mon-pager--top">
					{#if showPageSize}
						<div class="mon-pager__size">
							<Select
								label="Page size"
								value={String(list.pageSize)}
								options={MONITORS_PAGE_SIZES.map((size) => ({
									value: String(size),
									label: String(size)
								}))}
								interactive={true}
								on:change={(event) => setMonitorsPageSize(parseInt(event.detail.value, 10))}
							/>
						</div>
					{/if}
					{#if showPager && pagerView}
						<ServerPager
							{...pagerView}
							ariaLabel="Monitor pages, above the list"
							loading={list.loading}
							on:pagechange={(event) => gotoMonitorPage(event.detail.page)}
						/>
					{/if}
				</div>
			{/if}

			<ol class="mon-list">
				{#each list.items as row (row.task_id)}
					<li>
						<Card interactive elevation={1} on:click={() => openMonitor(row.task_id)}>
							<div class="mon-row">
								<div class="mon-row__main">
									<span class="mon-row__title">{row.title || 'Untitled monitor'}</span>
									<span class="mon-row__objective">{row.objective}</span>
								</div>
								<div class="mon-row__meta">
									<Badge
										text={row.state === 'paused' ? 'Paused' : 'Active'}
										color={row.state === 'paused' ? 'warning' : 'success'}
									/>
									<Badge text={healthBadge(row.health).text} color={healthBadge(row.health).color} />
									<span class="mon-row__cadence">{row.cadence_summary}</span>
									<span class="mon-row__last-scan">{lastScanLabel(row)}</span>
								</div>
							</div>
						</Card>
					</li>
				{/each}
			</ol>

			{#if showPager && pagerView}
				<div class="mon-pager mon-pager--bottom">
					<ServerPager
						{...pagerView}
						ariaLabel="Monitor pages, below the list"
						loading={list.loading}
						on:pagechange={(event) => gotoMonitorPage(event.detail.page)}
					/>
				</div>
			{:else if pagerView === null && list.hasMore}
				<!--
					No server `total`: there is no page count to show, so the surface
					keeps cursor-only paging. Absence means the server never applied the
					counting envelope — it is NOT "zero pages", which is why this branch
					is reached on `pagerView === null` and never on a pageCount of 0.
				-->
				<div class="mon-load-more">
					{#if list.loading}
						<Spinner size="sm" label="Loading more monitors…" centered />
					{:else}
						<Button
							variant="outline"
							size="sm"
							label="Load more"
							on:click={() => void pager.loadNext()}
						/>
					{/if}
				</div>
			{/if}
		{/if}
	{/if}
</div>

<style>
	.mon-shell {
		display: flex;
		flex-direction: column;
		gap: 0.9rem;
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1.1rem 1.45rem 2rem;
		box-sizing: border-box;
		min-height: 0;
		overflow-y: auto;
	}

	.mon-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		flex-wrap: wrap;
	}

	.mon-heading h1 {
		margin: 0;
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.4rem;
		font-weight: 700;
		letter-spacing: -0.01em;
		color: var(--text-primary);
	}

	.mon-heading p {
		margin: 0.3rem 0 0;
		font-size: 0.85rem;
		line-height: 1.5;
		color: var(--text-secondary);
	}

	.mon-header__actions {
		display: flex;
		gap: 0.5rem;
		flex-shrink: 0;
	}

	.mon-filters {
		display: flex;
		gap: 0.35rem;
	}

	.mon-filter {
		display: inline-flex;
		align-items: center;
		min-height: 1.9rem;
		padding: 0 0.7rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--bg-card) 88%, transparent);
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.8rem;
		font-weight: 600;
		cursor: pointer;
	}

	.mon-filter:hover,
	.mon-filter:focus-visible {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 42%, var(--border-soft));
		color: var(--text-primary);
	}

	.mon-filter:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.mon-filter--active {
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 48%, var(--border-soft));
		/* Token-derived fallback (was a hardcoded rgba that ignored dark themes). */
		background: var(
			--accent-primary-soft,
			color-mix(in srgb, var(--accent-primary, currentColor) 12%, transparent)
		);
		color: var(--text-primary);
	}

	.mon-error {
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

	.mon-list {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.mon-row {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.8rem;
		min-width: 0;
	}

	.mon-row__main {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		min-width: 0;
	}

	.mon-row__title {
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.95rem;
		font-weight: 650;
		color: var(--text-primary);
		overflow-wrap: anywhere;
	}

	.mon-row__objective {
		font-size: 0.78rem;
		color: var(--text-secondary);
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		overflow-wrap: anywhere;
	}

	.mon-row__meta {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		flex-wrap: wrap;
		justify-content: flex-end;
		flex-shrink: 0;
		max-width: 45%;
	}

	.mon-row__cadence,
	.mon-row__last-scan {
		font-size: 0.74rem;
		color: var(--text-muted);
		white-space: nowrap;
	}

	/* Layout only — the control itself is `ServerPager`, shared with both task
	   tabs on this route so all three render one pager in one place:
	   right-aligned, page size beside it on the same row. */
	.mon-pager {
		display: flex;
		align-items: center;
		justify-content: flex-end;
	}

	/* Asymmetric on purpose, matching the task tabs: the top one separates the
	   toolbar from the list, the bottom one closes the list and needs no gap
	   under it. */
	.mon-pager--top {
		gap: 12px;
		padding: 4px 0 8px;
	}

	.mon-pager--bottom {
		padding: 10px 0 4px;
	}

	/* `native/Select.svelte`, not a bare `<select>` — the same swap both task
	   tabs made. The component carries the per-theme treatments a hand-rolled
	   copy cannot (`retro-16bit` squares its corners where a literal
	   `border-radius` cannot), which is why the three dropdowns could never have
	   been converged by matching numbers.

	   `:global` because the component owns those classes. Row rather than the
	   component's default column, so the two-word label sits beside the control
	   instead of doubling the row's height above it. */
	.mon-pager__size :global(.native-select) {
		flex-direction: row;
		align-items: center;
		gap: 8px;
	}

	.mon-pager__size :global(.native-select__label) {
		white-space: nowrap;
	}

	.mon-load-more {
		display: flex;
		justify-content: center;
		padding: 0.4rem 0 0.2rem;
	}

	@media (max-width: 720px) {
		.mon-row {
			flex-direction: column;
		}

		.mon-row__meta {
			justify-content: flex-start;
			max-width: 100%;
		}
	}
</style>
