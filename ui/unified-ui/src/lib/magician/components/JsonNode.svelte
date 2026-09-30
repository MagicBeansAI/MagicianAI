<script lang="ts">
	import { getContext } from 'svelte';
	import type { Writable } from 'svelte/store';

	export let node: unknown;
	export let path = '';
	export let depth = 0;

	const expandedState = getContext<Writable<Record<string, boolean>>>('jsonExpandedState');
	const searchTerm = getContext<Writable<string>>('jsonSearchTerm');

	function isExpanded(state: Record<string, boolean>, p: string): boolean {
		if (p in state) return state[p];
		return depth < 1; // auto-expand root level
	}

	function toggle() {
		$expandedState = { ...$expandedState, [path]: !isExpanded($expandedState, path) };
	}

	function matchesSearch(value: unknown, term: string): boolean {
		if (!term) return false;
		const s = term.toLowerCase();
		if (value == null) return 'null'.includes(s);
		if (typeof value === 'string') return value.toLowerCase().includes(s);
		if (typeof value === 'number' || typeof value === 'boolean') return String(value).toLowerCase().includes(s);
		return false;
	}

	function keyMatch(key: string, term: string): boolean {
		if (!term) return false;
		return key.toLowerCase().includes(term.toLowerCase());
	}

	function subtreeMatch(obj: unknown, term: string): boolean {
		if (!term) return true;
		if (matchesSearch(obj, term)) return true;
		if (obj && typeof obj === 'object') {
			if (Array.isArray(obj)) return obj.some(item => subtreeMatch(item, term));
			return Object.entries(obj as Record<string, unknown>).some(
				([k, v]) => keyMatch(k, term) || subtreeMatch(v, term)
			);
		}
		return false;
	}

	$: expanded = isExpanded($expandedState, path);
	$: term = $searchTerm;
</script>

{#if node === null || node === undefined}
	<span class="jn-null">null</span>
{:else if typeof node === 'string'}
	<span class="jn-string" class:jn-hl={term && matchesSearch(node, term)}>"{node}"</span>
{:else if typeof node === 'number'}
	<span class="jn-number" class:jn-hl={term && matchesSearch(node, term)}>{node}</span>
{:else if typeof node === 'boolean'}
	<span class="jn-bool" class:jn-hl={term && matchesSearch(node, term)}>{node}</span>
{:else if Array.isArray(node)}
	{#if node.length === 0}
		<span class="jn-bkt">[]</span>
	{:else}
		<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
		<span class="jn-toggle" tabindex="0" role="button"
			on:click|stopPropagation={toggle}
			on:keydown|stopPropagation={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); } }}>
			{expanded ? '▾' : '▸'}
		</span>
		{#if expanded}
			<span class="jn-bkt">[</span><span class="jn-meta">{node.length}</span>
			<div class="jn-children">
				{#each node as item, i}
					{#if !term || subtreeMatch(item, term)}
						<div class="jn-row">
							<span class="jn-idx">{i}: </span>
							<svelte:self node={item} path="{path}[{i}]" depth={depth + 1} />
						</div>
					{/if}
				{/each}
			</div>
			<span class="jn-bkt">]</span>
		{:else}
			<span class="jn-bkt">[</span><span class="jn-collapsed">{node.length} items</span><span class="jn-bkt">]</span>
		{/if}
	{/if}
{:else if typeof node === 'object'}
	{@const entries = Object.entries(node)}
	{#if entries.length === 0}
		<span class="jn-bkt">{'{}'}</span>
	{:else}
		<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
		<span class="jn-toggle" tabindex="0" role="button"
			on:click|stopPropagation={toggle}
			on:keydown|stopPropagation={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); } }}>
			{expanded ? '▾' : '▸'}
		</span>
		{#if expanded}
			<span class="jn-bkt">{'{'}</span><span class="jn-meta">{entries.length}</span>
			<div class="jn-children">
				{#each entries as [key, val]}
					{#if !term || keyMatch(key, term) || subtreeMatch(val, term)}
						<div class="jn-row">
							<span class="jn-key" class:jn-hl={term && keyMatch(key, term)}>"{key}"</span><span class="jn-colon">: </span>
							<svelte:self node={val} path={path ? `${path}.${key}` : key} depth={depth + 1} />
						</div>
					{/if}
				{/each}
			</div>
			<span class="jn-bkt">{'}'}</span>
		{:else}
			<span class="jn-bkt">{'{'}</span><span class="jn-collapsed">{entries.length} keys</span><span class="jn-bkt">{'}'}</span>
		{/if}
	{/if}
{/if}

<style>
	.jn-children {
		padding-left: 1.25rem;
		border-left: 1px solid var(--border-soft, #e5e7eb);
		margin-left: 2px;
	}
	.jn-row { padding: 1px 0; }
	.jn-toggle {
		cursor: pointer;
		color: var(--text-muted, #9ca3af);
		user-select: none;
		font-size: 0.7rem;
		display: inline-block;
		width: 1rem;
		text-align: center;
		border-radius: var(--radius-sm, 2px);
	}
	.jn-toggle:hover { color: var(--text-primary, #111); background: var(--bg-soft, #f3f4f6); }
	.jn-toggle:focus-visible { outline: 2px solid var(--accent-primary, #2563eb); outline-offset: -1px; }
	.jn-key { color: var(--accent-primary, #2563eb); }
	.jn-colon { color: var(--text-muted, #9ca3af); }
	.jn-string { color: #16a34a; word-break: break-all; }
	.jn-number { color: #d97706; }
	.jn-bool { color: #9333ea; }
	.jn-null { color: var(--text-muted, #9ca3af); font-style: italic; }
	.jn-bkt { color: var(--text-muted, #9ca3af); }
	.jn-meta {
		color: var(--text-muted, #9ca3af);
		font-size: 0.6875rem;
		font-style: italic;
		margin-left: 4px;
	}
	.jn-collapsed {
		color: var(--text-muted, #9ca3af);
		font-size: 0.6875rem;
		font-style: italic;
		cursor: pointer;
	}
	.jn-idx { color: var(--text-muted, #9ca3af); margin-right: 2px; }
	.jn-hl {
		background: color-mix(in srgb, #facc15 40%, transparent);
		border-radius: 2px;
		padding: 0 2px;
	}
</style>
