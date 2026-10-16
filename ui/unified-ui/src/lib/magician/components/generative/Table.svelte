<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import {
		createLiveRewirer,
		setupLiveDataSource,
		type LiveDataSource
	} from '$lib/magician/dashboard/useLiveDataSource';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import LiveDataRefreshButton from './LiveDataRefreshButton.svelte';
	import { sanitizeCssNonNegativeLength } from './cssUtil';
	import { cleanDuckDbValue } from './chartUtil';

	interface TableColumn {
		key: string;
		label: string;
		sortable?: boolean;
		width?: string;
	}

	export let columns: TableColumn[] = [];
	export let rows: Array<Record<string, unknown>> = [];
	export let sortKey: string = '';
	export let sortDir: 'asc' | 'desc' = 'asc';
	/** Optional per-column sort lookup key (display key -> raw sortable value key). */
	export let sortValueByKey: Record<string, string> = {};
	/** Fields rendered as stacked detail lines above each table row. */
	export let stackedFields: Array<{ key: string; label?: string }> = [];
	/** Max visible lines for each stacked detail value. 0 means no clamp. */
	export let stackedValueMaxLines: number = 0;
	/** Render stacked detail rows behind a per-row Details toggle. */
	export let stackedRowsExpandable: boolean = false;
	/** Let compact tables fit the container instead of scrolling horizontally. */
	export let wrapMode: boolean = false;
	/** When true, skip internal sort — caller already sorted (R62). */
	export let presorted: boolean = false;
	/** Callback when an action button is clicked. Receives (rowId, actionId). */
	export let onAction: ((rowId: string, actionId: string) => void) | null = null;
	/** Callback after a user changes the table sort. */
	export let onSort: ((sortKey: string, sortDir: 'asc' | 'desc') => void) | null = null;
	/** Row key used to extract the row identifier for action callbacks. */
	export let rowIdKey: string = 'id';

	/**
	 * Live data binding — populates `rows` and `columns` from a SQL fetch
	 * on mount and on `magician:dashboard-refresh`. If `columns` is empty
	 * when the fetch returns, it's auto-derived from the result's column
	 * list with `sortable: true`.
	 */
	export let dataSource: LiveDataSource | null = null;

	let liveError: string | null = null;
	let liveRefreshing: boolean = false;

	$: compactLiveError =
		liveError && liveError.length > 240 ? `${liveError.slice(0, 237)}...` : liveError;

	let liveMounted = false;
	/** True once `columns` was auto-derived from a live result (vs passed
	 *  by the caller) — those refresh with each live result. */
	let liveAutoDerivedColumns = false;

	const liveRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				liveRefreshing = true;
			},
			onRows: (result) => {
				rows = result.records;
				// Re-derive auto columns on every live result (a rewired query
				// may carry a different column list); caller-supplied columns
				// are never overwritten.
				if (columns.length === 0 || liveAutoDerivedColumns) {
					columns = result.columns.map((c) => ({ key: c, label: c, sortable: true }));
					liveAutoDerivedColumns = true;
				}
				liveError = null;
			},
			onError: (err) => {
				liveError = err.message;
			},
			onSettled: () => {
				liveRefreshing = false;
			}
		})
	);

	onMount(() => {
		liveMounted = true;
	});

	// Initial wiring AND rewiring when the bound query changes: filter-driven
	// pages rebuild `dataSource.sql` reactively, and a controller frozen at
	// mount would keep re-fetching the original query on every refresh
	// event. Static dataSource props never retrigger this (key comparison
	// inside the rewirer).
	$: if (liveMounted) liveRewirer.sync(dataSource);

	onDestroy(() => {
		liveRewirer.destroy();
	});

	function refreshLiveData(): void {
		if (!liveRewirer.controller || liveRefreshing) return;
		void liveRewirer.controller.refresh();
	}

	const rowIdentity = new WeakMap<Record<string, unknown>, string>();
	let rowIdentitySeq = 0;

	function normalizeSortDir(value: unknown): 'asc' | 'desc' {
		return value === 'desc' ? 'desc' : 'asc';
	}

	function asString(value: unknown): string {
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') return String(value);
		return '';
	}

	function isSortableKey(key: string): boolean {
		return columns.some((col) => col.key === key && !!col.sortable);
	}

	function formatCell(rawValue: unknown): string {
		const value = cleanDuckDbValue(rawValue);
		if (value == null) return '';
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		// R451: Guard against invalid Date objects that throw on toISOString()
		if (value instanceof Date) {
			try { return value.toISOString(); } catch { return 'Invalid Date'; }
		}
		try {
			return JSON.stringify(value);
		} catch {
			return String(value);
		}
	}

	function hasCellValue(value: unknown): boolean {
		return value !== null && value !== undefined && String(value).length > 0;
	}

	function stackedFieldLabel(field: { key: string; label?: string }): string {
		if (field.label && field.label.trim().length > 0) return field.label;
		return columns.find((col) => col.key === field.key)?.label ?? field.key;
	}

	function columnWidthStyle(column: TableColumn): string {
		const width = sanitizeCssNonNegativeLength(column.width, '');
		return width ? `width: ${width};` : '';
	}

	function asTableCellLink(value: unknown): { href: string; label: string } | null {
		if (value == null || typeof value !== 'object' || Array.isArray(value)) return null;
		const record = value as Record<string, unknown>;
		const kind = asString(record.kind).trim();
		const rawHref = asString((record as { href?: unknown }).href).trim() || asString((record as { url?: unknown }).url).trim();
		if (kind !== 'link' || !rawHref) return null;
		const lowered = rawHref.toLowerCase();
		if (lowered.startsWith('javascript:') || lowered.startsWith('data:') || lowered.startsWith('vbscript:')) {
			return null;
		}
		const label = asString(record.label).trim() || rawHref;
		return { href: rawHref, label };
	}

	type BadgeColor = 'default' | 'success' | 'warning' | 'error' | 'info';

	function asTableCellBadge(value: unknown): { text: string; color: BadgeColor } | null {
		if (value == null || typeof value !== 'object' || Array.isArray(value)) return null;
		const record = value as Record<string, unknown>;
		const kind = asString(record.kind).trim();
		if (kind !== 'badge') return null;
		const text = asString(record.text).trim();
		if (!text) return null;
		const rawColor = asString(record.color).trim().toLowerCase();
		const color: BadgeColor =
			rawColor === 'success' || rawColor === 'warning' || rawColor === 'error' || rawColor === 'info'
				? rawColor
				: 'default';
		return { text, color };
	}

	interface TableCellAction {
		buttons: Array<{ id: string; label: string; variant?: string }>;
	}

	interface TableCellSummary {
		title: string;
		meta: string;
		links: Array<{ href: string; label: string }>;
	}

	function asTableCellActions(value: unknown): TableCellAction | null {
		if (value == null || typeof value !== 'object' || Array.isArray(value)) return null;
		const record = value as Record<string, unknown>;
		if (asString(record.kind).trim() !== 'actions') return null;
		const buttons = record.buttons;
		if (!Array.isArray(buttons)) return null;
		return {
			buttons: buttons.map((b: unknown) => {
				const btn = b as Record<string, unknown>;
				return {
					id: asString(btn.id),
					label: asString(btn.label),
					variant: asString(btn.variant) || 'default',
				};
			}).filter(b => b.id && b.label),
			};
	}

	function asTableCellSummary(value: unknown): TableCellSummary | null {
		if (value == null || typeof value !== 'object' || Array.isArray(value)) return null;
		const record = value as Record<string, unknown>;
		if (asString(record.kind).trim() !== 'summary') return null;
		const title = asString(record.title).trim();
		if (!title) return null;
		const meta = asString(record.meta).trim();
		const links = Array.isArray(record.links)
			? record.links.map((link) => asTableCellLink(link)).filter((link): link is { href: string; label: string } => link !== null)
			: [];
		return { title, meta, links };
	}

	function getRowIdentity(row: Record<string, unknown>): string {
		const existing = rowIdentity.get(row);
		if (existing) return existing;
		const next = `row-${rowIdentitySeq++}`;
		rowIdentity.set(row, next);
		return next;
	}

	function toggleSort(key: string) {
		if (sortKey === key) {
			const dir = normalizeSortDir(sortDir);
			sortDir = dir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			sortDir = 'asc';
		}
		onSort?.(sortKey, normalizeSortDir(sortDir));
	}

	function resolveSortLookupKey(key: string): string {
		const mapped = sortValueByKey[key];
		if (typeof mapped === 'string' && mapped.trim().length > 0) {
			return mapped;
		}
		return key;
	}

	$: safeSortDir = normalizeSortDir(sortDir);
	$: safeSortKey = isSortableKey(sortKey) ? sortKey : '';
	$: stackedFieldKeys = new Set(stackedFields.map((field) => field.key));
	$: visibleColumns = columns.filter((col) => !stackedFieldKeys.has(col.key));
	$: safeStackedValueMaxLines = Math.max(0, Math.min(8, Number.isFinite(+stackedValueMaxLines) ? Math.floor(+stackedValueMaxLines) : 0));

	$: sortedRows = (() => {
		if (presorted || !safeSortKey) return rows;
		// R319: Index-tagged copy preserves original position for tiebreaker
		const tagged = rows.map((row, i) => ({ row, idx: i }));
		const sortLookupKey = resolveSortLookupKey(safeSortKey);
		tagged.sort((a, b) => {
			const aVal = a.row[sortLookupKey];
			const bVal = b.row[sortLookupKey];
			if (aVal == null && bVal == null) return a.idx - b.idx;
			if (aVal == null) return 1;
			if (bVal == null) return -1;
			if (aVal < bVal) return safeSortDir === 'asc' ? -1 : 1;
			if (aVal > bVal) return safeSortDir === 'asc' ? 1 : -1;
			// R319: Tiebreaker — preserve original row order for equal values
			return a.idx - b.idx;
		});
		return tagged.map(t => t.row);
	})();
	$: rowRenderEntries = (() => {
		const seenCounts = new Map<string, number>();
		return sortedRows.map((row) => {
			const base = getRowIdentity(row);
			const count = seenCounts.get(base) ?? 0;
			seenCounts.set(base, count + 1);
			return { row, key: `${base}:${count}` };
		});
	})();
	$: visibleColspan = Math.max(1, visibleColumns.length);
</script>

<!-- R658: tabindex + role=region enables keyboard-only horizontal scrolling (WCAG 2.1.1) -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
	class="muij-table-wrapper"
	class:muij-table-wrapper-live={!!dataSource}
	class:muij-table-wrapper-wrap={wrapMode}
	class:muij-table-wrapper-clamp-stacked={safeStackedValueMaxLines > 0}
	style="--muij-table-stacked-lines: {safeStackedValueMaxLines || 1};"
	tabindex="0"
	role="region"
	aria-label="Data table"
>
	{#if dataSource}
		<LiveDataRefreshButton
			loading={liveRefreshing}
			label="Refetch table data"
			on:refresh={refreshLiveData}
		/>
	{/if}
	<table class="muij-table">
		<thead>
			<tr>
				{#each visibleColumns as col (col.key)}
					<th
						class="muij-table-th"
						class:muij-table-th-sortable={col.sortable}
						aria-sort={safeSortKey === col.key ? (safeSortDir === 'asc' ? 'ascending' : 'descending') : undefined}
						scope="col"
						style={columnWidthStyle(col)}
					>
						{#if col.sortable}
							<button
								type="button"
								class="muij-table-sort-button"
								on:click={() => toggleSort(col.key)}
								aria-label={safeSortKey === col.key ? `Sort by ${col.label}, currently ${safeSortDir === 'asc' ? 'ascending' : 'descending'}` : `Sort by ${col.label}`}
							>
								<span>{col.label}</span>
								{#if safeSortKey === col.key}
									<span class="muij-table-sort-icon" aria-hidden="true">{safeSortDir === 'asc' ? '▲' : '▼'}</span>
								{/if}
							</button>
						{:else}
							<span>{col.label}</span>
						{/if}
					</th>
				{/each}
			</tr>
		</thead>
		<tbody>
			{#if compactLiveError}
				<tr>
					<td class="muij-table-error" colspan={visibleColspan} title={liveError || ''}>
						{compactLiveError}
					</td>
				</tr>
			{:else if sortedRows.length === 0 || columns.length === 0}
				<tr>
					<td class="muij-table-empty" colspan={visibleColspan}>No data</td>
				</tr>
			{:else}
				{#each rowRenderEntries as rowEntry, i (rowEntry.key)}
					<tr class:muij-table-row-alt={i % 2 === 1} class:muij-table-row-with-stacked={stackedFields.length > 0}>
						{#each visibleColumns as col (col.key)}
							{@const cellValue = rowEntry.row[col.key]}
							{@const cellText = formatCell(cellValue)}
							{@const link = asTableCellLink(cellValue)}
							{@const summary = link ? null : asTableCellSummary(cellValue)}
							{@const badge = (!link && !summary) ? asTableCellBadge(cellValue) : null}
							{@const actions = (!link && !summary && !badge) ? asTableCellActions(cellValue) : null}
							{@const isFirstVisibleColumn = col.key === visibleColumns[0]?.key}
							{@const hasRowDetailsToggle = stackedRowsExpandable && stackedFields.length > 0 && isFirstVisibleColumn}
								<td
									class="muij-table-td"
									class:muij-table-td-summary={!!summary}
									title={!link && !summary && !badge && !actions ? cellText : undefined}
								>
									{#if summary}
										<span
											class="muij-table-summary-cell"
											class:muij-table-summary-cell-with-toggle={hasRowDetailsToggle}
										>
											{#if hasRowDetailsToggle}
												<details class="muij-table-detail-toggle">
													<summary title="Toggle details">
														<span class="muij-table-detail-toggle-closed" aria-hidden="true">
															<Icon name="chevron-down" size={14} class="muij-table-detail-icon" />
														</span>
														<span class="muij-table-detail-toggle-open" aria-hidden="true">
															<Icon name="chevron-up" size={14} class="muij-table-detail-icon" />
														</span>
														<span class="muij-table-sr-only muij-table-detail-toggle-closed">Show details</span>
														<span class="muij-table-sr-only muij-table-detail-toggle-open">Hide details</span>
													</summary>
												</details>
											{/if}
											<span class="muij-table-summary-main">
												<span class="muij-table-summary-title">{summary.title}</span>
												{#if summary.meta}
													<span class="muij-table-summary-meta">{summary.meta}</span>
												{/if}
											</span>
											<span class="muij-table-summary-actions">
												{#if summary.links.length > 0}
													<span class="muij-table-summary-links">
														{#each summary.links as summaryLink}
															<a href={summaryLink.href} class="muij-table-link">{summaryLink.label}</a>
														{/each}
													</span>
												{/if}
											</span>
										</span>
								{:else if link}
									<a href={link.href} class="muij-table-link">{link.label}</a>
								{:else if badge}
									<span
										class="muij-table-badge muij-table-badge-{badge.color}"
										role={badge.color !== 'default' ? 'status' : undefined}
										aria-label={badge.color !== 'default' ? `${badge.text} (${badge.color})` : undefined}
									>{badge.text}</span>
								{:else if actions}
									<span class="muij-table-actions">
										{#each actions.buttons as btn}
											<button
												type="button"
												class="muij-table-action-btn muij-table-action-btn--{btn.variant}"
												on:click|stopPropagation={() => {
													if (onAction) onAction(asString(rowEntry.row[rowIdKey]), btn.id);
												}}
											>{btn.label}</button>
										{/each}
									</span>
								{:else}
									{cellText}
								{/if}
								{#if !summary && hasRowDetailsToggle}
									<details class="muij-table-detail-toggle">
										<summary title="Toggle details">
											<span class="muij-table-detail-toggle-closed" aria-hidden="true">
												<Icon name="chevron-down" size={14} class="muij-table-detail-icon" />
											</span>
											<span class="muij-table-detail-toggle-open" aria-hidden="true">
												<Icon name="chevron-up" size={14} class="muij-table-detail-icon" />
											</span>
											<span class="muij-table-sr-only muij-table-detail-toggle-closed">Show details</span>
											<span class="muij-table-sr-only muij-table-detail-toggle-open">Hide details</span>
										</summary>
									</details>
								{/if}
							</td>
						{/each}
					</tr>
					{#if stackedFields.length > 0}
						<tr
							class="muij-table-detail-row"
							class:muij-table-detail-row-expandable={stackedRowsExpandable}
							class:muij-table-row-alt={i % 2 === 1}
						>
							<td class="muij-table-td muij-table-td-stacked" colspan={visibleColspan}>
								<div class="muij-table-stacked-fields">
									{#each stackedFields as field (field.key)}
										{@const stackedValue = rowEntry.row[field.key]}
										{@const stackedText = formatCell(stackedValue)}
										{#if hasCellValue(stackedValue)}
											<div class="muij-table-stacked-field">
												<span class="muij-table-stacked-label">{stackedFieldLabel(field)}:</span>
												<span class="muij-table-stacked-value" title={stackedText}>{stackedText}</span>
											</div>
										{/if}
									{/each}
								</div>
							</td>
						</tr>
					{/if}
				{/each}
			{/if}
		</tbody>
	</table>
</div>

<style>
	.muij-table-wrapper {
		position: relative;
		overflow-x: auto;
	}

	.muij-table-wrapper-wrap {
		overflow-x: visible;
	}

	.muij-table-wrapper-live {
		padding-top: 30px;
	}

	.muij-table {
		width: 100%;
		border-collapse: collapse;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.muij-table-wrapper-wrap .muij-table {
		table-layout: fixed;
	}

	.muij-table-wrapper-wrap .muij-table-th,
	.muij-table-wrapper-wrap .muij-table-td {
		padding-left: 8px;
		padding-right: 8px;
	}

	.muij-table-wrapper-wrap .muij-table-td:not(.muij-table-td-stacked) {
		overflow-wrap: normal;
	}

	.muij-table-th {
		text-align: left;
		padding: 8px 12px;
		background: var(--bg-card);
		color: var(--text-secondary);
		font-weight: 600;
		border-bottom: 1px solid var(--border-soft);
		white-space: nowrap;
	}

	.muij-table-th-sortable {
		padding: 0;
	}

	.muij-table-sort-button {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 4px;
		padding: 8px 12px;
		background: transparent;
		border: none;
		color: inherit;
		font: inherit;
		font-weight: inherit;
		text-align: left;
		cursor: pointer;
		user-select: none;
	}

	.muij-table-sort-button:hover {
		color: var(--text-primary);
	}

	.muij-table-sort-button:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	.muij-table-sort-icon {
		font-size: 0.625rem;
		margin-left: 4px;
	}

	.muij-table-td {
		padding: 8px 12px;
		color: var(--text-primary);
		border-bottom: 1px solid var(--border-soft);
		overflow-wrap: break-word;
		word-break: normal;
	}

	.muij-table-td-stacked {
		padding-bottom: 6px;
		border-bottom: 0;
	}

	.muij-table-row-alt {
		background: var(--bg-soft);
	}

	.muij-table-row-with-stacked .muij-table-td {
		padding-top: 6px;
	}

	.muij-table-detail-row-expandable {
		display: none;
	}

	.muij-table-row-with-stacked:has(.muij-table-detail-toggle[open]) + .muij-table-detail-row-expandable {
		display: table-row;
	}

	.muij-table-stacked-fields {
		display: grid;
		gap: 6px;
		min-width: 0;
	}

	.muij-table-stacked-field {
		display: grid;
		grid-template-columns: minmax(88px, max-content) minmax(0, 1fr);
		gap: 8px;
		align-items: start;
		min-width: 0;
	}

	.muij-table-stacked-label {
		color: var(--text-muted);
		font-size: 0.72rem;
		font-weight: 700;
		white-space: nowrap;
	}

	.muij-table-stacked-value {
		color: var(--text-primary);
		font-family: var(--font-mono);
		font-size: 0.75rem;
		line-height: 1.35;
		overflow-wrap: break-word;
		word-break: normal;
		white-space: normal;
	}

	.muij-table-wrapper-clamp-stacked .muij-table-stacked-value {
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: var(--muij-table-stacked-lines);
		line-clamp: var(--muij-table-stacked-lines);
		overflow: hidden;
	}

	.muij-table-wrapper-clamp-stacked .muij-table-detail-row-expandable .muij-table-stacked-value {
		display: block;
		-webkit-line-clamp: unset;
		line-clamp: unset;
		overflow: visible;
	}

	.muij-table-empty {
		padding: 16px 12px;
		color: var(--text-muted);
		text-align: center;
		font-style: italic;
	}

	.muij-table-error {
		padding: 16px 12px;
		color: var(--color-error, #b91c1c);
		background: color-mix(in srgb, var(--color-error, #b91c1c) 8%, transparent);
		border-bottom: 1px solid var(--border-soft);
		overflow-wrap: anywhere;
	}

	.muij-table-link {
		color: var(--accent-primary);
		text-decoration: underline;
	}

	.muij-table-link:hover {
		color: var(--accent-primary-hover);
	}

	.muij-table-td-summary {
		min-width: 0;
	}

	.muij-table-summary-cell {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: baseline;
		gap: 8px;
		width: 100%;
		min-width: 0;
	}

	.muij-table-summary-cell-with-toggle {
		grid-template-columns: 20px minmax(0, 1fr) auto;
		align-items: center;
		column-gap: 6px;
	}

	.muij-table-summary-main {
		display: inline-flex;
		align-items: baseline;
		flex-wrap: wrap;
		gap: 4px 8px;
		min-width: 0;
	}

	.muij-table-summary-title {
		font-weight: 650;
	}

	.muij-table-summary-meta {
		color: var(--text-muted);
		font-size: 0.75rem;
	}

	.muij-table-summary-links {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		white-space: nowrap;
	}

	.muij-table-summary-actions {
		display: inline-flex;
		align-items: baseline;
		justify-content: flex-end;
		gap: 8px;
		white-space: nowrap;
	}

	.muij-table-detail-toggle {
		display: inline-block;
		margin-left: 0;
	}

	.muij-table-detail-toggle summary {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		list-style: none;
		padding: 0;
		border: 0;
		border-radius: 4px;
		background: transparent;
		color: var(--accent-primary);
		font: inherit;
		font-size: 0.75rem;
		cursor: pointer;
		white-space: nowrap;
	}

	.muij-table-detail-toggle summary::-webkit-details-marker {
		display: none;
	}

	.muij-table-detail-toggle-open {
		display: none;
	}

	.muij-table-detail-toggle[open] .muij-table-detail-toggle-closed {
		display: none;
	}

	.muij-table-detail-toggle[open] .muij-table-detail-toggle-open {
		display: inline-flex;
	}

	.muij-table-detail-toggle summary:hover {
		color: var(--accent-primary-hover);
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
	}

	.muij-table-detail-toggle summary:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 2px;
	}

	.muij-table-sr-only {
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

	/* Badge cell styles — mirrors Badge.svelte appearance for inline table cells */
	.muij-table-badge {
		display: inline-flex;
		align-items: center;
		font-size: 0.75rem;
		font-weight: 500;
		padding: 2px 8px;
		border-radius: var(--radius-full, 9999px);
		line-height: 1.4;
		white-space: nowrap;
		overflow-wrap: normal;
		word-break: normal;
	}

	.muij-table-badge-default {
		background: var(--bg-soft);
		color: var(--text-secondary);
	}

	.muij-table-badge-success {
		background: color-mix(in srgb, var(--color-success) 15%, transparent);
		color: var(--color-success);
	}

	.muij-table-badge-warning {
		background: color-mix(in srgb, var(--color-warning, #f59e0b) 15%, transparent);
		color: var(--color-warning, #f59e0b);
	}

	.muij-table-badge-error {
		background: color-mix(in srgb, var(--color-error) 15%, transparent);
		color: var(--color-error);
	}

	.muij-table-badge-info {
		background: color-mix(in srgb, var(--accent-primary) 15%, transparent);
		color: var(--accent-primary);
	}

	/* Action cell styles */
	.muij-table-actions {
		display: flex;
		gap: 6px;
		flex-wrap: nowrap;
	}

	.muij-table-action-btn {
		padding: 4px 10px;
		border-radius: 4px;
		font-size: 12px;
		font-weight: 500;
		cursor: pointer;
		border: 1px solid var(--border-subtle, #d1d5db);
		background: var(--bg-surface, #fff);
		color: var(--text-primary, #374151);
		transition: background 0.15s, border-color 0.15s;
		white-space: nowrap;
	}

	.muij-table-action-btn:hover {
		background: var(--bg-hover, #f3f4f6);
	}

	.muij-table-action-btn--info {
		background: var(--bg-info, #eff6ff);
		border-color: var(--border-info, #93c5fd);
		color: var(--text-info, #1d4ed8);
	}

	.muij-table-action-btn--info:hover {
		background: var(--bg-info-hover, #dbeafe);
	}

	.muij-table-action-btn--danger {
		background: var(--bg-danger, #fef2f2);
		border-color: var(--border-danger, #fca5a5);
		color: var(--text-danger, #dc2626);
	}

	.muij-table-action-btn--danger:hover {
		background: var(--bg-danger-hover, #fee2e2);
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-table {
		font-family: var(--font-mono);
		border: 1px solid var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-table-th {
		background: var(--bg-surface);
		color: var(--text-primary);
		border-bottom: 2px solid var(--text-primary);
		text-transform: uppercase;
	}

	:global([data-theme^="retro-16bit"]) .muij-table-td {
		border-bottom: 1px dashed var(--text-muted);
		color: var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-table-row-alt {
		background: rgba(var(--text-primary), 0.05);
	}

	:global([data-theme^="retro-16bit"]) .muij-table-badge {
		border-radius: 0;
		border: 1px solid var(--text-primary);
		background: var(--bg-base);
		color: var(--text-primary);
		text-transform: uppercase;
	}
</style>
