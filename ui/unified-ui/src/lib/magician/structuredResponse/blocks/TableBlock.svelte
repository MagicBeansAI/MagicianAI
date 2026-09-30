<script lang="ts">
	import type { StructuredTableBlockV1 } from '../types';

	export let block: StructuredTableBlockV1;
</script>

<section class="sr-block sr-block--table" data-block-kind="table">
	{#if block.title}<h4 class="sr-block__title">{block.title}</h4>{/if}
	<div class="sr-table-wrap">
		<table class="sr-table">
			<thead>
				<tr>
					{#each block.columns as column}
						<th
							class={`sr-table__cell sr-table__cell--${column.alignment ?? 'start'}`}
							style={`text-align: ${column.alignment ?? 'left'}`}
						>
							{column.label}
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#if block.rows.length === 0}
					<tr>
						<td colspan={block.columns.length} class="sr-table__empty">No rows</td>
					</tr>
				{:else}
					{#each block.rows as row}
						<tr>
							{#each block.columns as column}
								<td class="sr-table__cell" style={`text-align: ${column.alignment ?? 'left'}`}>
									{row[column.key] ?? ''}
								</td>
							{/each}
						</tr>
					{/each}
				{/if}
			</tbody>
		</table>
	</div>
</section>

<style>
	.sr-block {
		display: block;
	}

	.sr-block__title {
		font-size: var(--text-sm);
		font-weight: 650;
		margin: 0 0 0.4rem;
	}

	.sr-table-wrap {
		overflow-x: auto;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
	}

	.sr-table {
		border-collapse: collapse;
		width: 100%;
		font-size: var(--text-sm);
		table-layout: fixed;
	}

	.sr-table th,
	.sr-table td {
		padding: 0.42rem 0.48rem;
		border-bottom: 1px solid var(--border);
		vertical-align: top;
	}

	.sr-table th {
		font-size: 12px;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
	}

	.sr-table__empty {
		text-align: left;
		color: var(--text-muted);
		font-style: italic;
	}
</style>
