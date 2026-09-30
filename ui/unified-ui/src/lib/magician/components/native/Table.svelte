<script lang="ts">
	interface TableColumn {
		key: string;
		label: string;
		sortable?: boolean;
		width?: string;
	}

	export let columns: TableColumn[] = [];
	export let rows: Array<Record<string, unknown>> = [];
	export let sortKey = '';
	export let sortDir: 'asc' | 'desc' = 'asc';
	export let presorted = false;

	function formatCell(value: unknown): string {
		if (value == null) return '';
		if (typeof value === 'string') return value;
		if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
			return String(value);
		}
		try {
			return JSON.stringify(value);
		} catch {
			return String(value);
		}
	}

	$: safeRows = Array.isArray(rows) ? rows : [];
	$: safeColumns = Array.isArray(columns) ? columns : [];
	$: displayRows = presorted || !sortKey
		? safeRows
		: [...safeRows].sort((a, b) => {
				const av = formatCell(a[sortKey]).toLowerCase();
				const bv = formatCell(b[sortKey]).toLowerCase();
				const cmp = av.localeCompare(bv, undefined, { numeric: true, sensitivity: 'base' });
				return sortDir === 'desc' ? -cmp : cmp;
			});
</script>

<div class="native-table-wrap">
	<table class="native-table">
		<thead>
			<tr>
				{#each safeColumns as column (column.key)}
					<th style={column.width ? `width:${column.width};` : undefined}>{column.label}</th>
				{/each}
			</tr>
		</thead>
		<tbody>
			{#if displayRows.length === 0}
				<tr>
					<td colspan={Math.max(1, safeColumns.length)} class="native-table__empty">No rows</td>
				</tr>
			{:else}
				{#each displayRows as row, index (row.id ?? index)}
					<tr>
						{#each safeColumns as column (column.key)}
							<td>{formatCell(row[column.key])}</td>
						{/each}
					</tr>
				{/each}
			{/if}
		</tbody>
	</table>
</div>

<style>
	.native-table-wrap {
		width: 100%;
		overflow: auto;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
	}

	.native-table {
		width: 100%;
		border-collapse: collapse;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-body);
	}

	.native-table th,
	.native-table td {
		padding: 0.5rem 0.625rem;
		border-bottom: 1px solid var(--border-soft);
		text-align: left;
		vertical-align: top;
		overflow-wrap: anywhere;
	}

	.native-table th {
		background: var(--bg-soft);
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
	}

	.native-table tbody tr:last-child td {
		border-bottom: 0;
	}

	.native-table tbody tr:hover td {
		background: color-mix(in srgb, var(--bg-soft) 72%, transparent);
	}

	.native-table__empty {
		color: var(--text-muted);
		text-align: center;
	}
</style>
