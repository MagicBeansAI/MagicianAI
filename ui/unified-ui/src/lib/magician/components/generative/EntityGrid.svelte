<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';
	import Table from './Table.svelte';

	interface EntityGridColumn {
		key: string;
		label: string;
		sortable?: boolean;
		width?: string;
	}

	export let columns: EntityGridColumn[] = [];
	export let rows: Array<Record<string, unknown>> = [];
	export let pageSize: number = 25;
	export let paginationMode: 'client' | 'server' = 'client';
	/** One-based canonical page metadata supplied with a server page. */
	export let currentPage: number = 1;
	export let pageCount: number = 1;
	export let totalItems: number = 0;
	export let pageCountExact: boolean = true;
	export let totalItemsExact: boolean = true;
	export let startItem: number = 0;
	export let endItem: number = 0;
	export let sortKey: string = '';
	export let sortDir: 'asc' | 'desc' = 'asc';
	/** When true, rows are expandable on click and an actions column is appended. */
	export let expandable: boolean = false;
	/** Row key used to extract the row identifier for expand/action callbacks. */
	export let rowIdKey: string = 'id';
	/** Action buttons to show in the actions column when expandable is true. */
	export let actions: Array<{ id: string; label: string; variant?: string }> = [];
	/** Optional row fields to search before sorting/pagination. */
	export let filterKeys: string[] = [];
	export let filterPlaceholder: string = 'Filter rows';
	export let filterLabel: string = 'Filter rows';
	export let stackedFields: Array<{ key: string; label?: string }> = [];
	export let stackedValueMaxLines: number = 0;
	export let stackedRowsExpandable: boolean = false;
	export let wrapTable: boolean = false;

	const dispatch = createEventDispatcher();

	let expandedRowId: string | null = null;
	let filterText: string = '';

	function toggleRow(rowId: string) {
		expandedRowId = expandedRowId === rowId ? null : rowId;
		dispatch('expand', { rowId: expandedRowId });
	}

	function handleAction(rowId: string, actionId: string) {
		dispatch('action', { rowId, actionId });
	}

	let clientPage: number = 0;

	function normalizeSortDir(value: unknown): 'asc' | 'desc' {
		return value === 'desc' ? 'desc' : 'asc';
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
		return '';
	}

	function searchableCellText(value: unknown): string {
		const MAX_SEARCH_NODES = 512;
		const MAX_SEARCH_CHARS = 4_096;
		const stack: unknown[] = [value];
		const seen = new WeakSet<object>();
		const parts: string[] = [];
		let inspected = 0;
		let characters = 0;

		while (stack.length > 0 && inspected < MAX_SEARCH_NODES && characters < MAX_SEARCH_CHARS) {
			const current = stack.pop();
			inspected += 1;
			if (current == null) continue;
			if (
				typeof current === 'string' ||
				typeof current === 'number' ||
				typeof current === 'boolean' ||
				typeof current === 'bigint'
			) {
				const text = String(current).slice(0, MAX_SEARCH_CHARS - characters);
				if (text) {
					parts.push(text);
					characters += text.length + 1;
				}
				continue;
			}
			if (typeof current !== 'object' || seen.has(current)) continue;
			seen.add(current);
			if (Array.isArray(current)) {
				for (let index = current.length - 1; index >= 0 && stack.length < MAX_SEARCH_NODES; index -= 1) {
					stack.push(current[index]);
				}
				continue;
			}

			const record = current as Record<string, unknown>;
			const kind = asString(record.kind).trim();
			if (kind === 'badge') {
				stack.push(record.text);
			} else if (kind === 'link') {
				stack.push(record.label, record.href, record.url);
			} else if (kind === 'summary') {
				stack.push(record.title, record.meta, record.links);
			} else if (kind === 'actions') {
				stack.push(record.buttons);
			} else {
				const values = Object.values(record);
				for (let index = values.length - 1; index >= 0 && stack.length < MAX_SEARCH_NODES; index -= 1) {
					stack.push(values[index]);
				}
			}
		}

		return parts.join(' ').slice(0, MAX_SEARCH_CHARS);
	}

	$: inferredColumns = (() => {
		if (columns.length > 0) return columns;
		const inferred = new Set<string>();
		for (const row of rows) {
			for (const key of Object.keys(row)) {
				const trimmed = key.trim();
				if (trimmed) inferred.add(trimmed);
			}
		}
		return Array.from(inferred).map((key) => ({ key, label: key, sortable: true }));
	})();

	$: effectiveColumns = (() => {
		let cols = columns.length > 0 ? columns : inferredColumns;
		if (expandable && actions.length > 0) {
			cols = [...cols, { key: '_actions', label: 'Actions', sortable: false }];
		}
		return cols;
	})();

	$: safeSortDir = normalizeSortDir(sortDir);
	$: serverControlled = paginationMode === 'server';
	$: normalizedFilterKeys = filterKeys.map((key) => asString(key).trim()).filter((key) => key.length > 0);
	$: normalizedFilterText = filterText.trim().toLowerCase();
	$: filterEnabled = normalizedFilterKeys.length > 0;
	$: filteredRows = (() => {
		if (serverControlled) return rows;
		if (!filterEnabled || normalizedFilterText.length === 0) return rows;
		return rows.filter((row) =>
			normalizedFilterKeys.some((key) => searchableCellText(row[key]).toLowerCase().includes(normalizedFilterText))
		);
	})();

	// Sort globally BEFORE pagination so sort applies across all pages (R48)
	$: globalSorted = (() => {
		if (serverControlled) return rows;
		const canSort = !!sortKey && effectiveColumns.some((col) => col.key === sortKey && !!col.sortable);
		if (!canSort) return filteredRows;
		// Keep equal-value rows stable across re-renders.
		const tagged = filteredRows.map((row, i) => ({ row, idx: i }));
		tagged.sort((a, b) => {
			const aVal = a.row[sortKey];
			const bVal = b.row[sortKey];
			if (aVal == null && bVal == null) return a.idx - b.idx;
			if (aVal == null) return 1;
			if (bVal == null) return -1;
			if (aVal < bVal) return safeSortDir === 'asc' ? -1 : 1;
			if (aVal > bVal) return safeSortDir === 'asc' ? 1 : -1;
			return a.idx - b.idx;
		});
		return tagged.map((t) => t.row);
	})();
	$: safePageSize = Math.max(1, Number.isFinite(+pageSize) ? Math.floor(+pageSize) : 25);
	$: clientTotalPages = Math.max(1, Math.ceil(globalSorted.length / safePageSize));
	// R325: Reset currentPage when totalPages shrinks below it to avoid stale empty view
	$: if (!serverControlled && clientPage >= clientTotalPages) clientPage = Math.max(0, clientTotalPages - 1);
	$: safeServerPageCount = Math.max(1, Number.isFinite(+pageCount) ? Math.floor(+pageCount) : 1);
	$: safeServerCurrentPage = Math.max(1, Math.min(safeServerPageCount, Number.isFinite(+currentPage) ? Math.floor(+currentPage) : 1));
	$: effectivePageCount = serverControlled ? safeServerPageCount : clientTotalPages;
	$: effectiveCurrentPage = serverControlled
		? safeServerCurrentPage
		: Math.max(1, Math.min(clientTotalPages, clientPage + 1));
	$: effectiveTotalItems = serverControlled
		? Math.max(0, Number.isFinite(+totalItems) ? Math.floor(+totalItems) : 0)
		: globalSorted.length;
	$: effectiveStartItem = serverControlled
		? Math.max(0, Math.min(effectiveTotalItems, Number.isFinite(+startItem) ? Math.floor(+startItem) : 0))
		: globalSorted.length === 0 ? 0 : (effectiveCurrentPage - 1) * safePageSize + 1;
	$: effectiveEndItem = serverControlled
		? Math.max(effectiveStartItem, Math.min(effectiveTotalItems, Number.isFinite(+endItem) ? Math.floor(+endItem) : 0))
		: Math.min(globalSorted.length, (effectiveCurrentPage - 1) * safePageSize + safePageSize);
	$: pagedRows = serverControlled
		? globalSorted
		: globalSorted.slice((effectiveCurrentPage - 1) * safePageSize, effectiveCurrentPage * safePageSize);

	$: effectiveRows = (() => {
		if (!expandable || actions.length === 0) return pagedRows;
		return pagedRows.map(row => ({
			...row,
			_actions: { kind: 'actions', buttons: actions },
		}));
	})();

	// Reset local filter-derived state only when the input actually changes.
	let prevFilterText: string = filterText;
	$: if (filterText !== prevFilterText) {
		prevFilterText = filterText;
		clientPage = 0;
		expandedRowId = null;
	}

	function goToPage(pageNumber: number) {
		const safePage = Math.min(effectivePageCount, Math.max(1, Math.floor(pageNumber)));
		if (serverControlled) {
			dispatch('pagechange', { page: safePage });
			return;
		}
		clientPage = safePage - 1;
	}

	function handleSort(nextSortKey: string, nextSortDir: 'asc' | 'desc') {
		if (serverControlled) {
			dispatch('sortchange', { sortKey: nextSortKey, sortDir: nextSortDir });
			return;
		}
		clientPage = 0;
	}

	function handleFilterInput(event: Event) {
		filterText = (event.currentTarget as HTMLInputElement).value;
		if (serverControlled) dispatch('filterchange', { filterText });
	}

	function handleRowClick(event: MouseEvent) {
		if (!expandable) return;
		// Walk up from target to find <tr>, then extract the row id
		const target = event.target as HTMLElement;
		// Don't toggle when clicking action buttons
		if (target.closest('.muij-table-action-btn')) return;
		const tr = target.closest('tr');
		if (!tr) return;
		const tbody = tr.closest('tbody');
		if (!tbody) return;
		const rowIndex = Array.from(tbody.querySelectorAll('tr')).indexOf(tr);
		if (rowIndex < 0 || rowIndex >= effectiveRows.length) return;
		const row = effectiveRows[rowIndex];
		const rowId = asString((row as Record<string, unknown>)[rowIdKey]);
		if (rowId) toggleRow(rowId);
	}
</script>

{#if rows.length === 0 && effectiveTotalItems === 0}
	<div class="muij-entitygrid-empty">
		<span class="muij-entitygrid-empty-text">No entities to display</span>
	</div>
{:else if effectiveColumns.length === 0}
	<div class="muij-entitygrid-empty">
		<span class="muij-entitygrid-empty-text">No columns to display</span>
	</div>
{:else}
	<!-- svelte-ignore a11y-click-events-have-key-events -->
	<!-- svelte-ignore a11y-no-static-element-interactions -->
	<div class="muij-entitygrid" class:muij-entitygrid-expandable={expandable} on:click={handleRowClick}>
		{#if filterEnabled}
			<div class="muij-entitygrid-toolbar">
				<input
					class="muij-entitygrid-filter"
					type="search"
					value={filterText}
					on:input={handleFilterInput}
					placeholder={filterPlaceholder}
					aria-label={filterLabel}
				/>
			</div>
		{/if}
		{#if effectivePageCount > 1}
			<ServerPager
				currentPage={effectiveCurrentPage}
				pageCount={effectivePageCount}
				startItem={effectiveStartItem}
				endItem={effectiveEndItem}
				totalItems={effectiveTotalItems}
				{pageCountExact}
				{totalItemsExact}
				ariaLabel="Entity grid pagination"
				on:pagechange={(event) => goToPage(event.detail.page)}
			/>
		{/if}
		<Table
			columns={effectiveColumns}
			rows={effectiveRows}
			{stackedFields}
			{stackedValueMaxLines}
			{stackedRowsExpandable}
			wrapMode={wrapTable}
			bind:sortKey
			bind:sortDir
			presorted={true}
			onAction={handleAction}
			onSort={handleSort}
			rowIdKey={rowIdKey}
		/>
		{#if expandable && expandedRowId}
			<div class="muij-entity-grid-detail" role="region" aria-label="Row detail">
				<slot name="detail" rowId={expandedRowId} />
			</div>
		{/if}
		{#if effectivePageCount > 1}
			<ServerPager
				currentPage={effectiveCurrentPage}
				pageCount={effectivePageCount}
				startItem={effectiveStartItem}
				endItem={effectiveEndItem}
				totalItems={effectiveTotalItems}
				{pageCountExact}
				{totalItemsExact}
				ariaLabel="Entity grid pagination"
				on:pagechange={(event) => goToPage(event.detail.page)}
			/>
		{/if}
	</div>
{/if}

<style>
	.muij-entitygrid {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.muij-entitygrid-expandable :global(tbody tr) {
		cursor: pointer;
	}

	.muij-entitygrid-expandable :global(tbody tr:hover) {
		background: var(--bg-hover, #f9fafb);
	}

	.muij-entitygrid-toolbar {
		display: flex;
		justify-content: flex-end;
	}

	.muij-entitygrid-filter {
		width: min(100%, 280px);
		padding: 6px 10px;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		line-height: 1.4;
	}

	.muij-entitygrid-filter::placeholder {
		color: var(--text-muted);
	}

	.muij-entitygrid-filter:focus {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 35%, transparent);
		outline-offset: 1px;
		border-color: var(--accent-primary);
	}

	.muij-entitygrid-empty {
		padding: 24px;
		text-align: center;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
	}

	.muij-entitygrid-empty-text {
		font-family: var(--font-primary);
		font-size: 0.875rem;
		color: var(--text-muted);
	}

	.muij-entity-grid-detail {
		padding: 12px 16px;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-body);
	}
</style>
