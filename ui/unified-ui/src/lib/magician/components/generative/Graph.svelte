<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	/**
	 * Graph Component — MUIJ graph family (plan 1.5)
	 *
	 * Declarative node/edge surface with deterministic layouts only
	 * (layered tiers / radial ring / vertical list). No physics.
	 *
	 * Hostile-input bounds mirror the Rust validator caps: at most 200
	 * nodes and 400 edges are admitted; malformed members are skipped;
	 * edges that reference undeclared nodes are dropped. Node ids (and
	 * every id reference — edges, focus, reveal) compare exactly like the
	 * Rust validator, with no trimming; text-length caps count Unicode
	 * code points, matching Rust's `chars().count()`.
	 */

	interface GraphNode {
		id: string;
		label: string;
		kind?: string;
		metadata: Array<[string, string]>;
	}

	interface GraphEdge {
		from: string;
		to: string;
		label?: string;
	}

	type GraphLayout = 'layered' | 'radial' | 'list';

	interface PositionedNode extends GraphNode {
		x: number;
		y: number;
		tier: number;
		revealIndex: number;
	}

	export let nodes: unknown = [];
	export let edges: unknown = [];
	export let layout: unknown = 'layered';
	export let focusNodeId: unknown = '';
	export let revealOrder: unknown = [];

	const dispatch = createEventDispatcher<{ select: { nodeId: string } }>();

	const MAX_NODES = 200;
	const MAX_EDGES = 400;
	const MAX_NODE_ID_CHARS = 128;
	const MAX_TEXT_CHARS = 200;
	const MAX_METADATA_KEY_CHARS = 64;

	/** Rust counts `chars()` (Unicode code points); JS `String.length`
	 * counts UTF-16 units, so every length cap is checked through code
	 * points to keep the renderers and the validator in agreement. */
	function codePointLength(text: string): number {
		return [...text].length;
	}

	const NODE_W = 96;
	const NODE_H = 30;
	const TIER_H = 64;
	const SVG_W = 360;

	function isRecord(value: unknown): value is Record<string, unknown> {
		return typeof value === 'object' && value !== null && !Array.isArray(value);
	}

	function boundedText(value: unknown): string {
		const text = typeof value === 'string' ? value.trim() : '';
		if (codePointLength(text) > MAX_TEXT_CHARS) {
			return [...text].slice(0, MAX_TEXT_CHARS).join('');
		}
		return text;
	}

	function normalizeNodes(value: unknown): GraphNode[] {
		if (!Array.isArray(value)) return [];
		const seen = new Set<string>();
		const result: GraphNode[] = [];
		for (const item of value) {
			if (result.length >= MAX_NODES) break;
			if (!isRecord(item)) continue;
			// Ids compare exactly like the Rust validator: no trim, and the
			// 128-code-point cap drops over-long ids like every other cap
			// violation.
			const id = typeof item.id === 'string' ? item.id : '';
			if (!id || codePointLength(id) > MAX_NODE_ID_CHARS || seen.has(id)) continue;
			seen.add(id);
			const kind = boundedText(item.kind);
			const metadata: Array<[string, string]> = [];
			if (isRecord(item.metadata)) {
				for (const [key, raw] of Object.entries(item.metadata)) {
					if (
						codePointLength(key) > MAX_METADATA_KEY_CHARS ||
						metadata.length >= 8
					) {
						continue;
					}
					const scalar =
						typeof raw === 'string'
							? raw
							: typeof raw === 'number' || typeof raw === 'boolean'
								? String(raw)
								: '';
					if (codePointLength(scalar) > MAX_TEXT_CHARS) continue;
					metadata.push([key, scalar]);
				}
			}
			result.push({
				id,
				label: boundedText(item.label) || id,
				...(kind ? { kind } : {}),
				metadata
			});
		}
		return result;
	}

	function normalizeEdges(value: unknown, nodeIds: Set<string>): GraphEdge[] {
		if (!Array.isArray(value)) return [];
		const result: GraphEdge[] = [];
		for (const item of value) {
			if (result.length >= MAX_EDGES) break;
			if (!isRecord(item)) continue;
			// Endpoints compare exactly like the Rust validator (no trim),
			// so a valid document's edges always resolve.
			const from = typeof item.from === 'string' ? item.from : '';
			const to = typeof item.to === 'string' ? item.to : '';
			if (!from || !to || !nodeIds.has(from) || !nodeIds.has(to)) continue;
			const label = boundedText(item.label);
			result.push({ from, to, ...(label ? { label } : {}) });
		}
		return result;
	}

	function normalizeLayout(value: unknown): GraphLayout {
		const normalized = typeof value === 'string' ? value.trim().toLowerCase() : '';
		if (normalized === 'radial' || normalized === 'list') return normalized;
		return 'layered';
	}

	function normalizeRevealOrder(value: unknown, nodeIds: Set<string>): Map<string, number> {
		const order = new Map<string, number>();
		if (!Array.isArray(value)) return order;
		for (const item of value) {
			// Reveal ids compare exactly against declared node ids.
			const id = typeof item === 'string' ? item : '';
			if (!id || !nodeIds.has(id) || order.has(id)) continue;
			if (order.size >= MAX_NODES) break;
			order.set(id, order.size);
		}
		return order;
	}

	/**
	 * Deterministic topological tiers (Kahn): tier 0 holds nodes without
	 * incoming edges from unplaced nodes. Cycle members all land in the
	 * final tier, preserving declaration order within every tier.
	 */
	function tierIndexes(normalizedNodes: GraphNode[], normalizedEdges: GraphEdge[]): number[] {
		const position = new Map(normalizedNodes.map((node, index) => [node.id, index]));
		const incoming = normalizedNodes.map(() => 0);
		const outgoing: number[][] = normalizedNodes.map(() => []);
		for (const edge of normalizedEdges) {
			const from = position.get(edge.from);
			const to = position.get(edge.to);
			if (from === undefined || to === undefined || from === to) continue;
			incoming[to] += 1;
			outgoing[from].push(to);
		}
		const tiers = normalizedNodes.map(() => 0);
		let frontier = incoming
			.map((count, index) => (count === 0 ? index : -1))
			.filter((index) => index >= 0);
		let tier = 0;
		const placed = new Set<number>();
		while (frontier.length > 0) {
			for (const index of frontier) {
				tiers[index] = tier;
				placed.add(index);
			}
			const next: number[] = [];
			for (const index of frontier) {
				for (const target of outgoing[index]) {
					incoming[target] -= 1;
					if (incoming[target] === 0 && !placed.has(target) && !next.includes(target)) {
						next.push(target);
					}
				}
			}
			frontier = next.sort((left, right) => left - right);
			tier += 1;
		}
		// Cycle members: one shared final tier after every acyclic tier.
		const cycleTier = tier;
		for (let index = 0; index < tiers.length; index += 1) {
			if (!placed.has(index)) tiers[index] = cycleTier;
		}
		return tiers;
	}

	function positioned(
		normalizedNodes: GraphNode[],
		normalizedEdges: GraphEdge[],
		graphLayout: GraphLayout,
		reveal: Map<string, number>
	): PositionedNode[] {
		const tiers = tierIndexes(normalizedNodes, normalizedEdges);
		const tierCount = Math.max(...tiers, 0) + 1;
		const byTier = new Map<number, number>();
		for (const tier of tiers) byTier.set(tier, (byTier.get(tier) ?? 0) + 1);
		const tierSeen = new Map<number, number>();
		return normalizedNodes.map((node, index) => {
			const tier = tiers[index];
			const within = tierSeen.get(tier) ?? 0;
			tierSeen.set(tier, within + 1);
			const width = byTier.get(tier) ?? 1;
			let x: number;
			let y: number;
			if (graphLayout === 'radial') {
				const count = normalizedNodes.length;
				const angle = (2 * Math.PI * index) / Math.max(count, 1) - Math.PI / 2;
				const radiusX = SVG_W / 2 - NODE_W / 2 - 4;
				const radiusY = (tierCount * TIER_H) / 2 - NODE_H;
				x = SVG_W / 2 + radiusX * Math.cos(angle);
				y = (tierCount * TIER_H) / 2 + Math.max(radiusY, 8) * Math.sin(angle);
			} else if (graphLayout === 'list') {
				x = SVG_W / 2;
				y = (index + 0.5) * TIER_H;
			} else {
				x = ((within + 0.5) / width) * SVG_W;
				y = (tier + 0.5) * TIER_H;
			}
			return {
				...node,
				x,
				y,
				tier,
				revealIndex: reveal.has(node.id) ? reveal.get(node.id) as number : -1
			};
		});
	}

	function nodeLabel(node: PositionedNode): string {
		// The chip truncates by code points like every cap above, so a label
		// is never cut mid-surrogate-pair.
		return [...node.label].length > 14
			? `${[...node.label].slice(0, 13).join('')}…`
			: node.label;
	}

	function truncate(value: string, max: number): string {
		return value.length > max ? `${value.slice(0, max - 1)}…` : value;
	}

	function adjacencySummary(nodeId: string): string {
		const targets = safeEdges
			.filter((edge) => edge.from === nodeId)
			.map((edge) => idToLabel.get(edge.to) ?? edge.to);
		if (targets.length === 0) return '—';
		return truncate(targets.join(', '), 48);
	}

	$: safeLayout = normalizeLayout(layout);
	$: normalized = normalizeNodes(nodes);
	$: nodeIds = new Set(normalized.map((node) => node.id));
	$: safeEdges = normalizeEdges(edges, nodeIds);
	$: revealOrderMap = normalizeRevealOrder(revealOrder, nodeIds);
	$: safeNodes = positioned(normalized, safeEdges, safeLayout, revealOrderMap);
	$: idToLabel = new Map(normalized.map((node) => [node.id, node.label]));
	$: svgHeight =
		safeLayout === 'radial'
			? Math.max(240, Math.max(...safeNodes.map((node) => node.tier), 0) * TIER_H + TIER_H)
			: safeLayout === 'list'
				? Math.max(80, normalized.length * TIER_H)
				: Math.max(160, (Math.max(...safeNodes.map((node) => node.tier), 0) + 1) * TIER_H);

	// Reset selection when the graph data changes to avoid stale detail state.
	let prevNodesRef: unknown = nodes;
	let selectedNodeId = '';
	$: if (nodes !== prevNodesRef) {
		prevNodesRef = nodes;
		selectedNodeId = normalizeFocus(focusNodeId, nodeIds);
	}
	$: initialFocus = normalizeFocus(focusNodeId, nodeIds);
	// Apply focus once per data identity (initial selection).
	let prevFocusKey = '';
	$: if (initialFocus !== prevFocusKey && initialFocus) {
		prevFocusKey = initialFocus;
		selectedNodeId = initialFocus;
	}

	function normalizeFocus(value: unknown, ids: Set<string>): string {
		// Focus ids compare exactly against declared node ids.
		const id = typeof value === 'string' ? value : '';
		return id && ids.has(id) ? id : '';
	}

	function selectNode(nodeId: string): void {
		selectedNodeId = nodeId;
		dispatch('select', { nodeId });
	}

	$: selectedNode = safeNodes.find((node) => node.id === selectedNodeId);
	$: incomingEdges = selectedNode
		? safeEdges.filter((edge) => edge.to === selectedNode.id)
		: [];
	$: outgoingEdges = selectedNode
		? safeEdges.filter((edge) => edge.from === selectedNode.id)
		: [];
</script>

{#if safeNodes.length > 0}
	<div class="muij-graph" role="group" aria-label="Graph">
		{#if safeLayout === 'list'}
			<ul class="muij-graph-list" role="list">
				{#each safeNodes as node (node.id)}
					<li
						class="muij-graph-list-row"
						class:muij-graph-list-row-selected={node.id === selectedNodeId}
						data-node-id={node.id}
					>
						<button
							type="button"
							class="muij-graph-list-button"
							class:muij-graph-button-reveal={node.revealIndex >= 0}
							on:click={() => selectNode(node.id)}
							style={node.revealIndex >= 0
								? `animation-delay: ${node.revealIndex * 120}ms`
								: ''}
						>
							{#if node.kind}<span class="muij-graph-kind">{node.kind}</span>{/if}
							<span class="muij-graph-list-label">{node.label}</span>
							<span class="muij-graph-list-edges" aria-label="outgoing edges">
								→ {adjacencySummary(node.id)}
							</span>
						</button>
					</li>
				{/each}
			</ul>
		{:else}
			<svg
				class="muij-graph-svg"
				viewBox="0 0 {SVG_W} {svgHeight}"
				role="img"
				aria-label="Graph diagram"
				preserveAspectRatio="xMidYMid meet"
			>
				{#each safeEdges as edge, index (index)}
					{@const from = safeNodes.find((node) => node.id === edge.from)}
					{@const to = safeNodes.find((node) => node.id === edge.to)}
					{#if from && to}
						<line
							class="muij-graph-edge"
							x1={from.x}
							y1={from.y}
							x2={to.x}
							y2={to.y}
							aria-hidden="true"
						/>
						<circle class="muij-graph-edge-head" cx={to.x} cy={to.y - NODE_H / 2} r="2.5" />
					{/if}
				{/each}
				{#each safeNodes as node (node.id)}
					<g
						class="muij-graph-node"
						class:muij-graph-node-selected={node.id === selectedNodeId}
						class:muij-graph-node-reveal={node.revealIndex >= 0}
						data-node-id={node.id}
						data-tier={node.tier}
						style={node.revealIndex >= 0
							? `animation-delay: ${node.revealIndex * 120}ms`
							: ''}
						role="button"
						tabindex="0"
						aria-label="{node.label}{node.kind ? `, ${node.kind}` : ''}"
						on:click={() => selectNode(node.id)}
						on:keydown={(event) => {
							if (event.key === 'Enter' || event.key === ' ') {
								event.preventDefault();
								selectNode(node.id);
							}
						}}
					>
						<rect
							x={node.x - NODE_W / 2}
							y={node.y - NODE_H / 2}
							width={NODE_W}
							height={NODE_H}
							rx="6"
						/>
						<text x={node.x} y={node.y + 4} text-anchor="middle">
							{nodeLabel(node)}
						</text>
					</g>
				{/each}
			</svg>
		{/if}

		{#if selectedNode}
			<div class="muij-graph-detail" data-testid="muij-graph-detail">
				<div class="muij-graph-detail-title">
					{#if selectedNode.kind}<span class="muij-graph-kind">{selectedNode.kind}</span>{/if}
					<span class="muij-graph-detail-label">{selectedNode.label}</span>
				</div>
				{#if selectedNode.metadata.length > 0}
					<dl class="muij-graph-detail-metadata">
						{#each selectedNode.metadata as [key, value] (key)}
							<div class="muij-graph-detail-row">
								<dt>{key}</dt>
								<dd>{value}</dd>
							</div>
						{/each}
					</dl>
				{/if}
				<div class="muij-graph-detail-edges">
					<span>→ {outgoingEdges.length}</span>
					<span>← {incomingEdges.length}</span>
				</div>
			</div>
		{/if}
	</div>
{:else}
	<div class="muij-graph-empty">
		<span class="muij-graph-empty-text">No items</span>
	</div>
{/if}

<style>
	.muij-graph {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		font-family: var(--font-primary);
	}

	.muij-graph-svg {
		width: 100%;
		height: auto;
		display: block;
	}

	.muij-graph-edge {
		stroke: var(--border-soft, #c9c4d8);
		stroke-width: 1.5;
	}

	.muij-graph-edge-head {
		fill: var(--border-soft, #c9c4d8);
	}

	.muij-graph-node rect {
		fill: var(--bg-soft, #f3f1f8);
		stroke: var(--border-soft, #c9c4d8);
		stroke-width: 1;
		cursor: pointer;
	}

	.muij-graph-node text {
		font-size: 11px;
		fill: var(--text-primary, #1f1b2e);
		pointer-events: none;
	}

	.muij-graph-node:hover rect {
		stroke: var(--accent-primary, #6d5efc);
	}

	.muij-graph-node-selected rect {
		stroke: var(--accent-primary, #6d5efc);
		stroke-width: 2;
	}

	.muij-graph-node:focus-visible {
		outline: none;
	}

	.muij-graph-node:focus-visible rect {
		stroke: var(--accent-primary, #6d5efc);
		stroke-width: 2;
	}

	.muij-graph-node-reveal,
	.muij-graph-button-reveal {
		animation: muij-graph-fade-in 240ms ease-out both;
	}

	@keyframes muij-graph-fade-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.muij-graph-node-reveal,
		.muij-graph-button-reveal {
			animation: none;
		}
	}

	.muij-graph-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	.muij-graph-list-row-selected .muij-graph-list-button {
		background: color-mix(in srgb, var(--accent, #6d5efc) 12%, transparent);
		border-color: var(--accent-primary, #6d5efc);
	}

	.muij-graph-list-button {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		width: 100%;
		padding: 4px 8px;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: transparent;
		color: var(--text-primary);
		font: inherit;
		text-align: left;
		cursor: pointer;
	}

	.muij-graph-list-button:hover {
		background: var(--bg-soft);
	}

	.muij-graph-list-label {
		font-size: 0.8125rem;
	}

	.muij-graph-list-edges {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		color: var(--text-muted);
		max-width: 55%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-graph-kind {
		flex-shrink: 0;
		font-family: var(--font-mono);
		font-size: 0.625rem;
		color: var(--accent-primary);
		background: color-mix(in srgb, var(--accent-primary, #6d5efc) 12%, transparent);
		padding: 1px 5px;
		border-radius: var(--radius-sm);
	}

	.muij-graph-detail {
		padding: 8px 10px;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.muij-graph-detail-title {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.muij-graph-detail-label {
		font-size: 0.8125rem;
		color: var(--text-primary);
	}

	.muij-graph-detail-metadata {
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.muij-graph-detail-row {
		display: flex;
		gap: 8px;
		font-size: 0.75rem;
	}

	.muij-graph-detail-row dt {
		color: var(--text-muted);
		min-width: 5rem;
	}

	.muij-graph-detail-row dd {
		margin: 0;
		color: var(--text-primary);
	}

	.muij-graph-detail-edges {
		display: flex;
		gap: 12px;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		color: var(--text-muted);
	}

	.muij-graph-empty {
		padding: var(--space-md);
		text-align: center;
		color: var(--text-muted);
		font-size: 0.75rem;
	}
</style>
