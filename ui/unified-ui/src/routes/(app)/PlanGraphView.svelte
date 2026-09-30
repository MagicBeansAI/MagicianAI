<script lang="ts">
	import { onMount, onDestroy, createEventDispatcher } from 'svelte';
	import * as d3 from 'd3';
	import type { PlanGraph, PlanStep, PlanEdge } from '$lib/types/plangraph';
	import { hasResolvedTool } from '$lib/types/plangraph';
	// Local TreeSelect group shape — formerly imported from `$lib/types/agents`
	// before the agent-tool catalog moved to flat MultiSelect on the live pack
	// registry. PlanGraphView's per-step tool override picker still uses
	// TreeSelect, so we declare the structural shape locally.
	interface ToolCategory {
		value: string;
		label: string;
		description?: string;
		children: { value: string; label: string }[];
	}
	import TreeSelect from '$lib/magician/components/native/TreeSelect.svelte';
	import Button from '$lib/magician/components/native/Button.svelte';

	export let planGraph: PlanGraph;
	export let viewMode: 'graph' | 'waterfall' = 'graph';
	export let editable: boolean = false;
	export let editPlanToolGroups: ToolCategory[] = [];

	const dispatch = createEventDispatcher<{ save: { plan: PlanGraph } }>();

	/**
	 * Shared topological sort using Kahn's algorithm with depth-first child placement.
	 * Returns ordered IDs. Handles cycles by appending remaining items.
	 */
	function topoSortIds(
		ids: string[],
		childrenMap: Map<string, string[]>,
		levelMap: Map<string, number>,
		getDeps: (id: string) => string[]
	): string[] {
		const ordered: string[] = [];
		const visited = new Set<string>();

		function visit(id: string) {
			if (visited.has(id)) return;
			visited.add(id);
			ordered.push(id);
			const children = (childrenMap.get(id) || [])
				.sort((a, b) => (levelMap.get(a) || 0) - (levelMap.get(b) || 0));
			for (const childId of children) {
				if (!visited.has(childId) && getDeps(childId).every((d) => visited.has(d))) {
					visit(childId);
				}
			}
		}

		// Start with roots (level 0)
		for (const id of ids) {
			if (getDeps(id).length === 0) visit(id);
		}

		// Safety sweep: repeatedly place items whose deps are satisfied
		let remaining = ids.filter((id) => !visited.has(id));
		while (remaining.length > 0) {
			const before = visited.size;
			for (const id of remaining) {
				if (getDeps(id).every((d) => visited.has(d))) visit(id);
			}
			remaining = ids.filter((id) => !visited.has(id));
			if (visited.size === before) {
				// Cycle or orphan — append the rest
				for (const id of remaining) {
					if (!visited.has(id)) {
						ordered.push(id);
						visited.add(id);
					}
				}
				break;
			}
		}

		return ordered;
	}

	let svgContainer: HTMLDivElement;
	let selectedStep: PlanStep | null = null;
	let zoomBehavior: any = null;
	let svgElement: any = null;

	// ========================================================================
	// Editable waterfall state
	// ========================================================================
	interface EditableStep extends PlanStep {
		depends_on: string[];
	}

	let editableSteps: EditableStep[] = [];
	let editableEdges: PlanEdge[] = [];

	// Drag-drop
	let editDragIndex: number | null = null;
	let editDropTarget: { type: 'before' | 'after' | 'child'; index: number } | null = null;

	// UI state
	let expandedToolStep: number | null = null;
	let detailToolOpen = false;
	let editingParamStep: number | null = null;
	let newParamKey = '';
	let newParamValue = '';

	// Init editable steps from planGraph (only once per planGraph identity, skip post-save updates)
	let lastInitPlanGraph: PlanGraph | null = null;
	let skipNextInit = false;
	let skipNextInitTimer: ReturnType<typeof setTimeout> | null = null;

	onDestroy(() => {
		if (skipNextInitTimer) clearTimeout(skipNextInitTimer);
	});
	$: if (editable && planGraph && planGraph !== lastInitPlanGraph) {
		if (skipNextInit) {
			skipNextInit = false;
			lastInitPlanGraph = planGraph;
		} else {
			lastInitPlanGraph = planGraph;
			const depsMap = new Map<string, string[]>();
			for (const edge of planGraph.edges) {
				if (!depsMap.has(edge.to)) depsMap.set(edge.to, []);
				depsMap.get(edge.to)!.push(edge.from);
			}
			editableSteps = planGraph.steps.map((s) => ({
				...structuredClone(s),
				depends_on: depsMap.get(s.id) || []
			}));
			editableEdges = [...planGraph.edges];
			sortEditableStepsByLevel();
		}
	}

	// Editable waterfall helpers
	function computeEditDepthMap(): Map<string, number> {
		const depthMap = new Map<string, number>();
		function getDepth(id: string, visited: Set<string>): number {
			if (depthMap.has(id)) return depthMap.get(id)!;
			if (visited.has(id)) return 0;
			visited.add(id);
			const step = editableSteps.find((s) => s.id === id);
			if (!step || step.depends_on.length === 0) {
				depthMap.set(id, 0);
				return 0;
			}
			const maxDep = Math.max(
				...step.depends_on.map((d) => getDepth(d, new Set(visited)))
			);
			const depth = maxDep + 1;
			depthMap.set(id, depth);
			return depth;
		}
		for (const s of editableSteps) getDepth(s.id, new Set());
		return depthMap;
	}

	function sortEditableStepsByLevel() {
		const depthMap = computeEditDepthMap();
		const childrenMap = new Map<string, string[]>();
		const stepDepsMap = new Map(editableSteps.map((s) => [s.id, s.depends_on]));

		for (const step of editableSteps) {
			for (const dep of step.depends_on) {
				if (!childrenMap.has(dep)) childrenMap.set(dep, []);
				childrenMap.get(dep)!.push(step.id);
			}
		}

		const ordered = topoSortIds(
			editableSteps.map((s) => s.id),
			childrenMap,
			depthMap,
			(id) => stepDepsMap.get(id) || []
		);

		const idxMap = new Map(ordered.map((id, i) => [id, i]));
		editableSteps = editableSteps.slice().sort((a, b) =>
			(idxMap.get(a.id) ?? 999) - (idxMap.get(b.id) ?? 999)
		);
	}

	function handleEditDragStart(index: number) {
		editDragIndex = index;
	}

	function handleEditDragOver(e: DragEvent, index: number) {
		e.preventDefault();
		if (editDragIndex === null || editDragIndex === index) {
			editDropTarget = null;
			return;
		}
		if (e.shiftKey) {
			// Shift held = make child (add dependency)
			editDropTarget = { type: 'child', index };
		} else {
			// Normal drop = reorder (before/after at midpoint)
			const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
			const midY = rect.top + rect.height / 2;
			editDropTarget = {
				type: e.clientY < midY ? 'before' : 'after',
				index
			};
		}
	}

	function handleEditDragEnd(_e?: DragEvent) {
		if (editDragIndex !== null && editDropTarget !== null) {
			const draggedStep = editableSteps[editDragIndex];
			const targetStep = editableSteps[editDropTarget.index];

			const isChild = editDropTarget.type === 'child';

			if (isChild) {
				// Make dragged step depend on target step (dragged becomes child of target)
				if (!wouldCreateCycle(draggedStep.id, targetStep.id)) {
					if (!draggedStep.depends_on.includes(targetStep.id)) {
						draggedStep.depends_on = [...draggedStep.depends_on, targetStep.id];
						editableSteps = editableSteps;
						sortEditableStepsByLevel();
					}
				}
			} else {
				// Reorder
				const step = editableSteps[editDragIndex];
				const newSteps = editableSteps.filter((_, i) => i !== editDragIndex);
				let insertAt = editDropTarget.index;
				if (editDragIndex < editDropTarget.index) insertAt--;
				if (editDropTarget.type === 'after') insertAt++;
				newSteps.splice(insertAt, 0, step);
				editableSteps = newSteps;
			}
		}
		editDragIndex = null;
		editDropTarget = null;
	}

	// Check if adding depId as a dependency of stepId would create a cycle
	function wouldCreateCycle(stepId: string, depId: string): boolean {
		// If depId transitively depends on stepId, adding stepId -> depId would cycle
		const visited = new Set<string>();
		function reachable(from: string): boolean {
			if (from === stepId) return true;
			if (visited.has(from)) return false;
			visited.add(from);
			const s = editableSteps.find((st) => st.id === from);
			if (!s) return false;
			return s.depends_on.some((d) => reachable(d));
		}
		return reachable(depId);
	}

	function addEditableStep() {
		const newId = `step_${Date.now()}`;
		editableSteps = [
			...editableSteps,
			{
				id: newId,
				task: 'New step',
				tool: null,
				parameters: {},
				expected_outputs: [],
				confidence: 0.5,
				metadata: {},
				depends_on: []
			}
		];
	}

	function removeEditableStep(index: number) {
		const removedId = editableSteps[index].id;
		editableSteps = editableSteps.filter((_, i) => i !== index);
		// Remove dependencies pointing to removed step
		for (const s of editableSteps) {
			s.depends_on = s.depends_on.filter((d) => d !== removedId);
		}
		editableSteps = editableSteps;
	}

	function updateParam(stepIdx: number, key: string, value: string) {
		editableSteps[stepIdx].parameters[key] = value;
		editableSteps = editableSteps;
	}

	function removeParam(stepIdx: number, key: string) {
		delete editableSteps[stepIdx].parameters[key];
		editableSteps = editableSteps;
	}

	function addParam(stepIdx: number) {
		if (!newParamKey.trim()) return;
		editableSteps[stepIdx].parameters[newParamKey.trim()] = newParamValue;
		editableSteps = editableSteps;
		newParamKey = '';
		newParamValue = '';
		editingParamStep = null;
	}

	function rebuildEdges(): PlanEdge[] {
		const edges: PlanEdge[] = [];
		for (const step of editableSteps) {
			for (const dep of step.depends_on) {
				const existing = editableEdges.find(
					(e) => e.from === dep && e.to === step.id
				);
				edges.push({
					from: dep,
					to: step.id,
					reason: existing?.reason || 'dependency'
				});
			}
		}
		return edges;
	}

	export function requestSave() {
		const plan: PlanGraph = {
			steps: editableSteps.map(({ depends_on, ...rest }) => rest),
			edges: rebuildEdges(),
			unresolved_inputs: planGraph.unresolved_inputs || [],
			confidence: planGraph.confidence,
			provenance: planGraph.provenance
		};
		// Prevent the save response from resetting editableSteps
		skipNextInit = true;
		// Auto-reset if parent never updates planGraph (e.g. save failure)
		if (skipNextInitTimer) clearTimeout(skipNextInitTimer);
		skipNextInitTimer = setTimeout(() => { skipNextInit = false; skipNextInitTimer = null; }, 5000);
		dispatch('save', { plan });
	}

	function handleToolSelect(stepIdx: number, tool: string) {
		editableSteps[stepIdx].tool = tool || null;
		editableSteps = editableSteps;
		expandedToolStep = null;
	}

	// Dependency management
	function addDependency(stepIdx: number, depId: string) {
		if (!editableSteps[stepIdx].depends_on.includes(depId)) {
			editableSteps[stepIdx].depends_on = [...editableSteps[stepIdx].depends_on, depId];
			editableSteps = editableSteps;
			sortEditableStepsByLevel();
		}
	}

	function removeDependency(stepIdx: number, depId: string) {
		editableSteps[stepIdx].depends_on = editableSteps[stepIdx].depends_on.filter((d) => d !== depId);
		editableSteps = editableSteps;
		sortEditableStepsByLevel();
	}

	// Get available steps that can be added as dependencies (exclude self and existing deps, prevent cycles)
	function getAvailableDeps(stepIdx: number): EditableStep[] {
		const step = editableSteps[stepIdx];
		const existing = new Set(step.depends_on);
		// Also exclude steps that depend on this step (would create cycle)
		const dependents = new Set<string>();
		function collectDependents(id: string) {
			for (const s of editableSteps) {
				if (s.depends_on.includes(id) && !dependents.has(s.id)) {
					dependents.add(s.id);
					collectDependents(s.id);
				}
			}
		}
		collectDependents(step.id);
		return editableSteps.filter((s) => s.id !== step.id && !existing.has(s.id) && !dependents.has(s.id));
	}

	let addingDepForStep: number | null = null;

	// Reactive index for editing selectedStep in the details panel
	$: selectedEditIdx = editable && selectedStep
		? editableSteps.findIndex((s) => s.id === selectedStep?.id)
		: -1;

	// Reset tool selector when step changes
	$: if (selectedStep) detailToolOpen = false;

	// Metadata helpers
	let newMetaKey = '';
	let newMetaValue = '';

	function updateMeta(stepIdx: number, key: string, value: string) {
		editableSteps[stepIdx].metadata[key] = value;
		editableSteps = editableSteps;
	}

	function removeMeta(stepIdx: number, key: string) {
		delete editableSteps[stepIdx].metadata[key];
		editableSteps = editableSteps;
	}

	function addMeta(stepIdx: number) {
		if (!newMetaKey.trim()) return;
		editableSteps[stepIdx].metadata[newMetaKey.trim()] = newMetaValue;
		editableSteps = editableSteps;
		newMetaKey = '';
		newMetaValue = '';
	}

	// Expected outputs helpers
	function updateOutput(stepIdx: number, outIdx: number, value: string) {
		editableSteps[stepIdx].expected_outputs[outIdx] = value;
		editableSteps = editableSteps;
	}

	function removeOutput(stepIdx: number, outIdx: number) {
		editableSteps[stepIdx].expected_outputs.splice(outIdx, 1);
		editableSteps = editableSteps;
	}

	function addOutput(stepIdx: number) {
		editableSteps[stepIdx].expected_outputs.push('');
		editableSteps = editableSteps;
	}

	// Graph edge editing state
	let graphLinkSource: string | null = null; // step ID of edge-draw origin

	function graphRemoveEdge(fromId: string, toId: string) {
		// Remove from editableSteps depends_on
		const step = editableSteps.find((s) => s.id === toId);
		if (step) {
			step.depends_on = step.depends_on.filter((d) => d !== fromId);
			editableSteps = editableSteps;
		}
		// Re-render the graph
		renderGraph();
	}

	function graphAddEdge(fromId: string, toId: string) {
		if (fromId === toId) return;
		const step = editableSteps.find((s) => s.id === toId);
		if (!step) return;
		if (step.depends_on.includes(fromId)) return;
		if (wouldCreateCycle(toId, fromId)) return;
		step.depends_on = [...step.depends_on, fromId];
		editableSteps = editableSteps;
		renderGraph();
	}

	// Toggle a step as starting node (clear all its dependencies)
	function makeStartingNode(stepId: string) {
		const step = editableSteps.find((s) => s.id === stepId);
		if (!step) return;
		step.depends_on = [];
		editableSteps = editableSteps;
		sortEditableStepsByLevel();
	}

	interface GraphNode extends d3.SimulationNodeDatum {
		id: string;
		step: PlanStep;
	}

	interface GraphLink extends d3.SimulationLinkDatum<GraphNode> {
		edge: PlanEdge;
	}

	onMount(() => {
		if (!planGraph || planGraph.steps.length === 0) {
			console.warn('[PlanGraphView] No steps to visualize');
			return;
		}

		if (viewMode === 'graph') {
			renderGraph();
		}
	});

	// Re-render graph when switching to graph view
	$: if (viewMode === 'graph' && svgContainer) {
		renderGraph();
	}

	function themeColor(tokenName: string, fallback: string): string {
		if (typeof window === 'undefined') return fallback;
		const scope = svgContainer ?? document.documentElement;
		return getComputedStyle(scope).getPropertyValue(tokenName).trim() || fallback;
	}

	function graphTheme() {
		return {
			accent: themeColor('--accent-primary', 'currentColor'),
			accentSoft: themeColor('--accent-primary-soft', 'var(--bg-soft)'),
			delegated: themeColor('--accent-secondary', 'var(--accent-primary)'),
			delegatedSoft: themeColor('--accent-secondary-soft', 'var(--bg-soft)'),
			edge: themeColor('--border-default', 'currentColor'),
			error: themeColor('--color-error', 'var(--status-failed)'),
			success: themeColor('--color-success', 'var(--status-completed)'),
			surface: themeColor('--bg-card', 'Canvas'),
			text: themeColor('--text-primary', 'CanvasText'),
			textMuted: themeColor('--text-muted', 'CanvasText'),
			textOnAccent: themeColor('--text-on-accent', 'Canvas'),
			warning: themeColor('--color-warning', 'var(--status-attention)')
		};
	}

	function renderGraph() {
		// Clear previous content
		d3.select(svgContainer).selectAll('*').remove();

		const width = svgContainer.clientWidth || 800;
		const height = 450;
		const theme = graphTheme();

		// Create SVG with zoom capability
		const svg = d3
			.select(svgContainer)
			.append('svg')
			.attr('width', '100%')
			.attr('height', '100%')
			.attr('viewBox', `0 0 ${width} ${height}`)
			.style('cursor', 'grab');

		// Create zoom behavior
		const zoom = d3.zoom<SVGSVGElement, unknown>()
			.scaleExtent([0.3, 3])
			.on('zoom', (event) => {
				g.attr('transform', event.transform);
			});

		// Apply zoom to SVG
		svg.call(zoom);

		// Store references for zoom controls
		svgElement = svg;
		zoomBehavior = zoom;

		// Create main group for content
		const g = svg.append('g');

		// Prepare data — use editable data when in edit mode
		const sourceSteps = editable ? editableSteps : planGraph.steps;
		const sourceEdges = editable ? rebuildEdges() : planGraph.edges;

		const nodes: GraphNode[] = sourceSteps.map((step) => ({
			id: step.id,
			step: step
		}));

		const links: GraphLink[] = sourceEdges.map((edge) => ({
			source: edge.from,
			target: edge.to,
			edge: edge
		}));

		// Create force simulation for DAG layout
		const simulation = d3
			.forceSimulation<GraphNode>(nodes)
			.force(
				'link',
				d3
					.forceLink<GraphNode, GraphLink>(links)
					.id((d) => d.id)
					.distance(150)
			)
			.force('charge', d3.forceManyBody().strength(-500))
			.force('center', d3.forceCenter(width / 2, height / 2))
			.force('collision', d3.forceCollide().radius(60));

		// Create arrow markers for edges
		svg
			.append('defs')
			.selectAll('marker')
			.data(['end'])
			.enter()
			.append('marker')
			.attr('id', 'arrowhead')
			.attr('viewBox', '0 -5 10 10')
			.attr('refX', 35)
			.attr('refY', 0)
			.attr('markerWidth', 6)
			.attr('markerHeight', 6)
			.attr('orient', 'auto')
			.append('path')
			.attr('d', 'M0,-5L10,0L0,5')
			.attr('fill', theme.edge);

		// Draw links (edges)
		const linkGroup = g.append('g');

		// Draw visible links
		const link = linkGroup
			.selectAll('line.link-visible')
			.data(links)
			.enter()
			.append('line')
			.attr('class', 'link-visible')
			.attr('stroke', theme.edge)
			.attr('stroke-width', 2)
			.attr('marker-end', 'url(#arrowhead)');

		// Invisible wider hit area for click-to-delete (only in edit mode)
		if (editable) {
			linkGroup
				.selectAll('line.link-hitarea')
				.data(links)
				.enter()
				.append('line')
				.attr('class', 'link-hitarea')
				.attr('stroke', 'transparent')
				.attr('stroke-width', 14)
				.style('cursor', 'pointer')
				.on('click', (event: MouseEvent, d: GraphLink) => {
					event.stopPropagation();
					const fromId = typeof d.source === 'string' ? d.source : (d.source as GraphNode).id;
					const toId = typeof d.target === 'string' ? d.target : (d.target as GraphNode).id;
					graphRemoveEdge(fromId, toId);
				})
				.on('mouseenter', function (_event: MouseEvent, d: GraphLink) {
					// Highlight the matching visible line
					link.filter((ld: GraphLink) => ld === d)
						.attr('stroke', theme.error)
						.attr('stroke-width', 3);
				})
				.on('mouseleave', function () {
					// Reset all visible lines
					link.attr('stroke', theme.edge).attr('stroke-width', 2);
				});
		}

		// Edge-drawing preview line (only in edit mode)
		let drawLine: d3.Selection<SVGLineElement, unknown, null, undefined> | null = null;
		if (editable) {
			drawLine = g.append('line')
				.attr('class', 'draw-edge-preview')
				.attr('stroke', theme.accent)
				.attr('stroke-width', 2)
				.attr('stroke-dasharray', '6,3')
				.attr('marker-end', 'url(#arrowhead-draw)')
				.style('display', 'none')
				.style('pointer-events', 'none');

			// Add a separate arrowhead for the draw preview
			svg.select('defs')
				.append('marker')
				.attr('id', 'arrowhead-draw')
				.attr('viewBox', '0 -5 10 10')
				.attr('refX', 10)
				.attr('refY', 0)
				.attr('markerWidth', 6)
				.attr('markerHeight', 6)
				.attr('orient', 'auto')
				.append('path')
				.attr('d', 'M0,-5L10,0L0,5')
				.attr('fill', theme.accent);

			// Cancel drawing on background click
			svg.on('click.draw', () => {
				graphLinkSource = null;
				drawLine?.style('display', 'none');
				node.selectAll('circle').filter((_d, i) => i === 0).attr('filter', null);
			});

			// Track mouse for drawing preview line
			svg.on('mousemove.draw', (event: MouseEvent) => {
				if (graphLinkSource && drawLine) {
					const sourceNode = nodes.find((n) => n.id === graphLinkSource);
					if (sourceNode && sourceNode.x != null && sourceNode.y != null) {
						const [mx, my] = d3.pointer(event, g.node());
						drawLine
							.attr('x1', sourceNode.x)
							.attr('y1', sourceNode.y)
							.attr('x2', mx)
							.attr('y2', my)
							.style('display', null);
					}
				}
			});
		}

		// Draw edge labels
		const edgeLabels = g
			.append('g')
			.selectAll('text')
			.data(links)
			.enter()
			.append('text')
			.attr('class', 'edge-label')
			.attr('font-size', '10px')
			.attr('fill', theme.textMuted)
			.attr('text-anchor', 'middle')
			.text((d) => d.edge.reason.substring(0, 20) + (d.edge.reason.length > 20 ? '...' : ''));

		// Draw nodes
		const node = g
			.append('g')
			.selectAll('g')
			.data(nodes)
			.enter()
			.append('g')
			.attr('class', 'node-group')
			.call(
				d3
					.drag<SVGGElement, GraphNode>()
					.on('start', dragstarted)
					.on('drag', dragged)
					.on('end', dragended)
			)
			.on('click', (event: MouseEvent, d: GraphNode) => {
				event.stopPropagation();
				if (editable && graphLinkSource) {
					// Complete edge draw: source -> this node
					if (graphLinkSource !== d.id) {
						graphAddEdge(graphLinkSource, d.id);
					}
					graphLinkSource = null;
					drawLine?.style('display', 'none');
					node.selectAll('circle').filter((_d, i) => i === 0).attr('filter', null);
				} else if (editable && (event.ctrlKey || event.metaKey)) {
					// Ctrl/Cmd+Click: make this a starting node
					makeStartingNode(d.id);
					renderGraph();
				} else if (editable && event.shiftKey) {
					// Start edge draw from this node
					graphLinkSource = d.id;
					// Highlight source node
					d3.select(event.currentTarget as SVGGElement).select('circle')
						.attr('filter', `drop-shadow(0 0 6px ${theme.accent})`);
				} else {
					selectedStep = d.step;
				}
			});

		// Draw node circles
		node
			.append('circle')
			.attr('r', 30)
			.attr('fill', (d) => getNodeColor(d.step))
			.attr('stroke', (d) => {
				if (isDelegated(d.step)) return theme.delegated;
				if (isStartingNode(d.step.id)) return theme.accent;
				return hasResolvedTool(d.step) ? theme.text : theme.textMuted;
			})
			.attr('stroke-width', (d) => {
				if (isDelegated(d.step)) return 3;
				if (isStartingNode(d.step.id)) return 4; // Thicker border for start nodes
				return hasResolvedTool(d.step) ? 3 : 2;
			})
			.attr('stroke-dasharray', (d) => {
				if (isDelegated(d.step)) return '6,3'; // Dashed border for delegated
				return hasResolvedTool(d.step) ? '0' : '5,5';
			})
			.style('cursor', 'pointer');

		// Add confidence indicator
		node
			.append('circle')
			.attr('r', 10)
			.attr('cx', 20)
			.attr('cy', -20)
			.attr('fill', (d) => getConfidenceColor(d.step.confidence))
			.attr('stroke', theme.surface)
			.attr('stroke-width', 2);

		// Add start indicator for starting nodes
		node
			.filter((d) => isStartingNode(d.step.id))
			.append('circle')
			.attr('r', 10)
			.attr('cx', -20)
			.attr('cy', -20)
			.attr('fill', theme.accent)
			.attr('stroke', theme.surface)
			.attr('stroke-width', 2);

		node
			.filter((d) => isStartingNode(d.step.id))
			.append('text')
			.attr('x', -20)
			.attr('y', -20)
			.attr('text-anchor', 'middle')
			.attr('dy', '.35em')
			.attr('font-size', '12px')
			.attr('fill', theme.textOnAccent)
			.text('▶');

		// Add tool icon
		node
			.append('text')
			.attr('text-anchor', 'middle')
			.attr('dy', '.3em')
			.attr('font-size', '20px')
			.text((d) => (d.step.tool ? '🔧' : '❓'));

		// Add node labels
		node
			.append('text')
			.attr('text-anchor', 'middle')
			.attr('dy', 45)
			.attr('font-size', '11px')
			.attr('font-weight', '600')
			.attr('fill', theme.text)
			.text((d) => truncate(d.step.task, 15));

		// Add "via [agent_id]" badge for delegated steps
		const delegatedNodes = node.filter((d) => isDelegated(d.step));

		delegatedNodes
			.append('rect')
			.attr('x', (d) => -(truncate(`via ${d.step.providing_agent_id!}`, 18).length * 3.2 + 8))
			.attr('y', 52)
			.attr('width', (d) => truncate(`via ${d.step.providing_agent_id!}`, 18).length * 6.4 + 16)
			.attr('height', 18)
			.attr('rx', 9)
			.attr('ry', 9)
			.attr('fill', theme.delegatedSoft)
			.attr('stroke', theme.delegated)
			.attr('stroke-width', 1);

		delegatedNodes
			.append('text')
			.attr('text-anchor', 'middle')
			.attr('dy', 65)
			.attr('font-size', '9px')
			.attr('font-weight', '600')
			.attr('fill', theme.delegated)
			.text((d) => truncate(`via ${d.step.providing_agent_id!}`, 18));

		// Update positions on simulation tick
		// Select hit-area lines for tick updates
		const hitareaLines = linkGroup.selectAll('line.link-hitarea');

		simulation.on('tick', () => {
			link
				.attr('x1', (d: any) => d.source.x)
				.attr('y1', (d: any) => d.source.y)
				.attr('x2', (d: any) => d.target.x)
				.attr('y2', (d: any) => d.target.y);

			hitareaLines
				.attr('x1', (d: any) => d.source.x)
				.attr('y1', (d: any) => d.source.y)
				.attr('x2', (d: any) => d.target.x)
				.attr('y2', (d: any) => d.target.y);

			edgeLabels
				.attr('x', (d: any) => (d.source.x + d.target.x) / 2)
				.attr('y', (d: any) => (d.source.y + d.target.y) / 2);

			node.attr('transform', (d: any) => `translate(${d.x},${d.y})`);
		});

		// Fit all nodes in view once simulation settles
		simulation.on('end', () => {
			fitToView(svg, g, zoom, nodes, width, height);
		});

		// Drag functions
		function dragstarted(event: any, d: GraphNode) {
			if (!event.active) simulation.alphaTarget(0.3).restart();
			d.fx = d.x;
			d.fy = d.y;
		}

		function dragged(event: any, d: GraphNode) {
			d.fx = event.x;
			d.fy = event.y;
		}

		function dragended(event: any, d: GraphNode) {
			if (!event.active) simulation.alphaTarget(0);
			d.fx = null;
			d.fy = null;
		}
	}

	// Check whether the step is delegated to a different agent
	function isDelegated(step: PlanStep): boolean {
		return !!step.providing_agent_id;
	}

	// Get node color based on confidence
	function getNodeColor(step: PlanStep): string {
		const confidence = step.confidence;
		if (confidence >= 0.7) return themeColor('--color-success', 'var(--status-completed)');
		if (confidence >= 0.4) return themeColor('--color-warning', 'var(--status-attention)');
		return themeColor('--color-error', 'var(--status-failed)');
	}

	// Get confidence indicator color
	function getConfidenceColor(confidence: number): string {
		if (confidence >= 0.8) return themeColor('--color-success', 'var(--status-completed)');
		if (confidence >= 0.6) return themeColor('--accent-primary', 'var(--status-running)');
		if (confidence >= 0.4) return themeColor('--color-warning', 'var(--status-attention)');
		return themeColor('--color-error', 'var(--status-failed)');
	}

	// Truncate text
	function truncate(text: string, maxLength: number): string {
		return text.length > maxLength ? text.substring(0, maxLength) + '...' : text;
	}

	// Fit all nodes into the visible viewport
	function fitToView(
		svg: d3.Selection<SVGSVGElement, unknown, null, undefined>,
		g: d3.Selection<SVGGElement, unknown, null, undefined>,
		zoom: d3.ZoomBehavior<SVGSVGElement, unknown>,
		nodes: GraphNode[],
		width: number,
		height: number
	) {
		if (nodes.length === 0) return;

		// Calculate bounding box of all node positions
		let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
		for (const n of nodes) {
			const x = n.x ?? 0;
			const y = n.y ?? 0;
			if (x < minX) minX = x;
			if (x > maxX) maxX = x;
			if (y < minY) minY = y;
			if (y > maxY) maxY = y;
		}

		// Add padding for node radius (30) + labels (~50 below, ~30 above)
		const pad = 80;
		minX -= pad;
		minY -= pad;
		maxX += pad;
		maxY += pad;

		const bboxW = maxX - minX;
		const bboxH = maxY - minY;
		if (bboxW <= 0 || bboxH <= 0) return;

		const scale = Math.min(width / bboxW, height / bboxH, 1.5); // cap at 1.5x
		const tx = (width - bboxW * scale) / 2 - minX * scale;
		const ty = (height - bboxH * scale) / 2 - minY * scale;

		const transform = d3.zoomIdentity.translate(tx, ty).scale(scale);
		svg.transition().duration(500).call(zoom.transform, transform);
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
			svgElement.transition().duration(500).call(zoomBehavior.transform, d3.zoomIdentity);
		}
	}

	// Get dependencies for a step
	function getDependencies(stepId: string): string[] {
		return planGraph.edges
			.filter((edge) => edge.to === stepId)
			.map((edge) => edge.from);
	}

	// Get dependency reasons for a step
	function getDependencyReasons(stepId: string): Array<{ from: string; reason: string }> {
		return planGraph.edges
			.filter((edge) => edge.to === stepId)
			.map((edge) => ({ from: edge.from, reason: edge.reason }));
	}

	// Get step by ID
	function getStepById(stepId: string): PlanStep | undefined {
		return planGraph.steps.find((s) => s.id === stepId);
	}

	// Check if a step is a starting node (no incoming edges)
	function isStartingNode(stepId: string): boolean {
		if (editable) {
			const step = editableSteps.find((s) => s.id === stepId);
			return !!step && step.depends_on.length === 0;
		}
		return !planGraph.edges.some((edge) => edge.to === stepId);
	}

	// Check if a step is an ending node (no outgoing edges)
	function isEndingNode(stepId: string): boolean {
		if (editable) {
			return !editableSteps.some((s) => s.depends_on.includes(stepId));
		}
		return !planGraph.edges.some((edge) => edge.from === stepId);
	}

	// Calculate the depth level of each step for indentation
	function calculateStepLevel(stepId: string, visited = new Set<string>()): number {
		// Prevent infinite loops in case of cycles
		if (visited.has(stepId)) return 0;
		visited.add(stepId);

		const dependencies = getDependencies(stepId);
		if (dependencies.length === 0) {
			return 0; // Starting node
		}

		// Level is 1 + max level of dependencies
		const maxDepLevel = Math.max(
			...dependencies.map((depId) => calculateStepLevel(depId, new Set(visited)))
		);
		return maxDepLevel + 1;
	}

	// Get all steps with their levels
	function getStepsWithLevels(): Array<{ step: PlanStep; level: number }> {
		const steps = planGraph.steps;
		const levels = new Map(steps.map((s) => [s.id, calculateStepLevel(s.id)]));

		// Build parent -> children map
		const childrenMap = new Map<string, string[]>();
		for (const edge of planGraph.edges) {
			if (!childrenMap.has(edge.from)) childrenMap.set(edge.from, []);
			childrenMap.get(edge.from)!.push(edge.to);
		}

		const ordered = topoSortIds(
			steps.map((s) => s.id),
			childrenMap,
			levels,
			getDependencies
		);

		const idxMap = new Map(ordered.map((id, i) => [id, i]));
		return steps
			.map((step) => ({ step, level: levels.get(step.id) || 0 }))
			.sort((a, b) => (idxMap.get(a.step.id) ?? 999) - (idxMap.get(b.step.id) ?? 999));
	}
</script>

<div class="plan-graph-container">
	<div class="graph-wrapper">
		<!-- Zoom controls (only for graph view) -->
		{#if viewMode === 'graph'}
			<div class="zoom-controls">
				<Button iconOnly variant="outline" size="sm" title="Zoom In" on:click={zoomIn}>
					<svg slot="icon" width="14" height="14" viewBox="0 0 20 20" fill="currentColor">
						<path d="M10 5a1 1 0 011 1v3h3a1 1 0 110 2h-3v3a1 1 0 11-2 0v-3H6a1 1 0 110-2h3V6a1 1 0 011-1z" />
					</svg>
				</Button>
				<Button iconOnly variant="outline" size="sm" title="Zoom Out" on:click={zoomOut}>
					<svg slot="icon" width="14" height="14" viewBox="0 0 20 20" fill="currentColor">
						<path d="M6 9a1 1 0 000 2h8a1 1 0 100-2H6z" />
					</svg>
				</Button>
				<Button iconOnly variant="outline" size="sm" title="Reset View" className="zoom-reset" on:click={resetZoom}>
					<svg slot="icon" width="14" height="14" viewBox="0 0 20 20" fill="currentColor">
						<path fill-rule="evenodd" d="M4 2a1 1 0 011 1v2.101a7.002 7.002 0 0111.601 2.566 1 1 0 11-1.885.666A5.002 5.002 0 005.999 7H9a1 1 0 010 2H4a1 1 0 01-1-1V3a1 1 0 011-1zm.008 9.057a1 1 0 011.276.61A5.002 5.002 0 0014.001 13H11a1 1 0 110-2h5a1 1 0 011 1v5a1 1 0 11-2 0v-2.101a7.002 7.002 0 01-11.601-2.566 1 1 0 01.61-1.276z" clip-rule="evenodd" />
					</svg>
				</Button>
			</div>
		{/if}

		<!-- Legend (only for graph view) -->
		{#if viewMode === 'graph'}
			<div class="legend">
				<div class="legend-title">Node Colors:</div>
				<div class="legend-items">
					<div class="legend-item">
						<div class="legend-dot legend-dot--high"></div>
						<span>High Confidence (≥70%)</span>
					</div>
					<div class="legend-item">
						<div class="legend-dot legend-dot--medium"></div>
						<span>Medium (40-70%)</span>
					</div>
					<div class="legend-item">
						<div class="legend-dot legend-dot--low"></div>
						<span>Low (&lt;40%)</span>
					</div>
				</div>
				<div class="legend-divider"></div>
				<div class="legend-items">
					<div class="legend-item">
						<div class="legend-border solid"></div>
						<span>Tool Resolved</span>
					</div>
					<div class="legend-item">
						<div class="legend-border dashed"></div>
						<span>Tool Missing</span>
					</div>
				</div>
				<div class="legend-divider"></div>
				<div class="legend-items">
					<div class="legend-item">
						<div class="legend-border start"></div>
						<span>Starting Node ▶</span>
					</div>
					<div class="legend-item">
						<div class="legend-border delegated"></div>
						<span>Delegated Step</span>
					</div>
				</div>
			</div>
		{/if}

		<!-- Graph Visualization (Graph Mode) -->
		{#if viewMode === 'graph'}
			<div class="graph-visualization" bind:this={svgContainer}></div>
		{/if}


		<!-- Waterfall Visualization (Waterfall Mode) - Read Only -->
		{#if viewMode === 'waterfall' && !editable}
				{#each getStepsWithLevels() as { step, level }, index}
					<div
						class="waterfall-step"
						class:waterfall-step-delegated={!!step.providing_agent_id}
						style="{level > 0 ? `border-left: ${1 + level}px solid var(--accent-primary);` : ''}"
						on:click={() => (selectedStep = selectedStep?.id === step.id ? null : step)}
						on:keypress={(e) => e.key === 'Enter' && (selectedStep = selectedStep?.id === step.id ? null : step)}
						role="button"
						tabindex="0"
					>
						<!-- Step Number and Header -->
						<div class="waterfall-step-header">
							<div class="step-number">{index + 1}</div>
							{#if level > 0}
								<div class="level-indicator" title="Execution level {level}">
									L{level}
								</div>
							{/if}
							<div class="step-title">
								<span class="task-text">{step.task}</span>
								<span class="confidence-badge" style="background: {getConfidenceColor(step.confidence)}">
									{(step.confidence * 100).toFixed(0)}%
								</span>
								{#if step.providing_agent_id}
									<span class="delegated-badge">via {step.providing_agent_id}</span>
								{/if}
							</div>
						</div>

						<!-- Step Content -->
						<div class="waterfall-step-content">
							<!-- Tool / Delegation Information -->
							<div class="step-info-row">
								<span class="info-label">{step.providing_agent_id ? 'Delegate:' : 'Tool:'}</span>
								{#if step.providing_agent_id}
									<span class="tool-name resolved">🤝 {step.providing_agent_id}</span>
								{:else if step.tool}
									<span class="tool-name" class:resolved={hasResolvedTool(step)}>
										{step.tool}
										{#if hasResolvedTool(step)}
											<span class="resolved-badge">✓</span>
										{:else}
											<span class="unresolved-badge">⚠</span>
										{/if}
									</span>
								{:else}
									<span class="tool-missing">Not resolved</span>
								{/if}
							</div>

							<!-- Dependencies -->
							{#if getDependencies(step.id).length > 0}
								<div class="step-dependencies">
									<span class="info-label">Depends on:</span>
									<div class="dependencies-list">
										{#each getDependencyReasons(step.id) as dep}
											{@const depStep = getStepById(dep.from)}
											<div class="dependency-item">
												<svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor">
													<polyline points="4 6 8 10 12 6" />
												</svg>
												<span class="dep-step">
													{depStep ? truncate(depStep.task, 30) : dep.from}
												</span>
												<span class="dep-reason">({dep.reason})</span>
											</div>
										{/each}
									</div>
								</div>
							{/if}

							<!-- Parameters (if any) -->
							{#if Object.keys(step.parameters).length > 0}
								<div class="step-parameters">
									<span class="info-label">Parameters:</span>
									<div class="parameters-grid">
										{#each Object.entries(step.parameters).slice(0, 3) as [key, value]}
											<div class="param-pill">
												<span class="param-key">{key}:</span>
												<span class="param-value">{truncate(String(value), 20)}</span>
											</div>
										{/each}
										{#if Object.keys(step.parameters).length > 3}
											<div class="param-pill more">
												+{Object.keys(step.parameters).length - 3} more
											</div>
										{/if}
									</div>
								</div>
							{/if}
						</div>

					</div>
				{/each}
		{/if}

		<!-- Editable Waterfall Mode -->
		{#if viewMode === 'waterfall' && editable}
			{@const depthMap = computeEditDepthMap()}
			{#each editableSteps as step, index (step.id)}
				{@const level = depthMap.get(step.id) || 0}
				<!-- Drop line before -->
				{#if editDropTarget && editDropTarget.type === 'before' && editDropTarget.index === index}
					<div class="drop-line"></div>
				{/if}
				<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
				<div
					class="waterfall-step waterfall-step-editable"
					class:waterfall-step-dragging={editDragIndex === index}
					class:waterfall-step-child-target={editDropTarget?.type === 'child' && editDropTarget?.index === index}
					style="{level > 0 ? `border-left: ${1 + level}px solid var(--accent-primary);` : ''}"
					draggable="true"
					on:dragstart={() => handleEditDragStart(index)}
					on:dragover={(e) => handleEditDragOver(e, index)}
					on:dragend={(e) => handleEditDragEnd(e)}
					on:drop|preventDefault={(e) => handleEditDragEnd(e)}
					on:click={(e) => {
						const tag = (e.target as HTMLElement).tagName;
						if (tag !== 'INPUT' && tag !== 'BUTTON' && tag !== 'SELECT') {
							selectedStep = selectedStep?.id === step.id ? null : step;
						}
					}}
					role="listitem"
				>
					<div class="waterfall-step-header">
						<!-- Drag handle -->
						<div class="drag-handle" title="Drag to reorder">
							<svg width="12" height="16" viewBox="0 0 12 16" fill="currentColor" opacity="0.4">
								<circle cx="3" cy="2" r="1.5"/><circle cx="9" cy="2" r="1.5"/>
								<circle cx="3" cy="6" r="1.5"/><circle cx="9" cy="6" r="1.5"/>
								<circle cx="3" cy="10" r="1.5"/><circle cx="9" cy="10" r="1.5"/>
								<circle cx="3" cy="14" r="1.5"/><circle cx="9" cy="14" r="1.5"/>
							</svg>
						</div>
						<!-- svelte-ignore a11y_click_events_have_key_events -->
						<!-- svelte-ignore a11y_no_static_element_interactions -->
						<div
							class="step-number step-number-clickable"
							title="View step details"
							on:click|stopPropagation={() => (selectedStep = selectedStep?.id === step.id ? null : step)}
						>{index + 1}</div>
						{#if level > 0}
							<div class="level-indicator" title="Execution level {level}">L{level}</div>
						{/if}
						<div class="step-title" style="flex:1">
							<input
								class="task-input-inline"
								type="text"
								bind:value={editableSteps[index].task}
								placeholder="Step description..."
							/>
						</div>
						<span class="confidence-badge" style="background: {getConfidenceColor(step.confidence)}">
							{(step.confidence * 100).toFixed(0)}%
						</span>
						<button
							class="remove-step-btn"
							title="Remove step"
							on:click|stopPropagation={() => removeEditableStep(index)}
						>✕</button>
					</div>

					<div class="waterfall-step-content">
						<!-- Tool selector -->
						<div class="step-info-row">
							<span class="info-label">Tool:</span>
							{#if expandedToolStep === index}
								<div class="tool-select-wrapper">
									<TreeSelect
										single={true}
										groups={editPlanToolGroups}
										values={step.tool ? [step.tool] : []}
										idBase="wf-step-{index}"
										label=""
										on:change={(e) => handleToolSelect(index, e.detail.values[0] || '')}
									/>
									<button class="param-remove" on:click={() => (expandedToolStep = null)}>✕</button>
								</div>
							{:else}
								<button
									class="tool-toggle"
									on:click|stopPropagation={() => (expandedToolStep = expandedToolStep === index ? null : index)}
								>
									{step.tool || 'Select tool...'}
								</button>
							{/if}
						</div>

						<!-- Inline Dependencies -->
						{#if step.depends_on.length > 0}
							<div class="step-info-row">
								<span class="info-label">Deps:</span>
								<div class="inline-deps">
									{#each step.depends_on as depId}
										{@const depStep = editableSteps.find((s) => s.id === depId)}
										<span class="inline-dep-chip">
											<span class="dep-step-number">{editableSteps.findIndex((s) => s.id === depId) + 1}</span>
											<span class="inline-dep-task">{depStep?.task?.slice(0, 30) || depId}{(depStep?.task?.length || 0) > 30 ? '…' : ''}</span>
											<button class="param-remove" title="Remove" on:click|stopPropagation={() => removeDependency(index, depId)}>✕</button>
										</span>
									{/each}
								</div>
							</div>
						{/if}

						<!-- Editable Parameters -->
						{#if Object.keys(step.parameters).length > 0 || editingParamStep === index}
							<div class="step-parameters">
								<span class="info-label">Parameters:</span>
								<div class="parameters-grid">
									{#each Object.entries(step.parameters) as [key, value]}
										<div class="param-pill param-pill-editable">
											<span class="param-key">{key}:</span>
											<input
												class="param-value-input"
												type="text"
												value={String(value)}
												on:change={(e) => updateParam(index, key, (e.target as HTMLInputElement).value)}
											/>
											<button class="param-remove" on:click={() => removeParam(index, key)}>✕</button>
										</div>
									{/each}
									{#if editingParamStep === index}
										<div class="param-add-row">
											<input
												class="param-value-input"
												type="text"
												bind:value={newParamKey}
												placeholder="key"
											/>
											<input
												class="param-value-input"
												type="text"
												bind:value={newParamValue}
												placeholder="value"
											/>
											<button class="param-add-btn" on:click={() => addParam(index)}>+</button>
											<button class="param-remove" on:click={() => (editingParamStep = null)}>✕</button>
										</div>
									{/if}
								</div>
							</div>
						{/if}
						{#if editingParamStep !== index}
							<button
								class="param-add-btn"
								on:click={() => { editingParamStep = index; newParamKey = ''; newParamValue = ''; }}
							>+ Add parameter</button>
						{/if}
					</div>
				</div>
				<!-- Drop line after -->
				{#if editDropTarget && editDropTarget.type === 'after' && editDropTarget.index === index}
					<div class="drop-line"></div>
				{/if}
			{/each}

			<!-- Add step button -->
			<button class="add-step-btn" on:click={addEditableStep}>
				+ Add Step
			</button>
		{/if}
	</div>

	<!-- Step Details Panel -->
	{#if selectedStep}
		<div class="step-details">
			<div class="details-header">
				<h4>📋 Step Details</h4>
				<button class="close-details" on:click={() => (selectedStep = null)}>✕</button>
			</div>

			<div class="details-content">
				{#if editable && selectedEditIdx >= 0}
					<!-- Editable mode -->
					<div class="detail-item">
						<span class="label">Task:</span>
						<textarea
							class="detail-edit-input detail-textarea"
							bind:value={editableSteps[selectedEditIdx].task}
							rows="2"
						></textarea>
					</div>

					<div class="detail-item">
						<span class="label">Step ID:</span>
						<span class="value mono">{selectedStep.id}</span>
					</div>

					<div class="detail-item">
						<span class="label">Start node:</span>
						{#if editableSteps[selectedEditIdx].depends_on.length === 0}
							<span class="value start-badge">Yes (root)</span>
							{@const available = getAvailableDeps(selectedEditIdx)}
							{#if available.length > 0}
								<button
									class="detail-add-row-btn"
									on:click={() => (addingDepForStep = selectedEditIdx)}
								>Add parent</button>
							{/if}
						{:else}
							<button
								class="detail-add-row-btn"
								on:click={() => { makeStartingNode(editableSteps[selectedEditIdx].id); editableSteps = editableSteps; if (viewMode === 'graph') renderGraph(); }}
							>Make starting node</button>
						{/if}
					</div>

					<div class="detail-item">
						<span class="label">Confidence:</span>
						<input
							type="number"
							class="detail-edit-input"
							min="0" max="1" step="0.05"
							bind:value={editableSteps[selectedEditIdx].confidence}
						/>
					</div>

					<div class="detail-item">
						<span class="label">Tool:</span>
						{#if detailToolOpen && editPlanToolGroups.length > 0}
							<div class="detail-tool-select">
								<TreeSelect
									single={true}
									groups={editPlanToolGroups}
									values={editableSteps[selectedEditIdx].tool ? [editableSteps[selectedEditIdx].tool] : []}
									idBase="detail-tool-{selectedEditIdx}"
									label=""
									on:change={(e) => {
										handleToolSelect(selectedEditIdx, e.detail.values[0] || '');
										detailToolOpen = false;
									}}
								/>
								<button class="detail-remove-btn" on:click={() => (detailToolOpen = false)}>✕</button>
							</div>
						{:else}
							<!-- svelte-ignore a11y_click_events_have_key_events -->
							<!-- svelte-ignore a11y_no_static_element_interactions -->
							<span
								class="value tool-badge tool-badge-clickable"
								on:click={() => (detailToolOpen = true)}
							>
								{editableSteps[selectedEditIdx].tool || 'Select tool...'}
							</span>
						{/if}
					</div>

					<!-- Dependencies -->
					<div class="detail-item params">
						<span class="label">Depends on:</span>
						<div class="params-list">
							{#each editableSteps[selectedEditIdx].depends_on as depId}
								{@const depStep = editableSteps.find((s) => s.id === depId)}
								<div class="param-row editable-param-row">
									<span class="dep-step-ref">
										<span class="dep-step-number">{editableSteps.findIndex((s) => s.id === depId) + 1}</span>
										{depStep?.task || depId}
									</span>
									<button class="detail-remove-btn" title="Remove dependency" on:click={() => removeDependency(selectedEditIdx, depId)}>✕</button>
								</div>
							{/each}
							{#if addingDepForStep === selectedEditIdx}
								{@const available = getAvailableDeps(selectedEditIdx)}
								{#if available.length > 0}
									<div class="dep-add-list">
										{#each available as avail}
											<!-- svelte-ignore a11y_click_events_have_key_events -->
											<!-- svelte-ignore a11y_no_static_element_interactions -->
											<div
												class="dep-add-option"
												on:click={() => { addDependency(selectedEditIdx, avail.id); addingDepForStep = null; }}
											>
												<span class="dep-step-number">{editableSteps.findIndex((s) => s.id === avail.id) + 1}</span>
												{avail.task}
											</div>
										{/each}
									</div>
								{:else}
									<span class="value missing">No available steps</span>
								{/if}
								<button class="detail-remove-btn" on:click={() => (addingDepForStep = null)}>✕</button>
							{:else}
								<button class="detail-add-row-btn" on:click={() => (addingDepForStep = selectedEditIdx)}>+ Add dependency</button>
							{/if}
						</div>
					</div>

				<!-- Parameters -->
					<div class="detail-item params">
						<span class="label">Parameters:</span>
						<div class="params-list">
							{#each Object.entries(editableSteps[selectedEditIdx].parameters) as [key, value]}
								<div class="param-row editable-param-row">
									<span class="param-key">{key}:</span>
									<input
										type="text"
										class="detail-edit-input param-input"
										value={value}
										on:input={(e) => updateParam(selectedEditIdx, key, e.currentTarget.value)}
									/>
									<button class="detail-remove-btn" on:click={() => removeParam(selectedEditIdx, key)}>✕</button>
								</div>
							{/each}
							{#if editingParamStep === selectedEditIdx}
								<div class="param-row editable-param-row">
									<input class="detail-edit-input param-input" placeholder="key" bind:value={newParamKey} />
									<input class="detail-edit-input param-input" placeholder="value" bind:value={newParamValue} />
									<button class="detail-add-btn" on:click={() => addParam(selectedEditIdx)}>+</button>
								</div>
							{:else}
								<button class="detail-add-row-btn" on:click={() => (editingParamStep = selectedEditIdx)}>+ Add parameter</button>
							{/if}
						</div>
					</div>

					<!-- Expected Outputs -->
					<div class="detail-item">
						<span class="label">Expected Outputs:</span>
						<div class="params-list">
							{#each editableSteps[selectedEditIdx].expected_outputs as output, outIdx}
								<div class="param-row editable-param-row">
									<input
										type="text"
										class="detail-edit-input param-input"
										value={output}
										on:input={(e) => updateOutput(selectedEditIdx, outIdx, e.currentTarget.value)}
									/>
									<button class="detail-remove-btn" on:click={() => removeOutput(selectedEditIdx, outIdx)}>✕</button>
								</div>
							{/each}
							<button class="detail-add-row-btn" on:click={() => addOutput(selectedEditIdx)}>+ Add output</button>
						</div>
					</div>

					<!-- Metadata -->
					<div class="detail-item metadata">
						<span class="label">Metadata:</span>
						<div class="metadata-list">
							{#each Object.entries(editableSteps[selectedEditIdx].metadata) as [key, value]}
								<div class="meta-row editable-meta-row">
									<span class="meta-key">{key}:</span>
									<input
										type="text"
										class="detail-edit-input"
										value={value}
										on:input={(e) => updateMeta(selectedEditIdx, key, e.currentTarget.value)}
									/>
									<button class="detail-remove-btn" on:click={() => removeMeta(selectedEditIdx, key)}>✕</button>
								</div>
							{/each}
							<div class="meta-add-row">
								<input class="detail-edit-input" placeholder="key" bind:value={newMetaKey} />
								<input class="detail-edit-input" placeholder="value" bind:value={newMetaValue} />
								<button class="detail-add-btn" on:click={() => addMeta(selectedEditIdx)}>+</button>
							</div>
						</div>
					</div>
				{:else}
					<!-- Read-only mode -->
					<div class="detail-item">
						<span class="label">Task:</span>
						<span class="value">{selectedStep.task}</span>
					</div>

					<div class="detail-item">
						<span class="label">Step ID:</span>
						<span class="value mono">{selectedStep.id}</span>
					</div>

					<div class="detail-item">
						<span class="label">Confidence:</span>
						<span class="value confidence">
							{(selectedStep.confidence * 100).toFixed(1)}%
						</span>
					</div>

					{#if selectedStep.providing_agent_id}
						<div class="detail-item">
							<span class="label">Delegate:</span>
							<span class="value tool-badge">🤝 {selectedStep.providing_agent_id}</span>
						</div>
					{:else if selectedStep.tool}
						<div class="detail-item">
							<span class="label">Tool:</span>
							<span class="value tool-badge">{selectedStep.tool}</span>
						</div>
					{:else}
						<div class="detail-item">
							<span class="label">Tool:</span>
							<span class="value missing">Not resolved</span>
						</div>
					{/if}

					{#if selectedStep.providing_agent_id && selectedStep.tool}
						<div class="detail-item">
							<span class="label">Delegate Tool:</span>
							<span class="value delegated-agent-badge">
								via {selectedStep.providing_agent_id}
							</span>
						</div>
					{/if}

					{#if Object.keys(selectedStep.parameters).length > 0}
						<div class="detail-item params">
							<span class="label">Parameters:</span>
							<div class="params-list">
								{#each Object.entries(selectedStep.parameters) as [key, value]}
									<div class="param-row">
										<span class="param-key">{key}:</span>
										<span class="param-value">{value}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}

					{#if selectedStep.expected_outputs.length > 0}
						<div class="detail-item">
							<span class="label">Expected Outputs:</span>
							<ul class="outputs-list">
								{#each selectedStep.expected_outputs as output}
									<li>{output}</li>
								{/each}
							</ul>
						</div>
					{/if}

					{#if Object.keys(selectedStep.metadata).length > 0}
						<div class="detail-item metadata">
							<span class="label">Metadata:</span>
							<div class="metadata-list">
								{#each Object.entries(selectedStep.metadata) as [key, value]}
									<div class="meta-row">
										<span class="meta-key">{key}:</span>
										<span class="meta-value">{value}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}
				{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.plan-graph-container {
		display: grid;
		grid-template-columns: 1fr;
		gap: 0.5rem;
		width: 100%;
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		align-items: start;
	}

	.plan-graph-container:has(.step-details) {
		grid-template-columns: 1fr 320px;
	}

	.graph-wrapper {
		position: relative;
		width: 100%;
		height: calc(100vh - 12.5rem);
		flex-shrink: 0;
	}

	.graph-visualization {
		width: 100%;
		height: 100%;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		overflow: hidden;
	}

	.waterfall-step {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		padding: 0.5rem;
		margin-bottom: 0.25rem;
		cursor: pointer;
		transition: all 0.2s ease;
		position: relative;
	}

	.waterfall-step:hover {
		border-color: var(--accent-primary);
		box-shadow: 0 2px 6px color-mix(in srgb, var(--accent-primary) 15%, transparent);
	}

	/* Delegated step styling in waterfall view */
	.waterfall-step-delegated {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, transparent);
		border-style: dashed;
		background: color-mix(in srgb, var(--accent-primary) 4%, var(--bg-card));
	}

	.waterfall-step-delegated:hover {
		border-color: var(--accent-primary);
		box-shadow: 0 2px 6px color-mix(in srgb, var(--accent-primary) 20%, transparent);
	}

	/* Level indicator badge */
	.level-indicator {
		display: flex;
		align-items: center;
		justify-content: center;
		min-width: 28px;
		height: 24px;
		background: var(--accent-primary);
		color: var(--bg-card);
		border-radius: 12px;
		font-weight: 700;
		font-size: 0.65rem;
		padding: 0 0.375rem;
		flex-shrink: 0;
	}

	.waterfall-step-header {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		margin-bottom: 0.375rem;
	}

	.step-number {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 24px;
		height: 24px;
		background: var(--accent-primary);
		color: var(--bg-card);
		border-radius: 50%;
		font-weight: 700;
		font-size: 0.7rem;
		flex-shrink: 0;
	}

	.step-title {
		flex: 1;
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.task-text {
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--text-primary);
		line-height: 1.3;
	}

	.confidence-badge {
		padding: 0.125rem 0.5rem;
		border-radius: 10px;
		color: var(--bg-card);
		font-size: 0.65rem;
		font-weight: 700;
		white-space: nowrap;
	}

	/* Delegated badge (waterfall view and step title) */
	.delegated-badge {
		padding: 0.125rem 0.5rem;
		border-radius: 10px;
		background: color-mix(in srgb, var(--accent-primary) 10%, var(--bg-surface));
		color: var(--accent-primary);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 30%, transparent);
		font-size: 0.65rem;
		font-weight: 600;
		white-space: nowrap;
	}

	/* Delegated agent badge in step details panel */
	.delegated-agent-badge {
		display: inline-block;
		padding: 0.375rem 0.75rem;
		border-radius: var(--radius-sm, 6px);
		background: var(--accent-primary);
		color: var(--bg-card);
		font-weight: 600;
		font-size: 0.8rem;
	}

	.waterfall-step-content {
		margin-left: 2rem;
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.step-info-row {
		display: flex;
		align-items: center;
		gap: 0.375rem;
		font-size: 0.75rem;
	}

	.info-label {
		font-weight: 600;
		color: var(--text-muted);
		min-width: 60px;
	}

	.tool-name {
		padding: 0.125rem 0.5rem;
		border-radius: 4px;
		background: var(--bg-surface);
		color: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 0.7rem;
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
	}

	.tool-name.resolved {
		background: var(--color-success);
		color: var(--bg-card);
	}

	.resolved-badge {
		background: color-mix(in srgb, var(--text-on-accent) 28%, transparent);
		padding: 0.05rem 0.25rem;
		border-radius: 3px;
		font-size: 0.6rem;
	}

	.unresolved-badge {
		background: var(--color-warning);
		color: var(--bg-card);
		padding: 0.05rem 0.25rem;
		border-radius: 3px;
		font-size: 0.6rem;
	}

	.tool-missing {
		color: var(--color-error);
		font-style: italic;
		font-size: 0.75rem;
	}

	.step-dependencies {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.dependencies-list {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	.dependency-item {
		display: flex;
		align-items: center;
		gap: 0.375rem;
		padding: 0.25rem 0.375rem;
		background: var(--color-warning-soft);
		border-left: 2px solid var(--color-warning);
		border-radius: 3px;
		font-size: 0.7rem;
	}

	.dependency-item svg {
		flex-shrink: 0;
		stroke: var(--color-warning);
		stroke-width: 2;
		width: 12px;
		height: 12px;
	}

	.dep-step {
		font-weight: 600;
		color: var(--text-primary);
	}

	.dep-reason {
		color: var(--text-secondary);
		font-size: 0.65rem;
		font-style: italic;
	}

	.step-parameters {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.parameters-grid {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem;
	}

	.param-pill {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
		padding: 0.125rem 0.5rem;
		background: color-mix(in srgb, var(--accent-primary) 8%, var(--bg-surface));
		border: 1px solid color-mix(in srgb, var(--accent-primary) 25%, transparent);
		border-radius: 10px;
		font-size: 0.65rem;
	}

	.param-pill.more {
		background: color-mix(in srgb, var(--accent-primary) 12%, var(--bg-surface));
		border-color: color-mix(in srgb, var(--accent-primary) 35%, transparent);
		color: var(--accent-primary);
		font-weight: 600;
	}

	.param-key {
		color: var(--accent-primary);
		font-weight: 600;
	}

	.param-value {
		color: var(--text-secondary);
	}

	/* Zoom Controls */
	.zoom-controls {
		position: absolute;
		top: 0.5rem;
		right: 0.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		z-index: 10;
		background: color-mix(in srgb, var(--bg-card) 95%, transparent);
		border-radius: var(--radius-sm, 6px);
		padding: 0.25rem;
		box-shadow: var(--shadow-sm);
		border: 1px solid var(--border-soft);
	}

	/* Reset/refresh button gets a top separator */
	.zoom-controls :global(.zoom-reset) {
		margin-top: 0.125rem;
	}

	/* Legend */
	.legend {
		position: absolute;
		top: 0.5rem;
		left: 0.5rem;
		background: color-mix(in srgb, var(--bg-card) 95%, transparent);
		border-radius: var(--radius-sm, 6px);
		padding: 0.375rem 0.5rem;
		box-shadow: var(--shadow-sm);
		border: 1px solid var(--border-soft);
		z-index: 10;
	}

	.legend-title {
		font-size: 0.6rem;
		font-weight: 600;
		color: var(--text-secondary);
		margin-bottom: 0.25rem;
	}

	.legend-items {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.legend-item {
		display: flex;
		align-items: center;
		gap: 0.3rem;
	}

	.legend-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		border: 1px solid color-mix(in srgb, var(--text-primary) 15%, transparent);
	}

	.legend-dot--high {
		background: var(--color-success);
	}

	.legend-dot--medium {
		background: var(--color-warning);
	}

	.legend-dot--low {
		background: var(--color-error);
	}

	.legend-border {
		width: 14px;
		height: 8px;
		border-radius: 2px;
	}

	.legend-border.solid {
		border: 2px solid var(--text-primary);
	}

	.legend-border.dashed {
		border: 2px dashed var(--text-muted);
	}

	.legend-border.start {
		border: 3px solid var(--accent-primary);
	}

	.legend-border.delegated {
		border: 3px dashed var(--accent-secondary);
	}

	.legend-item span {
		font-size: 0.55rem;
		color: var(--text-muted);
	}

	.legend-divider {
		height: 1px;
		background: var(--border-soft);
		margin: 0.25rem 0;
	}

	/* Step Details Panel */
	.step-details {
		padding: 1rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card);
		overflow-y: auto;
		position: sticky;
		top: 0;
		max-height: calc(100% - 0.5rem);
		align-self: start;
	}

	.details-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 0.5rem;
		padding-bottom: 0.375rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.details-header h4 {
		margin: 0;
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--text-primary);
	}

	.close-details {
		background: transparent;
		border: none;
		color: var(--text-muted);
		font-size: 1.25rem;
		cursor: pointer;
		padding: 0.25rem;
		line-height: 1;
		transition: color 0.2s;
	}

	.close-details:hover {
		color: var(--text-primary);
	}

	.details-content {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.detail-item {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.detail-item .label {
		font-size: 0.7rem;
		font-weight: 600;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.detail-item .value {
		font-size: 0.875rem;
		color: var(--text-primary);
	}

	.value.mono {
		font-family: var(--font-mono);
		background: var(--bg-surface);
		padding: 0.25rem 0.5rem;
		border-radius: 4px;
		font-size: 0.75rem;
	}

	.value.confidence {
		font-weight: 700;
		color: var(--color-success);
	}

	.value.tool-badge {
		background: var(--accent-primary);
		color: var(--bg-card);
		padding: 0.375rem 0.75rem;
		border-radius: var(--radius-sm, 6px);
		display: inline-block;
		font-weight: 600;
		font-size: 0.8rem;
	}

	.value.missing {
		color: var(--color-error);
		font-style: italic;
	}

	.start-badge {
		color: var(--accent-primary);
		font-weight: 600;
		font-size: 0.8rem;
	}

	/* Parameters */
	.detail-item.params {
		border-top: 1px solid var(--border-soft);
		padding-top: 0.75rem;
	}

	.params-list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin-top: 0.25rem;
	}

	.param-row {
		display: flex;
		gap: 0.5rem;
		padding: 0.5rem;
		background: var(--bg-surface);
		border-radius: 4px;
		font-size: 0.75rem;
	}

	.param-key {
		font-weight: 600;
		color: var(--text-secondary);
		font-family: var(--font-mono);
	}

	.param-value {
		color: var(--text-primary);
		word-break: break-word;
		flex: 1;
	}

	/* Outputs */
	.outputs-list {
		margin: 0.25rem 0 0 0;
		padding-left: 1.25rem;
		font-size: 0.8rem;
		color: var(--text-secondary);
	}

	.outputs-list li {
		margin-bottom: 0.25rem;
	}

	/* Metadata */
	.detail-item.metadata {
		border-top: 1px solid var(--border-soft);
		padding-top: 0.75rem;
	}

	.metadata-list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		margin-top: 0.25rem;
	}

	.meta-row {
		display: flex;
		flex-direction: column;
		gap: 0.125rem;
		padding: 0.375rem 0.5rem;
		background: var(--bg-surface);
		border-radius: 4px;
		font-size: 0.7rem;
	}

	.meta-key {
		font-weight: 600;
		color: var(--text-muted);
	}

	.meta-value {
		color: var(--text-secondary);
		word-break: break-word;
		flex: 1;
	}

	/* ========================================================================
	   Editable Detail Panel Styles
	   ======================================================================== */
	.tool-badge-clickable {
		cursor: pointer;
		transition: opacity 0.15s;
	}
	.tool-badge-clickable:hover {
		opacity: 0.8;
	}
	.detail-tool-select {
		display: flex;
		align-items: flex-start;
		gap: 0.25rem;
	}

	.detail-edit-input {
		width: 100%;
		padding: 0.25rem 0.5rem;
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		background: var(--bg-surface);
		color: var(--text-primary);
		font-size: 0.8rem;
		font-family: inherit;
	}

	.detail-edit-input:focus {
		outline: none;
		border-color: var(--accent-primary);
	}

	.detail-textarea {
		resize: vertical;
		min-height: 2.5rem;
	}

	.param-input {
		flex: 1;
		min-width: 0;
	}

	.editable-param-row {
		align-items: center;
	}

	.editable-meta-row {
		flex-direction: row !important;
		align-items: center;
		gap: 0.5rem !important;
	}

	.meta-add-row {
		display: flex;
		gap: 0.375rem;
		align-items: center;
	}

	.meta-add-row .detail-edit-input {
		flex: 1;
		min-width: 0;
	}

	.detail-remove-btn {
		background: none;
		border: none;
		color: var(--color-error);
		cursor: pointer;
		font-size: 0.75rem;
		padding: 0.125rem 0.25rem;
		line-height: 1;
		flex-shrink: 0;
	}

	.detail-remove-btn:hover {
		opacity: 0.7;
	}

	.detail-add-btn {
		background: var(--accent-primary);
		color: var(--text-on-accent);
		border: none;
		border-radius: 4px;
		cursor: pointer;
		font-size: 0.75rem;
		padding: 0.2rem 0.5rem;
		line-height: 1;
		flex-shrink: 0;
	}

	.detail-add-row-btn {
		background: none;
		border: 1px dashed var(--border-soft);
		border-radius: 4px;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 0.7rem;
		padding: 0.25rem 0.5rem;
		text-align: left;
	}

	.detail-add-row-btn:hover {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	/* Dependency editor */
	.dep-step-ref {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.8rem;
		color: var(--text-secondary);
	}

	.dep-step-number {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.4rem;
		height: 1.4rem;
		border-radius: 50%;
		background: var(--accent-primary);
		color: var(--text-on-accent);
		font-size: 0.7rem;
		font-weight: 600;
		flex-shrink: 0;
	}

	.dep-add-list {
		display: flex;
		flex-direction: column;
		gap: 2px;
		max-height: 150px;
		overflow-y: auto;
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		padding: 2px;
	}

	.dep-add-option {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.3rem 0.5rem;
		border-radius: 3px;
		cursor: pointer;
		font-size: 0.8rem;
		color: var(--text-secondary);
		transition: background 0.15s;
	}

	.dep-add-option:hover {
		background: var(--bg-soft);
		color: var(--text-primary);
	}

	.inline-deps {
		display: flex;
		flex-wrap: wrap;
		gap: 4px;
	}

	.inline-dep-chip {
		display: inline-flex;
		align-items: center;
		gap: 3px;
		padding: 1px 6px;
		border-radius: 10px;
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 25%, transparent);
		font-size: 0.72rem;
		color: var(--text-secondary);
	}

	.inline-dep-chip .dep-step-number {
		width: 1.1rem;
		height: 1.1rem;
		font-size: 0.6rem;
	}

	.inline-dep-task {
		max-width: 120px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.inline-dep-chip .param-remove {
		font-size: 0.65rem;
		padding: 0 2px;
	}

	/* ========================================================================
	   Editable Waterfall Styles
	   ======================================================================== */
	.step-number-clickable {
		cursor: pointer;
		transition: transform 0.15s, box-shadow 0.15s;
	}

	.step-number-clickable:hover {
		transform: scale(1.15);
		box-shadow: 0 0 0 2px var(--accent-primary);
	}

	.waterfall-step-editable {
		cursor: grab;
		outline: 1px dashed transparent;
		transition: outline-color 0.15s ease, opacity 0.15s ease;
	}

	.waterfall-step-editable:hover {
		outline-color: var(--accent-primary);
	}

	.waterfall-step-dragging {
		opacity: 0.4;
	}

	.waterfall-step-child-target {
		outline: 2px dashed var(--accent-primary);
		outline-offset: -2px;
		background: color-mix(in srgb, var(--accent-primary) 8%, transparent) !important;
	}

	.drag-handle {
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: grab;
		padding: 0 0.125rem;
		color: var(--text-muted);
		flex-shrink: 0;
	}

	.drag-handle:active {
		cursor: grabbing;
	}

	.task-input-inline {
		flex: 1;
		background: transparent;
		border: 1px solid transparent;
		border-radius: var(--radius-sm, 6px);
		padding: 0.25rem 0.375rem;
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--text-primary);
		font-family: inherit;
		outline: none;
		transition: border-color 0.15s;
	}

	.task-input-inline:focus {
		border-color: var(--accent-primary);
		background: var(--bg-surface);
	}

	.remove-step-btn {
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 0.9rem;
		padding: 0.125rem 0.375rem;
		border-radius: var(--radius-sm, 6px);
		line-height: 1;
		transition: color 0.15s, background 0.15s;
	}

	.remove-step-btn:hover {
		color: var(--color-error);
		background: color-mix(in srgb, var(--color-error) 10%, transparent);
	}

	.tool-toggle {
		background: var(--bg-surface);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		padding: 0.125rem 0.5rem;
		font-size: 0.7rem;
		font-family: var(--font-mono);
		color: var(--text-secondary);
		cursor: pointer;
		transition: border-color 0.15s;
	}

	.tool-toggle:hover {
		border-color: var(--accent-primary);
	}

	.tool-select-wrapper {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		flex: 1;
	}

	.param-pill-editable {
		display: inline-flex;
		align-items: center;
		gap: 0.2rem;
	}

	.param-value-input {
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		padding: 0.125rem 0.25rem;
		font-size: 0.65rem;
		color: var(--text-primary);
		width: 80px;
		outline: none;
	}

	.param-value-input:focus {
		border-color: var(--accent-primary);
	}

	.param-remove {
		background: transparent;
		border: none;
		color: var(--text-muted);
		cursor: pointer;
		font-size: 0.7rem;
		padding: 0 0.2rem;
		line-height: 1;
	}

	.param-remove:hover {
		color: var(--color-error);
	}

	.param-add-btn {
		background: transparent;
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		padding: 0.125rem 0.5rem;
		font-size: 0.65rem;
		color: var(--text-muted);
		cursor: pointer;
		transition: border-color 0.15s, color 0.15s;
	}

	.param-add-btn:hover {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.param-add-row {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.125rem;
	}

	.drop-line {
		height: 2px;
		background: var(--accent-primary);
		border-radius: 1px;
		margin: 0.125rem 0;
	}

	.add-step-btn {
		width: 100%;
		background: transparent;
		border: 2px dashed var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		padding: 0.75rem;
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--text-muted);
		cursor: pointer;
		transition: border-color 0.15s, color 0.15s;
		margin-top: 0.25rem;
	}

	.add-step-btn:hover {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	/* Responsive */
	@media (max-width: 1024px) {
		.plan-graph-container {
			grid-template-columns: 1fr;
			height: auto;
		}

		.graph-wrapper {
			height: 380px;
		}

		.step-details {
			position: static;
			max-height: 400px;
		}
	}
</style>
