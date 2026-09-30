<script lang="ts">
	import { onMount } from 'svelte';
	import * as d3 from 'd3';

	export let allNodes: Record<string, any> = {};
	export let rootNodeId: string | null = null;

	let svgContainer: HTMLDivElement;
	let selectedNode: any = null;
	let zoomBehavior: d3.ZoomBehavior<SVGSVGElement, unknown> | null = null;
	let svgElement: d3.Selection<SVGSVGElement, unknown, null, undefined> | null = null;

	interface TreeNode extends d3.HierarchyNode<any> {
		x0?: number;
		y0?: number;
		_children?: TreeNode[];
	}

	onMount(() => {
		console.log('[ExplorationTreeView] onMount called');
		console.log('[ExplorationTreeView] rootNodeId:', rootNodeId);
		console.log('[ExplorationTreeView] allNodes:', allNodes);
		console.log('[ExplorationTreeView] allNodes keys:', Object.keys(allNodes));
		console.log('[ExplorationTreeView] allNodes length:', Object.keys(allNodes).length);

		if (!rootNodeId || Object.keys(allNodes).length === 0) {
			console.warn('[ExplorationTreeView] ❌ No nodes to visualize - rootNodeId:', rootNodeId, 'allNodes length:', Object.keys(allNodes).length);
			return;
		}

		console.log('[ExplorationTreeView] ✅ Calling renderTree()');
		renderTree();
	});

	function renderTree() {
		// Clear previous content
		d3.select(svgContainer).selectAll('*').remove();

		// Calculate dynamic dimensions based on node count
		const nodeCount = Object.keys(allNodes).length;
		const baseWidth = svgContainer.clientWidth || 800;
		const baseHeight = 600;

		// Scale up dimensions for larger trees
		const width = Math.max(baseWidth, Math.min(2400, nodeCount * 100));
		const height = Math.max(baseHeight, Math.min(1800, nodeCount * 80));
		const margin = { top: 20, right: 120, bottom: 20, left: 120 };

		// Create SVG with zoom capability
		const svg = d3
			.select(svgContainer)
			.append<SVGSVGElement>('svg')
			.attr('width', '100%')
			.attr('height', '100%')
			.attr('viewBox', `0 0 ${width} ${height}`)
			.style('cursor', 'grab');

		// Create zoom behavior
		const zoom = d3.zoom<SVGSVGElement, unknown>()
			.scaleExtent([0.5, 3])
			.on('zoom', (event) => {
				g.attr('transform', event.transform);
			});

		// Apply zoom to SVG
		svg.call(zoom);

		// Store references for zoom controls
		svgElement = svg;
		zoomBehavior = zoom;

		// Create main group for content
		const g = svg.append('g')
			.attr('transform', `translate(${margin.left},${margin.top})`);

		// Create hierarchy from flat nodes structure
		const hierarchyData = buildHierarchy(allNodes, rootNodeId!);
		if (!hierarchyData) {
			console.error('Failed to build hierarchy');
			return;
		}

		const root: TreeNode = d3.hierarchy(hierarchyData);

		// Tree layout
		const treeLayout = d3.tree<any>().size([height - margin.top - margin.bottom, width - margin.left - margin.right]);

		// Set initial positions
		root.x0 = (height - margin.top - margin.bottom) / 2;
		root.y0 = 0;

		// Keep all nodes expanded by default for full visibility
		console.log('📊 Rendering tree with all nodes expanded');
		// Don't collapse any nodes - tree starts fully expanded

		update(root);

		function update(source: TreeNode) {
			const treeData = treeLayout(root);
			const nodes = treeData.descendants();
			const links = treeData.links();

			// Ensure we have at least the root node to display
			if (nodes.length === 0) {
				console.error('❌ No nodes to display in tree');
				return;
			}
			console.log(`📊 Rendering ${nodes.length} node(s), ${links.length} link(s)`);

			// Normalize for fixed-depth
			nodes.forEach((d: any) => {
				d.y = d.depth * 180;
			});

			// ********** Nodes **********
			const node = g.selectAll('g.node').data(nodes, (d: any) => d.data.id);

			// Enter new nodes
			const nodeEnter = node
				.enter()
				.append('g')
				.attr('class', 'node')
				.attr('transform', () => `translate(${source.y0},${source.x0})`)
				.on('click', click);

			// Add circles
			nodeEnter
				.append('circle')
				.attr('r', 1e-6)
				.style('fill', (d: any) => getNodeColor(d.data))
				.style('stroke', '#steelblue')
				.style('stroke-width', 2);

			// Add labels
			nodeEnter
				.append('text')
				.attr('dy', '.35em')
				.attr('x', (d: any) => (d.children || d._children ? -13 : 13))
				.attr('text-anchor', (d: any) => (d.children || d._children ? 'end' : 'start'))
				.text((d: any) => truncate(d.data.task, 30))
				.style('fill-opacity', 1e-6)
				.style('font-size', '12px');

			// Update
			const nodeUpdate = nodeEnter.merge(node as any);

			nodeUpdate
				.transition()
				.duration(750)
				.attr('transform', (d: any) => `translate(${d.y},${d.x})`);

			nodeUpdate
				.select('circle')
				.attr('r', 10)
				.style('fill', (d: any) => getNodeColor(d.data))
				.attr('cursor', 'pointer');

			nodeUpdate.select('text').style('fill-opacity', 1);

			// Remove exiting nodes
			const nodeExit = node
				.exit()
				.transition()
				.duration(750)
				.attr('transform', () => `translate(${source.y},${source.x})`)
				.remove();

			nodeExit.select('circle').attr('r', 1e-6);
			nodeExit.select('text').style('fill-opacity', 1e-6);

			// ********** Links **********
			const link = g.selectAll('path.link').data(links, (d: any) => d.target.data.id);

			// Enter new links
			const linkEnter = link
				.enter()
				.insert('path', 'g')
				.attr('class', 'link')
				.attr('d', () => {
					const o = { x: source.x0!, y: source.y0! };
					return diagonal(o, o);
				})
				.style('fill', 'none')
				.style('stroke', '#ccc')
				.style('stroke-width', 2);

			// Update
			const linkUpdate = linkEnter.merge(link as any);

			linkUpdate
				.transition()
				.duration(750)
				.attr('d', (d: any) => diagonal(d.source, d.target));

			// Remove exiting links
			link
				.exit()
				.transition()
				.duration(750)
				.attr('d', () => {
					const o = { x: source.x!, y: source.y! };
					return diagonal(o, o);
				})
				.remove();

			// Store old positions
			nodes.forEach((d: any) => {
				d.x0 = d.x;
				d.y0 = d.y;
			});
		}

		// Toggle children on click
		function click(event: any, d: TreeNode) {
			if (d.children) {
				d._children = d.children;
				d.children = undefined;
			} else {
				d.children = d._children;
				d._children = undefined;
			}
			update(d);
			selectedNode = d.data;
		}

		// Collapse node
		function collapse(d: TreeNode) {
			if (d.children) {
				d._children = d.children;
				d._children.forEach(collapse);
				d.children = undefined;
			}
		}

		// Diagonal path generator
		function diagonal(s: any, d: any) {
			return `M ${s.y} ${s.x}
					C ${(s.y + d.y) / 2} ${s.x},
					  ${(s.y + d.y) / 2} ${d.x},
					  ${d.y} ${d.x}`;
		}
	}

	// Build hierarchical structure from flat nodes
	function buildHierarchy(nodes: Record<string, any>, rootId: string): any | null {
		const nodeById = new Map<string, any>();

		// Create node objects
		Object.entries(nodes).forEach(([id, node]) => {
			nodeById.set(id, {
				id,
				task: node.task || 'Unknown task',
				confidence: node.confidence || 0,
				analysis: node.analysis,
				tool_match: node.tool_match,
				children: []
			});
		});

		// Build parent-child relationships
		Object.entries(nodes).forEach(([id, node]) => {
			if (node.parent_id && nodeById.has(node.parent_id)) {
				const parent = nodeById.get(node.parent_id);
				const child = nodeById.get(id);
				if (parent && child) {
					parent.children.push(child);
				}
			}
		});

		return nodeById.get(rootId) || null;
	}

	// Get node color based on confidence and priority
	function getNodeColor(node: any): string {
		const confidence = node.confidence || 0;
		const priority = node.priority || 1.0;

		// High priority nodes (priority > 0.7) get special colors
		if (priority > 0.7) {
			if (confidence >= 0.7) return '#8b5cf6'; // Purple - high priority, high confidence
			if (confidence >= 0.4) return '#a78bfa'; // Light purple - high priority, medium confidence
			return '#c4b5fd'; // Very light purple - high priority, low confidence
		}

		// Normal priority nodes
		if (confidence >= 0.7) return '#4ade80'; // Green - high confidence
		if (confidence >= 0.4) return '#fbbf24'; // Yellow - medium confidence
		return '#ef4444'; // Red - low confidence
	}

	// Truncate text to max length
	function truncate(text: string, maxLength: number): string {
		return text.length > maxLength ? text.substring(0, maxLength) + '...' : text;
	}

	// Zoom control functions
	function zoomIn() {
		if (svgElement && zoomBehavior) {
			svgElement.transition().duration(300).call(zoomBehavior.scaleBy, 1.3);
		}
	}

	function zoomOut() {
		if (svgElement && zoomBehavior) {
			svgElement.transition().duration(300).call(zoomBehavior.scaleBy, 0.7);
		}
	}

	function resetZoom() {
		if (svgElement && zoomBehavior) {
			svgElement.transition().duration(500).call(
				zoomBehavior.transform,
				d3.zoomIdentity
			);
		}
	}
</script>

<div class="tree-container">
	<div class="tree-visualization-wrapper">
		<!-- Zoom controls -->
		<div class="zoom-controls">
			<button on:click={zoomIn} class="zoom-btn" title="Zoom In">
				<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor">
					<path d="M10 5a1 1 0 011 1v3h3a1 1 0 110 2h-3v3a1 1 0 11-2 0v-3H6a1 1 0 110-2h3V6a1 1 0 011-1z"/>
				</svg>
			</button>
			<button on:click={zoomOut} class="zoom-btn" title="Zoom Out">
				<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor">
					<path d="M6 9a1 1 0 000 2h8a1 1 0 100-2H6z"/>
				</svg>
			</button>
			<button on:click={resetZoom} class="zoom-btn reset-btn" title="Reset View">
				<svg width="20" height="20" viewBox="0 0 20 20" fill="currentColor">
					<path fill-rule="evenodd" d="M4 2a1 1 0 011 1v2.101a7.002 7.002 0 0111.601 2.566 1 1 0 11-1.885.666A5.002 5.002 0 005.999 7H9a1 1 0 010 2H4a1 1 0 01-1-1V3a1 1 0 011-1zm.008 9.057a1 1 0 011.276.61A5.002 5.002 0 0014.001 13H11a1 1 0 110-2h5a1 1 0 011 1v5a1 1 0 11-2 0v-2.101a7.002 7.002 0 01-11.601-2.566 1 1 0 01.61-1.276z" clip-rule="evenodd"/>
				</svg>
			</button>
		</div>

		<!-- Color Legend -->
		<div class="color-legend">
			<div class="legend-title">Node Colors:</div>
			<div class="legend-items">
				<div class="legend-item">
					<div class="legend-color" style="background: #8b5cf6;"></div>
					<span class="legend-label">High Priority</span>
				</div>
				<div class="legend-item">
					<div class="legend-color" style="background: #4ade80;"></div>
					<span class="legend-label">High Confidence</span>
				</div>
				<div class="legend-item">
					<div class="legend-color" style="background: #fbbf24;"></div>
					<span class="legend-label">Medium Confidence</span>
				</div>
				<div class="legend-item">
					<div class="legend-color" style="background: #ef4444;"></div>
					<span class="legend-label">Low Confidence</span>
				</div>
			</div>
		</div>

		<div class="tree-visualization" bind:this={svgContainer}></div>
	</div>

	{#if selectedNode}
		<div class="node-details">
			<h4>📊 Node Details</h4>
			<div class="detail-section">
				<div class="detail-item">
					<span class="label">Task:</span>
					<span class="value">{selectedNode.task}</span>
				</div>
				<div class="detail-item">
					<span class="label">Confidence:</span>
					<span class="value confidence-score">
						{(selectedNode.confidence * 100).toFixed(1)}%
					</span>
				</div>

				{#if selectedNode.depth !== undefined}
					<div class="detail-item">
						<span class="label">Depth:</span>
						<span class="value">{selectedNode.depth}</span>
					</div>
				{/if}

				{#if selectedNode.priority !== undefined}
					<div class="detail-item">
						<span class="label">Priority:</span>
						<span class="value priority-score">
							{selectedNode.priority.toFixed(2)}
						</span>
					</div>
				{/if}

				{#if selectedNode.visits !== undefined}
					<div class="detail-item">
						<span class="label">Visits (GuidedSearch):</span>
						<span class="value">{selectedNode.visits}</span>
					</div>
				{/if}

				{#if selectedNode.analysis}
					<div class="detail-item">
						<span class="label">Complexity:</span>
						<span class="value">{(selectedNode.analysis.complexity.score * 100).toFixed(1)}%</span>
					</div>
					<div class="detail-item">
						<span class="label">Categories:</span>
						<div class="tags">
							{#each selectedNode.analysis.categories.categories || [] as category}
								<span class="tag">{category}</span>
							{/each}
						</div>
					</div>
					{#if selectedNode.analysis.intent}
						<div class="detail-item">
							<span class="label">Intent:</span>
							<span class="value">{selectedNode.analysis.intent}</span>
						</div>
					{/if}
				{/if}

				{#if selectedNode.tool_match?.primary_match}
					<div class="detail-item">
						<span class="label">Tool:</span>
						<span class="value">{selectedNode.tool_match.primary_match.tool_name}</span>
					</div>
				{/if}

				<!-- Parameter Information -->
				{#if selectedNode.extracted_parameters && Object.keys(selectedNode.extracted_parameters).length > 0}
					<div class="detail-item params-section">
						<span class="label">📝 Extracted Parameters:</span>
						<div class="params-list">
							{#each Object.entries(selectedNode.extracted_parameters) as [key, value]}
								<div class="param-item extracted">
									<span class="param-key">{key}:</span>
									<span class="param-value">{value}</span>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if selectedNode.inherited_parameters && Object.keys(selectedNode.inherited_parameters).length > 0}
					<div class="detail-item params-section">
						<span class="label">🔗 Inherited Parameters:</span>
						<div class="params-list">
							{#each Object.entries(selectedNode.inherited_parameters) as [key, value]}
								<div class="param-item inherited">
									<span class="param-key">{key}:</span>
									<span class="param-value">{value}</span>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if selectedNode.available_parameters && Object.keys(selectedNode.available_parameters).length > 0}
					<div class="detail-item params-section">
						<span class="label">✅ Available Parameters:</span>
						<div class="params-list">
							{#each Object.entries(selectedNode.available_parameters) as [key, value]}
								<div class="param-item available">
									<span class="param-key">{key}:</span>
									<span class="param-value">{value}</span>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				<!-- Task Context -->
				{#if selectedNode.task_context}
					<div class="detail-item context-section">
						<span class="label">🌲 Task Hierarchy:</span>
						<div class="context-path">
							{#each selectedNode.task_context.task_path || [] as task, i}
								{#if i > 0}<span class="path-separator">→</span>{/if}
								<span class="path-item" class:current={i === selectedNode.task_context.task_path.length - 1}>
									{task.length > 30 ? task.substring(0, 30) + '...' : task}
								</span>
							{/each}
						</div>
						{#if selectedNode.task_context.parent_context}
							<div class="parent-context">
								<span class="context-label">Context:</span>
								<span class="context-value">{selectedNode.task_context.parent_context}</span>
							</div>
						{/if}
					</div>
				{/if}

				<!-- Dependencies -->
				{#if selectedNode.dependencies && selectedNode.dependencies.length > 0}
					<div class="detail-item">
						<span class="label">🔗 Dependencies:</span>
						<div class="tags">
							{#each selectedNode.dependencies as depId}
								<span class="tag dependency-tag">{depId}</span>
							{/each}
						</div>
					</div>
				{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.tree-container {
		display: grid;
		grid-template-columns: 2fr 300px;
		gap: 1rem;
		height: 600px;
		min-width: 600px; /* Ensure minimum width for tree */
	}

	.tree-visualization-wrapper {
		position: relative;
		width: 100%;
		height: 100%;
	}

	.tree-visualization {
		width: 100%;
		min-width: 400px; /* Minimum width for tree visualization */
		height: 100%;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
		background: white;
		overflow: hidden; /* Changed from auto to hidden - zoom handles overflow */
	}

	.zoom-controls {
		position: absolute;
		top: 1rem;
		right: 1rem;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		z-index: 10;
		background: rgba(255, 255, 255, 0.95);
		border-radius: 8px;
		padding: 0.5rem;
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);
		border: 1px solid #e2e8f0;
	}

	.zoom-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 40px;
		height: 40px;
		background: white;
		border: 1px solid #cbd5e1;
		border-radius: 6px;
		cursor: pointer;
		color: #475569;
		transition: all 0.2s ease;
		padding: 0;
	}

	.zoom-btn:hover {
		background: #f1f5f9;
		border-color: #94a3b8;
		color: #1e293b;
		transform: scale(1.05);
	}

	.zoom-btn:active {
		transform: scale(0.95);
	}

	.zoom-btn.reset-btn {
		border-top: 1px solid #e2e8f0;
		margin-top: 0.25rem;
	}

	.zoom-btn svg {
		width: 20px;
		height: 20px;
	}

	/* Color Legend */
	.color-legend {
		position: absolute;
		top: 1rem;
		left: 1rem;
		background: rgba(255, 255, 255, 0.95);
		border-radius: 8px;
		padding: 0.75rem;
		box-shadow: 0 2px 8px rgba(0, 0, 0, 0.1);
		border: 1px solid #e2e8f0;
		z-index: 10;
	}

	.legend-title {
		font-size: 0.75rem;
		font-weight: 600;
		color: #475569;
		margin-bottom: 0.5rem;
	}

	.legend-items {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.legend-item {
		display: flex;
		align-items: center;
		gap: 0.5rem;
	}

	.legend-color {
		width: 12px;
		height: 12px;
		border-radius: 50%;
		border: 1px solid rgba(0, 0, 0, 0.1);
	}

	.legend-label {
		font-size: 0.7rem;
		color: #64748b;
	}

	.node-details {
		padding: 1rem;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
		background: white;
		overflow-y: auto;
	}

	.node-details h4 {
		margin: 0 0 1rem 0;
		font-size: 1rem;
		font-weight: 600;
		color: #1e293b;
	}

	.detail-section {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.detail-item {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.detail-item .label {
		font-size: 0.75rem;
		font-weight: 600;
		color: #64748b;
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.detail-item .value {
		font-size: 0.875rem;
		color: #1e293b;
	}

	.confidence-score {
		font-weight: 600;
		color: #10b981;
	}

	.tags {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
	}

	.tag {
		display: inline-block;
		padding: 0.25rem 0.75rem;
		background: #f1f5f9;
		border-radius: 9999px;
		font-size: 0.75rem;
		font-weight: 500;
		color: #475569;
	}

	.dependency-tag {
		background: #fef3c7;
		color: #92400e;
	}

	.priority-score {
		color: #8b5cf6;
		font-weight: 600;
	}

	/* Parameter sections */
	.params-section {
		margin-top: 0.75rem;
		padding-top: 0.75rem;
		border-top: 1px solid #e2e8f0;
	}

	.params-list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin-top: 0.5rem;
	}

	.param-item {
		display: flex;
		gap: 0.5rem;
		padding: 0.5rem;
		border-radius: 4px;
		font-size: 0.75rem;
	}

	.param-item.extracted {
		background: #dbeafe;
		border-left: 3px solid #3b82f6;
	}

	.param-item.inherited {
		background: #fef3c7;
		border-left: 3px solid #f59e0b;
	}

	.param-item.available {
		background: #d1fae5;
		border-left: 3px solid #10b981;
	}

	.param-key {
		font-weight: 600;
		color: #1e293b;
	}

	.param-value {
		color: #475569;
		word-break: break-word;
	}

	/* Task context section */
	.context-section {
		margin-top: 0.75rem;
		padding-top: 0.75rem;
		border-top: 1px solid #e2e8f0;
	}

	.context-path {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.25rem;
		margin-top: 0.5rem;
		padding: 0.5rem;
		background: #f8fafc;
		border-radius: 4px;
	}

	.path-separator {
		color: #94a3b8;
		font-weight: bold;
		margin: 0 0.25rem;
	}

	.path-item {
		font-size: 0.75rem;
		color: #64748b;
		padding: 0.125rem 0.5rem;
		background: white;
		border-radius: 4px;
		border: 1px solid #e2e8f0;
	}

	.path-item.current {
		background: #dbeafe;
		color: #1e40af;
		border-color: #3b82f6;
		font-weight: 600;
	}

	.parent-context {
		margin-top: 0.5rem;
		padding: 0.5rem;
		background: #f1f5f9;
		border-radius: 4px;
		font-size: 0.75rem;
	}

	.context-label {
		font-weight: 600;
		color: #475569;
		margin-right: 0.5rem;
	}

	.context-value {
		color: #64748b;
	}
</style>
