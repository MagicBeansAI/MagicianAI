<!--
  CallsTable — presentational, metadata-only per-call log for the /llm Calls
  tab. Renders rows produced by `buildCallsSql` (LLM ops) and/or
  `buildEmbeddingCallsSql` (embeddings) — content-free: no prompt, response,
  or vector text ever reaches this component. Rows may be tagged with a
  `kind` (`'llm'` | `'embedding'`); the Type column reads that tag (falling
  back to `batch_size` presence) so the "All" view can mix both. Embedding
  rows have no output_tokens/ttft_ms/agent_id and LLM rows have no batch_size
  — every cell degrades to an em dash when its column is absent. Pagination is
  owner-driven: the parent appends more rows and toggles `loading`; this
  component only renders what it's given and fires `onLoadMore` when asked.
-->
<script lang="ts">
	import { modelCost } from '$lib/llm/decisionModels';
	export let rows: any[] = [];
	export let loading = false;
	export let onLoadMore: () => void = () => {};

	function cell(row: Record<string, unknown>, key: string): unknown {
		return row?.[key];
	}

	function toNumber(value: unknown): number | null {
		if (typeof value === 'number' && Number.isFinite(value)) return value;
		if (typeof value === 'string' && value.trim() !== '') {
			const parsed = Number(value);
			return Number.isFinite(parsed) ? parsed : null;
		}
		return null;
	}

	function text(value: unknown): string {
		if (value == null) return '—';
		const str = String(value);
		return str.trim() === '' ? '—' : str;
	}

	const timeFmt = new Intl.DateTimeFormat(undefined, {
		month: 'short',
		day: 'numeric',
		hour: 'numeric',
		minute: '2-digit',
		second: '2-digit'
	});

	function formatTime(value: unknown): string {
		const ms = toNumber(value);
		if (ms == null) return '—';
		return timeFmt.format(new Date(ms));
	}

	function formatInt(value: unknown): string {
		const num = toNumber(value);
		return num == null ? '—' : Math.round(num).toLocaleString();
	}

	function formatCost(value: unknown): string {
		return value == null ? '—' : modelCost(value);
	}

	function formatMs(value: unknown): string {
		const num = toNumber(value);
		if (num == null) return '—';
		return num >= 1000 ? `${(num / 1000).toFixed(2)}s` : `${Math.round(num)}ms`;
	}

	function isSuccess(value: unknown): boolean | null {
		if (typeof value === 'boolean') return value;
		if (value === 1 || value === '1' || value === 'true') return true;
		if (value === 0 || value === '0' || value === 'false') return false;
		return null;
	}

	function successLabel(value: unknown): string {
		const state = isSuccess(value);
		if (state == null) return '—';
		return state ? 'ok' : 'failed';
	}

	function rowKey(row: Record<string, unknown>, index: number): string {
		const id = row?.llm_call_id;
		return typeof id === 'string' && id.length > 0 ? id : `row-${index}`;
	}

	// A row is an embedding when it's tagged `kind: 'embedding'` or — as a
	// fallback for untagged rows — when it carries a `batch_size` (a column
	// that only exists on the llm_embeddings relation).
	function isEmbedding(row: Record<string, unknown>): boolean {
		if (row?.kind === 'embedding') return true;
		if (row?.kind === 'llm') return false;
		return row?.batch_size != null;
	}

	function typeLabel(row: Record<string, unknown>): string {
		return isEmbedding(row) ? 'embed' : String(row.provider ?? '').startsWith('decision:') ? 'decision' : 'llm';
	}
</script>

<div class="calls" aria-label="Per-call LLM log">
	{#if rows.length === 0 && !loading}
		<p class="calls-empty">
			No calls matched these filters. Widen the range or clear a filter to see individual LLM
			calls.
		</p>
	{:else}
		<div class="calls-table" role="table">
			<div class="calls-row calls-row--head" role="row">
				<span role="columnheader">Time</span>
				<span role="columnheader">Type</span>
				<span role="columnheader">Operation</span>
				<span role="columnheader">Provider</span>
				<span role="columnheader">Model</span>
				<span role="columnheader">Agent</span>
				<span role="columnheader" class="calls-num">In</span>
				<span role="columnheader" class="calls-num">Out</span>
				<span role="columnheader" class="calls-num">Cache read</span>
				<span role="columnheader" class="calls-num">Cache write</span>
				<span role="columnheader" class="calls-num">Batch</span>
				<span role="columnheader" class="calls-num">Cost</span>
				<span role="columnheader" class="calls-num">Latency</span>
				<span role="columnheader" class="calls-num">TTFT</span>
				<span role="columnheader">Status</span>
			</div>
			{#each rows as row, index (rowKey(row, index))}
				<div class="calls-row" role="row">
					<span role="cell" class="calls-time">{formatTime(cell(row, 'timestamp_ms'))}</span>
					<span role="cell" class="calls-type" data-type={typeLabel(row)}>{typeLabel(row)}</span>
					<span role="cell" title={text(cell(row, 'operation'))}>{text(cell(row, 'operation'))}</span>
					<span role="cell" title={text(cell(row, 'provider'))}>{text(cell(row, 'provider'))}</span>
					<span role="cell" title={text(cell(row, 'model'))}>{text(cell(row, 'model'))}</span>
					<span role="cell" title={text(cell(row, 'agent_id'))}>{text(cell(row, 'agent_id'))}</span>
					<span role="cell" class="calls-num">{formatInt(cell(row, 'input_tokens'))}</span>
					<span role="cell" class="calls-num">{formatInt(cell(row, 'output_tokens'))}</span>
					<span role="cell" class="calls-num">{formatInt(cell(row, 'cache_read_tokens'))}</span>
					<span role="cell" class="calls-num">{formatInt(cell(row, 'cache_creation_tokens'))}</span>
					<span role="cell" class="calls-num">{formatInt(cell(row, 'batch_size'))}</span>
					<span role="cell" class="calls-num">{formatCost(cell(row, 'cost_usd'))}</span>
					<span role="cell" class="calls-num">{formatMs(cell(row, 'latency_ms'))}</span>
					<span role="cell" class="calls-num">{formatMs(cell(row, 'ttft_ms'))}</span>
					<span
						role="cell"
						class="calls-status"
						data-status={successLabel(cell(row, 'success'))}
					>
						{successLabel(cell(row, 'success'))}
					</span>
				</div>
			{/each}
		</div>
	{/if}

	<div class="calls-footer">
		{#if loading}
			<span class="calls-loading">Loading calls…</span>
		{:else if rows.length > 0}
			<button type="button" class="calls-more" on:click={onLoadMore}>Load more</button>
		{/if}
	</div>
</div>

<style>
	.calls {
		display: flex;
		flex-direction: column;
		gap: 12px;
		min-width: 0;
	}

	.calls-empty,
	.calls-loading {
		margin: 0;
		color: var(--theme-color-foreground-muted, #6b7280);
		font-family: var(--theme-font-body);
		font-size: 0.86rem;
	}

	.calls-table {
		display: flex;
		flex-direction: column;
		min-width: 0;
		overflow-x: auto;
	}

	.calls-row {
		display: grid;
		grid-template-columns:
			minmax(8.5rem, 0.9fr) minmax(3.5rem, 0.35fr) minmax(7rem, 1fr) minmax(5rem, 0.6fr)
			minmax(7rem, 0.9fr) minmax(6rem, 0.8fr) minmax(3.5rem, 0.4fr) minmax(3.5rem, 0.4fr)
			minmax(3.5rem, 0.4fr) minmax(4rem, 0.5fr) minmax(4.5rem, 0.5fr) minmax(4.5rem, 0.5fr)
			minmax(4rem, 0.45fr) minmax(4rem, 0.45fr) minmax(4rem, 0.45fr);
		gap: 12px;
		align-items: center;
		padding: 9px 0;
		border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.08));
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.83rem;
		font-variant-numeric: tabular-nums;
		min-width: 1000px;
	}

	.calls-row:first-child {
		border-top: none;
	}

	.calls-row--head {
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.68rem;
		font-weight: 700;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}

	.calls-row > span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.calls-num {
		text-align: right;
	}

	.calls-time {
		color: var(--theme-color-foreground-muted, #6b7280);
	}

	/* Type badge — small pill distinguishing embedding rows (blue-ish accent)
	 * from LLM-op rows (muted) so the "All" view reads at a glance. */
	.calls-type {
		justify-self: start;
		padding: 1px 7px;
		border-radius: 999px;
		background: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.05));
		color: var(--theme-color-foreground-muted, #6b7280);
		font-size: 0.68rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.calls-type[data-type='embed'] {
		background: color-mix(in srgb, var(--theme-color-accent, #4ecdc4) 16%, transparent);
		color: color-mix(in srgb, var(--theme-color-accent, #0891b2) 78%, var(--theme-color-foreground, #111827));
	}

	.calls-status {
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--theme-color-foreground-muted, #6b7280);
	}

	.calls-status[data-status='ok'] {
		color: var(--color-success, #15803d);
	}

	.calls-status[data-status='failed'] {
		color: var(--color-error, #b91c1c);
	}

	.calls-footer {
		display: flex;
		justify-content: center;
		min-height: 1.5rem;
	}

	.calls-more {
		padding: 7px 16px;
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.12));
		border-radius: 8px;
		background-color: var(--theme-color-surface, #fff);
		color: var(--theme-color-foreground, #111827);
		font-family: var(--theme-font-body);
		font-size: 0.8rem;
		font-weight: 650;
		cursor: pointer;
	}

	.calls-more:hover {
		background-color: var(--theme-color-surface-muted, rgba(0, 0, 0, 0.04));
	}
</style>
