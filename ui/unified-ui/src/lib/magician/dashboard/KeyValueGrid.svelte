<!--
  KeyValueGrid — renders a single object as a two-column key/value grid.
  Used by the published-surface dispatcher when JSON body is an object.

  Key column: display font, muted color, uppercase + letterspaced.
  Value column: monospace for scalars, recursive JsonViewer for nested.
-->
<script lang="ts">
	import JsonViewer from './JsonViewer.svelte';

	export let object: Record<string, unknown> = {};

	function isScalar(v: unknown): boolean {
		return v === null || typeof v !== 'object';
	}

	function formatScalar(v: unknown): string {
		if (v === null || v === undefined) return '—';
		if (typeof v === 'string') return v;
		if (typeof v === 'number') {
			return new Intl.NumberFormat(undefined, { maximumFractionDigits: 6 }).format(v);
		}
		if (typeof v === 'boolean') return v ? 'true' : 'false';
		return JSON.stringify(v);
	}

	$: entries = Object.entries(object);
</script>

<dl class="kv-grid">
	{#each entries as [key, value] (key)}
		<dt>{key}</dt>
		<dd>
			{#if isScalar(value)}
				<span class="scalar">{formatScalar(value)}</span>
			{:else}
				<JsonViewer node={value} depth={0} />
			{/if}
		</dd>
	{/each}
</dl>

<style>
	.kv-grid {
		display: grid;
		grid-template-columns: minmax(180px, 0.3fr) 1fr;
		gap: 8px 24px;
		margin: 0;
		padding: 16px 0;
		font-family: var(--theme-font-body);
		color: var(--theme-color-foreground);
	}

	.kv-grid dt {
		font-family: var(--theme-font-display, var(--theme-font-body));
		font-size: 0.78rem;
		font-weight: 600;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--theme-color-foreground-muted);
		padding-top: 6px;
		border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.06));
	}

	.kv-grid dd {
		margin: 0;
		padding-top: 6px;
		border-top: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.06));
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.scalar {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-variant-numeric: tabular-nums;
		color: var(--theme-color-foreground);
	}
</style>
