<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	/**
	 * Tree Component — GAUI-ε (Phase 5 sub-goal tree visualization)
	 *
	 * Hierarchical tree with expand/collapse, status indicators,
	 * and keyboard navigation. Designed for sub-goal tree display (P5-10).
	 *
	 * Depth limit: renders up to 3 levels (root → child → grandchild).
	 * This matches the P5-04 sub-goal depth cap of 1 (parent + sub-goal + leaf).
	 */

	interface TreeNode {
		id: string;
		label: string;
		children?: TreeNode[];
		status?: string;
		icon?: string;
	}

	export let nodes: unknown = [];
	export let expandAll: unknown = true;
	export let maxDepth: unknown = 3;
	export let selectable: unknown = false;

	const dispatch = createEventDispatcher<{ select: { nodeId: string } }>();

	const ABSOLUTE_MAX_DEPTH = 3;
	const MAX_NORMALIZED_NODES = 500;
	const MAX_INSPECTED_NODES = 1_000;

	function isTreeNode(item: unknown): item is TreeNode {
		return (
			typeof item === 'object' &&
			item !== null &&
			typeof (item as Record<string, unknown>).id === 'string' &&
			typeof (item as Record<string, unknown>).label === 'string'
		);
	}

	function normalizeMaxDepth(value: unknown): number {
		if (typeof value !== 'number' || !Number.isFinite(value)) return ABSOLUTE_MAX_DEPTH;
		return Math.max(1, Math.min(ABSOLUTE_MAX_DEPTH, Math.floor(value)));
	}

	function normalizeNodes(value: unknown, depthLimit: number): TreeNode[] {
		if (!Array.isArray(value)) return [];
		const roots: TreeNode[] = [];
		const seenObjects = new WeakSet<object>();
		const seenIds = new Set<string>();
		let admitted = 0;
		let inspected = 0;
		const stack: Array<{ source: unknown[]; target: TreeNode[]; depth: number }> = [
			{ source: value, target: roots, depth: 1 }
		];

		while (
			stack.length > 0 &&
			admitted < MAX_NORMALIZED_NODES &&
			inspected < MAX_INSPECTED_NODES
		) {
			const frame = stack.pop();
			if (!frame) break;
			for (const item of frame.source) {
				if (admitted >= MAX_NORMALIZED_NODES || inspected >= MAX_INSPECTED_NODES) break;
				inspected += 1;
				if (!isTreeNode(item)) continue;
				const object = item as object;
				if (seenObjects.has(object) || seenIds.has(item.id)) continue;
				seenObjects.add(object);
				seenIds.add(item.id);
				admitted += 1;

				const childTarget =
					frame.depth < depthLimit && Array.isArray(item.children)
						? ([] as TreeNode[])
						: undefined;
				frame.target.push({
					id: item.id,
					label: item.label,
					status: typeof item.status === 'string' ? item.status : undefined,
					icon: typeof item.icon === 'string' ? item.icon : undefined,
					children: childTarget
				});
				if (childTarget && item.children) {
					stack.push({ source: item.children, target: childTarget, depth: frame.depth + 1 });
				}
			}
		}
		return roots;
	}

	function toBool(value: unknown, fallback: boolean): boolean {
		if (typeof value === 'boolean') return value;
		return fallback;
	}

	function statusIcon(status: string | undefined): string {
		switch (status) {
			case 'running':
			case 'active':
			case 'in_progress':
				return '~';
			case 'completed':
			case 'success':
				return '+';
			case 'failed':
			case 'error':
				return '!';
			case 'timed_out':
			case 'timeout':
				return 'x';
			case 'pending':
			case 'waiting':
				return '-';
			default:
				return '';
		}
	}

	function statusClass(status: string | undefined): string {
		switch (status) {
			case 'running':
			case 'active':
			case 'in_progress':
				return 'muij-tree-status-running';
			case 'completed':
			case 'success':
				return 'muij-tree-status-completed';
			case 'failed':
			case 'error':
				return 'muij-tree-status-failed';
			case 'timed_out':
			case 'timeout':
				return 'muij-tree-status-timeout';
			case 'pending':
			case 'waiting':
				return 'muij-tree-status-pending';
			default:
				return '';
		}
	}

	$: safeMaxDepth = normalizeMaxDepth(maxDepth);
	$: safeNodes = normalizeNodes(nodes, safeMaxDepth);
	$: defaultExpanded = toBool(expandAll, true);
	$: selectionEnabled = toBool(selectable, false);

	// Reset expand state when the tree data changes to avoid stale entries.
	let prevNodesRef: unknown = nodes;
	let expandedState: Record<string, boolean> = {};
	let selectedNodeId = '';
	$: if (nodes !== prevNodesRef) {
		prevNodesRef = nodes;
		expandedState = {};
		selectedNodeId = '';
	}

	function isExpanded(nodeId: string): boolean {
		if (nodeId in expandedState) return expandedState[nodeId];
		return defaultExpanded;
	}

	function toggleExpand(nodeId: string) {
		const current = isExpanded(nodeId);
		expandedState = { ...expandedState, [nodeId]: !current };
	}

	function selectNode(nodeId: string): void {
		if (!selectionEnabled) return;
		selectedNodeId = nodeId;
		dispatch('select', { nodeId });
	}

	function activateNode(nodeId: string, hasChildren: boolean): void {
		if (hasChildren) toggleExpand(nodeId);
		selectNode(nodeId);
	}

	function handleKeydown(event: KeyboardEvent, nodeId: string, hasChildren: boolean) {
		if (event.key === 'Enter' || event.key === ' ') {
			if (hasChildren || selectionEnabled) {
				event.preventDefault();
				activateNode(nodeId, hasChildren);
			}
		} else if (event.key === 'ArrowRight') {
			if (hasChildren && !isExpanded(nodeId)) {
				event.preventDefault();
				expandedState = { ...expandedState, [nodeId]: true };
			}
		} else if (event.key === 'ArrowLeft') {
			if (hasChildren && isExpanded(nodeId)) {
				event.preventDefault();
				expandedState = { ...expandedState, [nodeId]: false };
			}
		}
	}
</script>

{#if safeNodes.length > 0}
	<ul class="muij-tree" role="tree" aria-label="Tree">
		{#each safeNodes as node (node.id)}
			<li
				class="muij-tree-item"
				role="treeitem"
				aria-selected={selectionEnabled ? selectedNodeId === node.id : false}
				aria-expanded={node.children?.length ? isExpanded(node.id) : undefined}
			>
				<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
				<div
					class="muij-tree-row"
					class:muij-tree-row-expandable={!!node.children?.length}
					class:muij-tree-row-selectable={selectionEnabled}
					class:muij-tree-row-selected={selectionEnabled && selectedNodeId === node.id}
					on:click={() => activateNode(node.id, !!node.children?.length)}
					on:keydown={(e) => handleKeydown(e, node.id, !!node.children?.length)}
					tabindex={node.children?.length || selectionEnabled ? 0 : -1}
					role={node.children?.length || selectionEnabled ? 'button' : undefined}
				>
					{#if node.children?.length}
						<span class="muij-tree-chevron" aria-hidden="true">
							{isExpanded(node.id) ? '\u25BE' : '\u25B8'}
						</span>
					{:else}
						<span class="muij-tree-spacer" aria-hidden="true"></span>
					{/if}
					{#if node.status}
						<span class="muij-tree-status {statusClass(node.status)}" aria-label={node.status}>
							{statusIcon(node.status)}
						</span>
					{/if}
					{#if node.icon}
						<span class="muij-tree-icon" aria-hidden="true">{node.icon}</span>
					{/if}
					<span class="muij-tree-label">{node.label}</span>
				</div>
				{#if node.children?.length && isExpanded(node.id)}
					<ul class="muij-tree-children" role="group">
						{#each node.children as child (child.id)}
							<li
								class="muij-tree-item"
								role="treeitem"
								aria-selected={selectionEnabled ? selectedNodeId === child.id : false}
								aria-expanded={child.children?.length ? isExpanded(child.id) : undefined}
							>
								<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
								<div
									class="muij-tree-row"
									class:muij-tree-row-expandable={!!child.children?.length}
									class:muij-tree-row-selectable={selectionEnabled}
									class:muij-tree-row-selected={selectionEnabled && selectedNodeId === child.id}
									on:click={() => activateNode(child.id, !!child.children?.length)}
									on:keydown={(e) => handleKeydown(e, child.id, !!child.children?.length)}
									tabindex={child.children?.length || selectionEnabled ? 0 : -1}
									role={child.children?.length || selectionEnabled ? 'button' : undefined}
								>
									{#if child.children?.length}
										<span class="muij-tree-chevron" aria-hidden="true">
											{isExpanded(child.id) ? '\u25BE' : '\u25B8'}
										</span>
									{:else}
										<span class="muij-tree-spacer" aria-hidden="true"></span>
									{/if}
									{#if child.status}
										<span class="muij-tree-status {statusClass(child.status)}" aria-label={child.status}>
											{statusIcon(child.status)}
										</span>
									{/if}
									{#if child.icon}
										<span class="muij-tree-icon" aria-hidden="true">{child.icon}</span>
									{/if}
									<span class="muij-tree-label">{child.label}</span>
								</div>
								{#if child.children?.length && isExpanded(child.id)}
									<ul class="muij-tree-children" role="group">
							{#each child.children as grandchild (grandchild.id)}
								<li class="muij-tree-item" role="treeitem" aria-selected={selectionEnabled ? selectedNodeId === grandchild.id : false}>
									<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
									<div
													class="muij-tree-row"
													class:muij-tree-row-selectable={selectionEnabled}
													class:muij-tree-row-selected={selectionEnabled && selectedNodeId === grandchild.id}
													on:click={() => selectNode(grandchild.id)}
													on:keydown={(event) => handleKeydown(event, grandchild.id, false)}
													tabindex={selectionEnabled ? 0 : -1}
													role={selectionEnabled ? 'button' : undefined}
												>
													<span class="muij-tree-spacer" aria-hidden="true"></span>
													{#if grandchild.status}
														<span class="muij-tree-status {statusClass(grandchild.status)}" aria-label={grandchild.status}>
															{statusIcon(grandchild.status)}
														</span>
													{/if}
													{#if grandchild.icon}
														<span class="muij-tree-icon" aria-hidden="true">{grandchild.icon}</span>
													{/if}
													<span class="muij-tree-label">{grandchild.label}</span>
												</div>
											</li>
										{/each}
									</ul>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			</li>
		{/each}
	</ul>
{:else}
	<div class="muij-tree-empty">
		<span class="muij-tree-empty-text">No items</span>
	</div>
{/if}

<style>
	.muij-tree {
		list-style: none;
		margin: 0;
		padding: 0;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.muij-tree-children {
		list-style: none;
		margin: 0;
		padding-left: 1.25rem;
	}

	.muij-tree-row-selected {
		background: color-mix(in srgb, var(--accent, #6d5efc) 12%, transparent);
	}

	.muij-tree-item {
		margin: 0;
	}

	.muij-tree-row {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		padding: 3px 4px;
		border-radius: var(--radius-sm);
		cursor: default;
		user-select: none;
	}

	.muij-tree-row-expandable,
	.muij-tree-row-selectable {
		cursor: pointer;
	}

	.muij-tree-row-expandable:hover,
	.muij-tree-row-selectable:hover {
		background: var(--bg-soft);
	}

	.muij-tree-row:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -1px;
	}

	.muij-tree-chevron {
		flex-shrink: 0;
		width: 1rem;
		text-align: center;
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.muij-tree-spacer {
		flex-shrink: 0;
		width: 1rem;
	}

	.muij-tree-status {
		flex-shrink: 0;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.125rem;
		height: 1.125rem;
		border-radius: 50%;
		font-size: 0.625rem;
		font-weight: 600;
		font-family: var(--font-mono);
	}

	.muij-tree-status-running {
		background: color-mix(in srgb, var(--accent-primary) 20%, transparent);
		color: var(--accent-primary);
	}

	.muij-tree-status-completed {
		background: color-mix(in srgb, #22c55e 20%, transparent);
		color: #16a34a;
	}

	.muij-tree-status-failed {
		background: color-mix(in srgb, #ef4444 20%, transparent);
		color: #dc2626;
	}

	.muij-tree-status-timeout {
		background: color-mix(in srgb, #f59e0b 20%, transparent);
		color: #d97706;
	}

	.muij-tree-status-pending {
		background: color-mix(in srgb, var(--text-muted) 15%, transparent);
		color: var(--text-muted);
	}

	.muij-tree-icon {
		flex-shrink: 0;
		font-size: 0.875rem;
	}

	.muij-tree-label {
		color: var(--text-primary);
		line-height: 1.4;
	}

	.muij-tree-empty {
		padding: var(--space-md);
		text-align: center;
		color: var(--text-muted);
		font-size: 0.75rem;
	}
</style>
