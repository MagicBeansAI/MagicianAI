<!--
  AutoTable — renders Array<Record<string, unknown>> as a sortable,
  theme-styled table. Used by the generic-JSON dispatcher in the published-
  surface renderer when the payload is an array of objects.

  Auto-detects column types from the first 10 rows and formats accordingly:
    number    → right-aligned, monospace, Intl-formatted
    currency  → detect by field name containing 'cost' / 'usd' / 'price' →
                Intl currency formatter
    date      → ISO-string detection → locale date
    boolean   → ✓ / ✗ pill
    url       → underlined link

  Sortable headers, sticky header on scroll, paginated past 100 rows.
-->
<script lang="ts">
	import BarChart from '$lib/magician/components/generative/BarChart.svelte';
	import ServerPager from '$lib/shared/components/ServerPager.svelte';

	export let rows: Array<Record<string, unknown>> = [];
	export let pageSize: number = 100;
	/** Set to `false` to suppress the auto-derived companion BarChart. */
	export let showCompanionChart: boolean = true;

	let sortKey: string | null = null;
	let sortDir: 'asc' | 'desc' = 'asc';
	let page: number = 0;

	$: columns = inferColumns(rows);
	$: companion = showCompanionChart ? inferCompanionChart(rows, columns) : null;
	$: sortedRows = sortKey ? sortRows(rows, sortKey, sortDir) : rows;
	$: safePageSize = Math.max(1, Number.isFinite(+pageSize) ? Math.floor(+pageSize) : 100);
	$: pageCount = Math.max(1, Math.ceil(sortedRows.length / safePageSize));
	$: if (page >= pageCount) page = Math.max(0, pageCount - 1);
	$: clampedPage = Math.max(0, Math.min(pageCount - 1, page));
	$: pageStart = sortedRows.length === 0 ? 0 : clampedPage * safePageSize + 1;
	$: pageEnd = Math.min(sortedRows.length, clampedPage * safePageSize + safePageSize);
	$: pagedRows = sortedRows.slice(clampedPage * safePageSize, (clampedPage + 1) * safePageSize);

	interface ColumnSpec {
		key: string;
		kind: 'number' | 'currency' | 'date' | 'boolean' | 'url' | 'text';
	}

	function inferColumns(input: Array<Record<string, unknown>>): ColumnSpec[] {
		if (input.length === 0) return [];
		const keys = new Set<string>();
		for (const row of input.slice(0, 10)) {
			for (const k of Object.keys(row)) keys.add(k);
		}
		const sample = input.slice(0, 10);
		return Array.from(keys).map((key) => {
			const kind = inferKind(key, sample.map((r) => r[key]));
			return { key, kind };
		});
	}

	function inferKind(key: string, values: unknown[]): ColumnSpec['kind'] {
		const nonNull = values.filter((v) => v !== null && v !== undefined);
		if (nonNull.length === 0) return 'text';
		const lower = key.toLowerCase();
		if (/cost|usd|price|spend|amount/.test(lower) && nonNull.every((v) => typeof v === 'number')) {
			return 'currency';
		}
		if (nonNull.every((v) => typeof v === 'number')) return 'number';
		if (nonNull.every((v) => typeof v === 'boolean')) return 'boolean';
		if (nonNull.every((v) => typeof v === 'string' && /^https?:\/\//.test(v))) return 'url';
		if (
			nonNull.every(
				(v) => typeof v === 'string' && /^\d{4}-\d{2}-\d{2}/.test(v) && !Number.isNaN(Date.parse(v))
			)
		) {
			return 'date';
		}
		return 'text';
	}

	function sortRows(
		input: Array<Record<string, unknown>>,
		key: string,
		dir: 'asc' | 'desc'
	): Array<Record<string, unknown>> {
		const sorted = [...input].sort((a, b) => compareValues(a[key], b[key]));
		return dir === 'asc' ? sorted : sorted.reverse();
	}

	function compareValues(a: unknown, b: unknown): number {
		if (a === b) return 0;
		if (a === null || a === undefined) return 1;
		if (b === null || b === undefined) return -1;
		if (typeof a === 'number' && typeof b === 'number') return a - b;
		return String(a).localeCompare(String(b), undefined, { numeric: true, sensitivity: 'base' });
	}

	function onSort(key: string): void {
		if (sortKey === key) {
			sortDir = sortDir === 'asc' ? 'desc' : 'asc';
		} else {
			sortKey = key;
			sortDir = 'asc';
		}
	}

	function goToPage(pageNumber: number): void {
		const safePage = Math.min(pageCount, Math.max(1, Math.floor(pageNumber)));
		page = safePage - 1;
	}

	function formatCurrency(v: unknown): string {
		if (typeof v !== 'number') return String(v ?? '—');
		return new Intl.NumberFormat(undefined, {
			style: 'currency',
			currency: 'USD',
			maximumFractionDigits: 2
		}).format(v);
	}

	function formatNumber(v: unknown): string {
		if (typeof v !== 'number') return String(v ?? '—');
		return new Intl.NumberFormat(undefined, { maximumFractionDigits: 3 }).format(v);
	}

	function formatDate(v: unknown): string {
		if (typeof v !== 'string') return String(v ?? '—');
		const ms = Date.parse(v);
		if (!Number.isFinite(ms)) return v;
		return new Date(ms).toLocaleString();
	}

	interface CompanionChart {
		title: string;
		bars: Array<{ label: string; value: number }>;
	}

	/**
	 * Derives a BarChart companion from raw rows: picks the first numeric/
	 * currency column as the metric, the first text column with sensible
	 * cardinality (1 < unique <= 30) as the category, then aggregates and
	 * takes the top 12 bars by descending metric. Returns null when no
	 * sensible pairing is possible — e.g. tables that are pure text,
	 * single-column, or have too many distinct categories to chart.
	 */
	function inferCompanionChart(
		input: Array<Record<string, unknown>>,
		cols: ColumnSpec[]
	): CompanionChart | null {
		if (input.length < 2 || cols.length < 2) return null;
		const metric = cols.find((c) => c.kind === 'number' || c.kind === 'currency');
		if (!metric) return null;
		const candidates = cols.filter((c) => c.key !== metric.key && c.kind === 'text');
		let chosen: ColumnSpec | null = null;
		for (const candidate of candidates) {
			const values = new Set<string>();
			for (const row of input) {
				const v = row[candidate.key];
				if (v === null || v === undefined) continue;
				values.add(String(v));
			}
			if (values.size > 1 && values.size <= 30) {
				chosen = candidate;
				break;
			}
		}
		if (!chosen) return null;
		const agg = new Map<string, number>();
		for (const row of input) {
			const label = row[chosen.key];
			const value = row[metric.key];
			if (label === null || label === undefined) continue;
			if (typeof value !== 'number' || !Number.isFinite(value)) continue;
			const key = String(label);
			agg.set(key, (agg.get(key) ?? 0) + value);
		}
		if (agg.size === 0) return null;
		const bars = Array.from(agg.entries())
			.map(([label, value]) => ({ label, value }))
			.sort((a, b) => b.value - a.value)
			.slice(0, 12);
		if (bars.length < 2) return null;
		return {
			title: `${metric.key} by ${chosen.key}`,
			bars
		};
	}
</script>

{#if rows.length === 0}
	<div class="auto-table-empty">No rows.</div>
{:else}
	{#if companion}
		<div class="auto-table-companion">
			<div class="auto-table-companion-title">{companion.title}</div>
			<BarChart data={companion.bars} horizontal={true} />
		</div>
	{/if}
	{#if pageCount > 1}
		<div class="auto-table-pager">
			<ServerPager
				currentPage={clampedPage + 1}
				pageCount={pageCount}
				startItem={pageStart}
				endItem={pageEnd}
				totalItems={sortedRows.length}
				ariaLabel="Table pagination"
				on:pagechange={(event) => goToPage(event.detail.page)}
			/>
		</div>
	{/if}
	<div class="auto-table-wrap">
		<table class="auto-table">
			<thead>
				<tr>
					{#each columns as col (col.key)}
						<th
							class:numeric={col.kind === 'number' || col.kind === 'currency'}
							class:sort-asc={sortKey === col.key && sortDir === 'asc'}
							class:sort-desc={sortKey === col.key && sortDir === 'desc'}
							on:click={() => onSort(col.key)}
						>
							{col.key}
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each pagedRows as row, ri (ri)}
					<tr>
						{#each columns as col (col.key)}
							<td class:numeric={col.kind === 'number' || col.kind === 'currency'}>
								{#if col.kind === 'currency'}
									<span class="value-mono">{formatCurrency(row[col.key])}</span>
								{:else if col.kind === 'number'}
									<span class="value-mono">{formatNumber(row[col.key])}</span>
								{:else if col.kind === 'boolean'}
									<span class="bool-pill">{row[col.key] ? '✓' : '✗'}</span>
								{:else if col.kind === 'url'}
									<a href={String(row[col.key] ?? '')} target="_blank" rel="noopener noreferrer">
										{String(row[col.key] ?? '')}
									</a>
								{:else if col.kind === 'date'}
									{formatDate(row[col.key])}
								{:else}
									{row[col.key] === null || row[col.key] === undefined ? '—' : String(row[col.key])}
								{/if}
							</td>
						{/each}
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
	{#if pageCount > 1}
		<div class="auto-table-pager">
			<ServerPager
				currentPage={clampedPage + 1}
				pageCount={pageCount}
				startItem={pageStart}
				endItem={pageEnd}
				totalItems={sortedRows.length}
				ariaLabel="Table pagination"
				on:pagechange={(event) => goToPage(event.detail.page)}
			/>
		</div>
	{/if}
{/if}

<style>
	.auto-table-wrap {
		overflow: auto;
		max-height: 70vh;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 8px;
		background-color: var(--theme-color-surface, #fff);
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}

	.auto-table {
		width: 100%;
		border-collapse: separate;
		border-spacing: 0;
		font-family: var(--theme-font-body, system-ui);
		font-size: 0.9rem;
	}

	.auto-table thead th {
		position: sticky;
		top: 0;
		background-color: var(--theme-color-background, #FAF8F4);
		padding: 10px 14px;
		text-align: left;
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.72rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground, #1A1814);
		cursor: pointer;
		user-select: none;
		border-bottom: 1px solid var(--theme-color-border);
		white-space: nowrap;
	}

	.auto-table thead th.numeric {
		text-align: right;
	}

	.auto-table thead th:hover {
		background-color: var(--theme-color-surface);
	}

	.auto-table thead th.sort-asc::after {
		content: ' ↑';
		color: var(--theme-color-accent);
	}
	.auto-table thead th.sort-desc::after {
		content: ' ↓';
		color: var(--theme-color-accent);
	}

	.auto-table tbody td {
		padding: 9px 14px;
		border-bottom: 1px solid var(--theme-color-border);
		color: var(--theme-color-foreground);
		vertical-align: top;
	}

	.auto-table tbody td.numeric {
		text-align: right;
	}

	.auto-table tbody tr:nth-child(even) td {
		background-color: color-mix(in srgb, var(--theme-color-background) 50%, transparent);
	}

	.auto-table tbody tr:last-child td {
		border-bottom: none;
	}

	.auto-table tbody tr:hover td {
		background-color: color-mix(in srgb, var(--theme-color-accent) 8%, transparent);
	}

	.value-mono {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-variant-numeric: tabular-nums;
	}

	.bool-pill {
		font-family: var(--theme-font-mono);
		display: inline-block;
		min-width: 20px;
		text-align: center;
	}

	.auto-table tbody td a {
		color: var(--theme-color-accent);
		text-decoration: underline;
	}

	.auto-table-empty {
		padding: 24px;
		color: var(--theme-color-foreground-muted);
		font-style: italic;
		text-align: center;
		border: 1px dashed var(--theme-color-border);
		border-radius: 8px;
	}

	.auto-table-pager {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		margin: 12px 0;
		font-family: var(--theme-font-body);
		font-size: 0.85rem;
	}

	.auto-table-companion {
		margin-bottom: 16px;
		padding: 16px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		border-radius: 8px;
		background-color: var(--theme-color-surface, #fff);
	}

	.auto-table-companion-title {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.72rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted, var(--theme-color-foreground));
		margin-bottom: 10px;
	}
</style>
