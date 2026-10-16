<!--
  JsonViewer — collapsible, syntax-colored JSON tree.

  Used by the published-surface dispatcher when JSON payload is neither
  MUI-JSON nor an array of objects nor a flat object. Also used recursively
  by KeyValueGrid for nested object/array values.

  Keys get accent color, strings get foreground, numbers get a distinct
  shade, booleans get a colored pill, null/undefined gets a muted dash.
  Objects/arrays show length + click to collapse/expand. Default depth 2
  expanded (root + 2 levels), deeper levels start collapsed.
-->
<script lang="ts">
	export let node: unknown = null;
	export let depth: number = 0;

	const INITIAL_OPEN_DEPTH = 2;

	let open: boolean = depth <= INITIAL_OPEN_DEPTH;

	function kind(v: unknown): 'array' | 'object' | 'string' | 'number' | 'boolean' | 'null' {
		if (v === null) return 'null';
		if (Array.isArray(v)) return 'array';
		const t = typeof v;
		if (t === 'object') return 'object';
		if (t === 'string') return 'string';
		if (t === 'number') return 'number';
		if (t === 'boolean') return 'boolean';
		return 'null';
	}

	function toggle(): void {
		open = !open;
	}
</script>

{#if node === null || node === undefined}
	<span class="json-null">null</span>
{:else if typeof node === 'string'}
	<span class="json-string">"{node}"</span>
{:else if typeof node === 'number'}
	<span class="json-number">{node}</span>
{:else if typeof node === 'boolean'}
	<span class="json-boolean">{node ? 'true' : 'false'}</span>
{:else if Array.isArray(node)}
	<span class="json-collapsible">
		<button type="button" class="json-toggle" on:click={toggle} aria-expanded={open}>
			{open ? '▾' : '▸'}
		</button>
		<span class="json-bracket">[</span>
		{#if !open}
			<span class="json-summary">{node.length} item{node.length === 1 ? '' : 's'}</span>
		{/if}
		{#if open}
			<ul class="json-list">
				{#each node as item, i (i)}
					<li>
						<span class="json-index">{i}</span>
						<svelte:self node={item} depth={depth + 1} />
					</li>
				{/each}
			</ul>
		{/if}
		<span class="json-bracket">]</span>
	</span>
{:else}
	{@const entries = Object.entries(node)}
	<span class="json-collapsible">
		<button type="button" class="json-toggle" on:click={toggle} aria-expanded={open}>
			{open ? '▾' : '▸'}
		</button>
		<span class="json-bracket">{'{'}</span>
		{#if !open}
			<span class="json-summary">{entries.length} key{entries.length === 1 ? '' : 's'}</span>
		{/if}
		{#if open}
			<ul class="json-list">
				{#each entries as [key, value] (key)}
					<li>
						<span class="json-key">"{key}"</span>:
						<svelte:self node={value} depth={depth + 1} />
					</li>
				{/each}
			</ul>
		{/if}
		<span class="json-bracket">{'}'}</span>
	</span>
{/if}

<style>
	.json-collapsible {
		display: inline;
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 0.875rem;
		line-height: 1.5;
	}

	.json-toggle {
		font-family: inherit;
		font-size: 0.75rem;
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--theme-color-foreground-muted);
		padding: 0 4px 0 0;
	}

	.json-bracket {
		color: var(--theme-color-foreground-muted);
	}

	.json-list {
		list-style: none;
		margin: 0;
		padding-left: 16px;
		display: block;
	}

	.json-list > li {
		display: block;
		padding: 1px 0;
	}

	.json-key {
		color: var(--theme-color-accent);
		font-weight: 500;
	}

	.json-string {
		color: var(--theme-color-foreground);
	}

	.json-number {
		color: var(--theme-color-accent-alt, var(--theme-color-accent));
		font-variant-numeric: tabular-nums;
	}

	.json-boolean {
		color: var(--theme-color-accent);
		font-weight: 500;
	}

	.json-null {
		color: var(--theme-color-foreground-muted);
		font-style: italic;
	}

	.json-summary {
		color: var(--theme-color-foreground-muted);
		font-style: italic;
		padding: 0 4px;
	}

	.json-index {
		color: var(--theme-color-foreground-muted);
		padding-right: 6px;
		font-size: 0.78rem;
	}
</style>
