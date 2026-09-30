<script lang="ts">
	import { setContext } from 'svelte';
	import { writable } from 'svelte/store';
	import ScrollArea from './generative/ScrollArea.svelte';
	import JsonNode from './JsonNode.svelte';

	export let data: unknown = null;
	export let maxHeight: string = '60vh';

	// Stores shared with JsonNode via context
	const expandedState = writable<Record<string, boolean>>({ '': true });
	const searchTermStore = writable('');

	setContext('jsonExpandedState', expandedState);
	setContext('jsonSearchTerm', searchTermStore);

	// Local binding for the search input
	let searchInput = '';
	$: $searchTermStore = searchInput;

	function collectPaths(obj: unknown, prefix = ''): string[] {
		const paths: string[] = [];
		if (obj && typeof obj === 'object') {
			paths.push(prefix);
			if (Array.isArray(obj)) {
				obj.forEach((_, i) => paths.push(...collectPaths(obj[i], `${prefix}[${i}]`)));
			} else {
				for (const key of Object.keys(obj as Record<string, unknown>)) {
					paths.push(...collectPaths((obj as Record<string, unknown>)[key], prefix ? `${prefix}.${key}` : key));
				}
			}
		}
		return paths;
	}

	function expandAll() {
		const record: Record<string, boolean> = {};
		for (const p of collectPaths(data)) record[p] = true;
		$expandedState = record;
	}

	function collapseAll() {
		$expandedState = {};
	}

	let copied = false;
	function copyToClipboard() {
		navigator.clipboard.writeText(JSON.stringify(data, null, 2)).then(() => {
			copied = true;
			setTimeout(() => copied = false, 1500);
		});
	}

	// Reset expanded state when data changes
	let prevData: unknown = undefined;
	$: if (data !== prevData) {
		prevData = data;
		$expandedState = { '': true };
	}
</script>

<div class="jv-root">
	<div class="jv-toolbar">
		<input
			type="text"
			class="jv-search"
			placeholder="Search keys or values..."
			bind:value={searchInput}
		/>
		<div class="jv-actions">
			<button class="jv-btn" on:click={expandAll} title="Expand all">
				<svg class="jv-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 10l5 5 5-5"/></svg>
			</button>
			<button class="jv-btn" on:click={collapseAll} title="Collapse all">
				<svg class="jv-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 14l5-5 5 5"/></svg>
			</button>
			<button class="jv-btn" on:click={copyToClipboard} title="Copy JSON">
				{#if copied}
					<svg class="jv-icon" style="color: #16a34a" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>
				{:else}
					<svg class="jv-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
				{/if}
			</button>
		</div>
	</div>
	<ScrollArea {maxHeight} scrollbar="thin" ariaLabel="JSON tree">
		<div class="jv-tree">
			{#if data != null}
				<JsonNode node={data} />
			{:else}
				<span class="jv-empty">null</span>
			{/if}
		</div>
	</ScrollArea>
</div>

<style>
	.jv-root {
		display: flex;
		flex-direction: column;
		font-family: var(--font-mono);
		font-size: 12px;
		line-height: 1.5;
	}
	.jv-toolbar {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 12px;
		border-bottom: 1px solid var(--border-soft, #e5e7eb);
		flex-shrink: 0;
	}
	.jv-search {
		flex: 1;
		padding: 4px 8px;
		font-size: 12px;
		border: 1px solid var(--border-soft, #e5e7eb);
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-base, #fff);
		color: var(--text-primary, #111);
		outline: none;
	}
	.jv-search:focus {
		border-color: var(--accent-primary, #2563eb);
		box-shadow: 0 0 0 1px var(--accent-primary, #2563eb);
	}
	.jv-actions {
		display: flex;
		gap: 2px;
	}
	.jv-btn {
		padding: 4px;
		border-radius: var(--radius-sm, 4px);
		color: var(--text-secondary, #6b7280);
		background: none;
		border: none;
		cursor: pointer;
		display: flex;
		align-items: center;
	}
	.jv-btn:hover {
		background: var(--bg-hover, #f3f4f6);
		color: var(--text-primary, #111);
	}
	.jv-icon {
		width: 14px;
		height: 14px;
	}
	.jv-tree {
		padding: 12px;
	}
	.jv-empty {
		color: var(--text-tertiary, #9ca3af);
		font-style: italic;
	}
</style>
