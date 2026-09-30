<!--
  ThinkingMapCanvas — a D3 force-directed canvas that renders a Live Thinking Map.

  A premium, fully-themed brainstorm canvas. Built on a D3 force simulation with
  `d3.zoom` pan/zoom and a `themeColor(token, fallback)` helper that reads CSS
  custom properties so every stroke/fill tracks the app's light/dark themes.

  Canvas UX:
    · Zoom-to-cursor    — wheel/trackpad zoom focuses the pointer (d3.zoom owns
                          the wheel; we never override to a fixed center).
    · Double-click zoom — dbl-click zooms IN toward the cursor; ⌥/⇧ + dbl-click
                          zooms OUT. Both use a short eased transition.
    · Buttons/keys      — +/− zoom, 0/f fit-to-view, arrows pan, Esc deselect —
                          all eased. A live "120%" readout sits by the controls.
    · Immediate drag    — grabbing a node pins it instantly (fx/fy set directly,
                          the sim kept calm) so it tracks the cursor 1:1. On drag
                          END the new position is emitted via `on:moveNode`
                          `{ node_id, x, y }` so the PAGE persists it (move_node).
                          A node that already carries a `position` stays anchored.
    · Animated select   — the accent ring fades/scales in (not a hard toggle);
                          background click deselects.
    · Edge readability  — arrowheads; labels that fade with zoom + on hover/
                          selection; hovering an edge highlights it.
    · Export dropdown   — toolbar "Export": SVG (live svg serialized standalone,
                          theme colors inlined), PNG (that SVG rasterized at 2×),
                          Markdown (backend /export/markdown with provisional/
                          superseded inclusion toggles). SVG/PNG export exactly
                          what's rendered.

  Visual:
    · Node cards colored by `kind` mapped to semantic tokens, elevation shadow,
      and a colored kind header. Provisional / model_inferred → dashed + spark ✦;
      confirmed → emphasized; contradicted/superseded/rejected → faded.
    · New nodes get a subtle spring entrance (respecting prefers-reduced-motion).

  Events: `on:selectNode` emits `{ nodeId }`; `on:moveNode` emits
  `{ node_id, x, y }` on drag end for the page to persist.

  `readOnly` (history/replay mode) disables node dragging entirely — selection,
  pan and zoom still work, but nodes can't be repositioned and no `moveNode`
  events fire.
-->
<script lang="ts">
	import { onMount, onDestroy, createEventDispatcher } from 'svelte';
	import * as d3 from 'd3';
	import { exportMarkdown } from './api';
	import { themeStore } from '$lib/shared/stores/themeStore';
	import type {
		ThinkingMap,
		ThinkingNode,
		ThinkingEdge,
		NodeKind,
		EdgeKind
	} from '$lib/types/thinkingMap';

	export let map: ThinkingMap;
	export let selectedNodeId: string | undefined = undefined;
	/** History/replay mode: node drags disabled, no moveNode events. */
	export let readOnly = false;

	const dispatch = createEventDispatcher<{
		selectNode: { nodeId: string };
		moveNode: { node_id: string; x: number; y: number };
	}>();

	let svgContainer: HTMLDivElement;
	let svgElement: d3.Selection<SVGSVGElement, unknown, null, undefined> | null = null;
	let zoomBehavior: d3.ZoomBehavior<SVGSVGElement, unknown> | null = null;
	let simulation: d3.Simulation<CanvasNode, CanvasLink> | null = null;

	/** Live zoom readout (percentage) — updated by the zoom handler. */
	let zoomPct = 100;

	// ── Export dropdown state ───────────────────────────────────────────────────
	let exportOpen = false;
	/** Markdown inclusion toggles (SVG/PNG always export what's rendered). */
	let exportIncludeProvisional = true;
	let exportIncludeSuperseded = false;
	let exportBusy: 'png' | 'md' | null = null;
	let exportWrap: HTMLDivElement | null = null;

	/**
	 * The last applied pan/zoom transform. A poll-driven re-render rebuilds the
	 * SVG; carrying this forward keeps the user's viewport steady across live
	 * updates instead of snapping back to the identity transform.
	 */
	let lastTransform: d3.ZoomTransform | null = null;
	/**
	 * Which map_id we've auto-fitted. We fit ONCE per map (on its first layout);
	 * subsequent re-renders (polls, mutations) keep the current viewport.
	 */
	let fittedMapId: string | null = null;

	const NODE_W = 138;
	const NODE_H = 50;
	const SCALE_MIN = 0.1;
	const SCALE_MAX = 4;

	const reducedMotion =
		typeof window !== 'undefined' &&
		typeof window.matchMedia === 'function' &&
		window.matchMedia('(prefers-reduced-motion: reduce)').matches;

	/** Node ids we've already rendered — so only genuinely-new nodes animate in. */
	let seenNodeIds = new Set<string>();

	// ── D3 datum shapes ─────────────────────────────────────────────────────────
	interface CanvasNode extends d3.SimulationNodeDatum {
		id: string;
		node: ThinkingNode;
		/** True when the node carries an authored position → anchored in the sim. */
		anchored: boolean;
		/** True the first time this id is rendered → plays the entrance. */
		fresh: boolean;
	}

	interface CanvasLink extends d3.SimulationLinkDatum<CanvasNode> {
		id: string;
		/** 'semantic' = a map edge; 'parent' = a parent_id hierarchy link. */
		linkType: 'semantic' | 'parent';
		edgeKind?: EdgeKind;
	}

	// ── Theme (reads CSS custom properties off the container) ─────────────────────
	function themeColor(tokenName: string, fallback: string): string {
		if (typeof window === 'undefined') return fallback;
		const scope = svgContainer ?? document.documentElement;
		return getComputedStyle(scope).getPropertyValue(tokenName).trim() || fallback;
	}

	function palette() {
		return {
			text: themeColor('--text-primary', 'CanvasText'),
			textMuted: themeColor('--text-muted', 'CanvasText'),
			textOnAccent: themeColor('--text-on-accent', 'Canvas'),
			surface: themeColor('--bg-card', 'Canvas'),
			surfaceSoft: themeColor('--bg-soft', 'Canvas'),
			border: themeColor('--border-default', 'currentColor'),
			borderSoft: themeColor('--border-soft', 'var(--border-default)'),
			accent: themeColor('--accent-primary', 'currentColor'),
			accentSecondary: themeColor('--accent-secondary', 'var(--accent-primary)'),
			success: themeColor('--color-success', 'var(--status-completed)'),
			warning: themeColor('--color-warning', 'var(--status-attention)'),
			error: themeColor('--color-error', 'var(--status-failed)'),
			info: themeColor('--color-info', 'var(--status-running)')
		};
	}

	/**
	 * Kind → semantic token. Follows the natural mapping:
	 * question/fact/evidence → info, risk/assumption → warning, action/metric →
	 * success, decision → accent-secondary, idea → accent-primary, group → muted.
	 */
	function nodeColor(kind: NodeKind): string {
		const p = palette();
		switch (kind) {
			case 'idea':
				return p.accent;
			case 'question':
			case 'fact':
			case 'evidence':
				return p.info;
			case 'decision':
				return p.accentSecondary;
			case 'option':
				return p.warning;
			case 'risk':
				return p.error;
			case 'action':
			case 'metric':
				return p.success;
			case 'assumption':
				return p.warning;
			case 'group':
				return p.textMuted;
			default:
				return p.accent;
		}
	}

	/** Color for a semantic edge by its kind. */
	function edgeColor(kind: EdgeKind): string {
		const p = palette();
		switch (kind) {
			case 'supports':
				return p.success;
			case 'contradicts':
				return p.error;
			case 'answers':
			case 'measures':
				return p.info;
			case 'alternative_to':
			case 'depends_on':
				return p.warning;
			case 'grouped_under':
				return p.accentSecondary;
			case 'leads_to':
				return p.accent;
			case 'related_to':
			default:
				return p.textMuted;
		}
	}

	/**
	 * Shape reinforces color so relationship meaning survives color-vision
	 * differences. Forward/supporting relationships stay solid; conflict,
	 * alternatives, dependencies, measurement, grouping, and loose association
	 * each retain a visibly distinct rhythm.
	 */
	function edgeDasharray(kind: EdgeKind): string | null {
		switch (kind) {
			case 'contradicts':
				return '9,4';
			case 'alternative_to':
				return '5,4';
			case 'depends_on':
			case 'measures':
				return '2,4';
			case 'grouped_under':
				return '7,4';
			case 'related_to':
				return '3,5';
			default:
				return null;
		}
	}

	function truncate(text: string, maxLength: number): string {
		return text.length > maxLength ? text.slice(0, maxLength - 1).trimEnd() + '…' : text;
	}

	/** A provisional / model-inferred assertion reads as "AI-suggested". */
	function isProvisional(n: ThinkingNode): boolean {
		return n.assertion_origin === 'model_inferred' || n.epistemic_state === 'provisional';
	}

	function isEmphasized(n: ThinkingNode): boolean {
		return n.epistemic_state === 'confirmed' || n.epistemic_state === 'resolved';
	}

	function isFaded(n: ThinkingNode): boolean {
		return (
			n.epistemic_state === 'contradicted' ||
			n.epistemic_state === 'superseded' ||
			n.epistemic_state === 'rejected'
		);
	}

	function nodeOpacity(n: ThinkingNode): number {
		if (isFaded(n)) return 0.42;
		if (isProvisional(n)) return 0.9;
		return 1;
	}

	// ── Build force graph ─────────────────────────────────────────────────────────
	function buildData(): { nodes: CanvasNode[]; links: CanvasLink[] } {
		const liveNodes = Object.values(map.nodes ?? {}).filter((n) => !n.tombstoned);
		const nodeIds = new Set(liveNodes.map((n) => n.node_id));

		const nodes: CanvasNode[] = liveNodes.map((n) => {
			const anchored = !!n.position;
			return {
				id: n.node_id,
				node: n,
				x: n.position?.x,
				y: n.position?.y,
				// Owner-placed nodes are pinned so a persisted layout survives the sim.
				fx: anchored ? n.position!.x : undefined,
				fy: anchored ? n.position!.y : undefined,
				anchored,
				fresh: !seenNodeIds.has(n.node_id)
			};
		});

		const links: CanvasLink[] = [];

		// Semantic edges (skip tombstoned + dangling endpoints).
		for (const e of Object.values(map.edges ?? {}) as ThinkingEdge[]) {
			if (e.tombstoned) continue;
			if (!nodeIds.has(e.from_node) || !nodeIds.has(e.to_node)) continue;
			links.push({
				id: `edge:${e.edge_id}`,
				source: e.from_node,
				target: e.to_node,
				linkType: 'semantic',
				edgeKind: e.kind
			});
		}

		// Parent hierarchy links (the canonical tree lives in parent_id).
		for (const n of liveNodes) {
			if (n.parent_id && nodeIds.has(n.parent_id)) {
				links.push({
					id: `parent:${n.parent_id}->${n.node_id}`,
					source: n.parent_id,
					target: n.node_id,
					linkType: 'parent'
				});
			}
		}

		// Remember every id we're about to draw so the NEXT render only animates
		// nodes that weren't here before.
		seenNodeIds = new Set(liveNodes.map((n) => n.node_id));

		return { nodes, links };
	}

	function render() {
		if (!svgContainer) return;
		// A map revision or theme swap rebuilds the SVG. Stop the detached force
		// graph first so it cannot keep ticking behind the replacement canvas.
		simulation?.stop();
		simulation = null;
		d3.select(svgContainer).selectAll('*').remove();

		const { nodes, links } = buildData();

		const width = svgContainer.clientWidth || 800;
		const height = svgContainer.clientHeight || 450;
		const p = palette();

		const svg = d3
			.select(svgContainer)
			.append('svg')
			.attr('width', '100%')
			.attr('height', '100%')
			.attr('viewBox', `0 0 ${width} ${height}`)
			.style('cursor', 'grab')
			.style('display', 'block');

		// d3.zoom owns the wheel → zoom focuses the pointer automatically.
		const zoom = d3
			.zoom<SVGSVGElement, unknown>()
			.scaleExtent([SCALE_MIN, SCALE_MAX])
			.on('zoom', (event) => {
				g.attr('transform', event.transform.toString());
				zoomPct = Math.round(event.transform.k * 100);
				lastTransform = event.transform;
			});
		svg.call(zoom);
		// Our own dbl-click handler drives zoom (toward the pointer), so disable
		// d3's built-in dblclick.zoom to avoid a double transition.
		svg.on('dblclick.zoom', null);

		svgElement = svg;
		zoomBehavior = zoom;

		const g = svg.append('g');

		// Carry the prior viewport forward so a live re-render doesn't snap the
		// user's pan/zoom back to identity. (Applied without a transition.)
		if (lastTransform) {
			svg.call(zoom.transform, lastTransform);
		}

		// Empty map → nothing to lay out; the calm empty-state overlay handles it.
		if (nodes.length === 0) {
			simulation = null;
			zoomPct = 100;
			return;
		}

		// ── defs: soft node shadow + per-kind arrowheads ─────────────────────────
		const defs = svg.append('defs');

		const shadow = defs
			.append('filter')
			.attr('id', 'tm-shadow')
			.attr('x', '-40%')
			.attr('y', '-40%')
			.attr('width', '180%')
			.attr('height', '180%');
		shadow
			.append('feDropShadow')
			.attr('dx', 0)
			.attr('dy', 2)
			.attr('stdDeviation', 3)
			.attr('flood-color', p.text)
			.attr('flood-opacity', 0.14);

		// Structural hierarchy gets its own neutral arrow. It intentionally does
		// not borrow a semantic color because `parent_id` means containment, not
		// support/contradiction/dependency.
		defs
			.append('marker')
			.attr('id', 'tm-parent-arrow')
			.attr('viewBox', '0 -5 10 10')
			// The line itself now terminates at the target card boundary, so the
			// marker tip—not an oversized compensating offset—owns the endpoint.
			.attr('refX', 10)
			.attr('refY', 0)
			.attr('markerUnits', 'userSpaceOnUse')
			.attr('markerWidth', 10)
			.attr('markerHeight', 10)
			.attr('orient', 'auto')
			.append('path')
			.attr('d', 'M0,-5L10,0L0,5')
			.attr('fill', p.textMuted)
			.attr('opacity', 0.88);

		// One arrowhead marker per distinct edge color so heads match their line.
		const usedEdgeColors = new Map<string, string>();
		for (const l of links) {
			if (l.linkType !== 'semantic') continue;
			const c = edgeColor(l.edgeKind!);
			if (!usedEdgeColors.has(c)) usedEdgeColors.set(c, `tm-arrow-${usedEdgeColors.size}`);
		}
		for (const [color, id] of usedEdgeColors) {
			defs
				.append('marker')
				.attr('id', id)
				.attr('viewBox', '0 -5 10 10')
				.attr('refX', 10)
				.attr('refY', 0)
				.attr('markerUnits', 'userSpaceOnUse')
				.attr('markerWidth', 11)
				.attr('markerHeight', 11)
				.attr('orient', 'auto')
				.append('path')
				.attr('d', 'M0,-5L10,0L0,5')
				.attr('fill', color);
		}
		const arrowId = (l: CanvasLink) => usedEdgeColors.get(edgeColor(l.edgeKind!)) ?? '';

		const sim = d3
			.forceSimulation<CanvasNode>(nodes)
			.force(
				'link',
				d3
					.forceLink<CanvasNode, CanvasLink>(links)
					.id((d) => d.id)
					.distance((l) => (l.linkType === 'parent' ? 118 : 210))
					.strength((l) => (l.linkType === 'parent' ? 0.45 : 0.12))
			)
			.force('charge', d3.forceManyBody().strength(-1050).distanceMax(620))
			.force('center', d3.forceCenter(width / 2, height / 2))
			.force('x', d3.forceX(width / 2).strength(0.03))
			.force('y', d3.forceY(height / 2).strength(0.04))
			.force('collision', d3.forceCollide().radius(NODE_W / 1.4));
		simulation = sim;

		// ── Parent links (neutral hierarchy, readable at fit-to-view) ─────────────
		const parentLink = g
			.append('g')
			.selectAll('line.tm-parent-link')
			.data(links.filter((l) => l.linkType === 'parent'))
			.enter()
			.append('line')
			.attr('class', 'tm-parent-link')
			.attr('stroke', p.textMuted)
			.attr('stroke-width', 2)
			.attr('stroke-dasharray', '7,5')
			.attr('stroke-linecap', 'round')
			.attr('vector-effect', 'non-scaling-stroke')
			.attr('opacity', 0.78)
			.attr('marker-end', 'url(#tm-parent-arrow)');
		parentLink.append('title').text('Parent → child');

		// ── Semantic edges (arrowed, colored by kind, hover-highlight) ───────────
		const semanticData = links.filter((l) => l.linkType === 'semantic');

		const semanticLink = g
			.append('g')
			.selectAll('line.tm-edge')
			.data(semanticData)
			.enter()
			.append('line')
			.attr('class', 'tm-edge')
			.attr('data-edge-kind', (d) => d.edgeKind ?? 'related_to')
			.attr('stroke', (d) => edgeColor(d.edgeKind!))
			.attr('stroke-width', 2.5)
			.attr('stroke-dasharray', (d) => edgeDasharray(d.edgeKind!))
			.attr('stroke-linecap', 'round')
			.attr('vector-effect', 'non-scaling-stroke')
			.attr('opacity', 0.92)
			.attr('marker-end', (d) => `url(#${arrowId(d)})`)
			.style('cursor', 'pointer')
			.style('transition', 'stroke-width 120ms ease, opacity 120ms ease');
		semanticLink
			.append('title')
			.text((d) => (d.edgeKind ?? 'related_to').replace(/_/g, ' '));

		// A wide invisible hit area makes thin edges easy to hover.
		const edgeHit = g
			.append('g')
			.selectAll('line.tm-edge-hit')
			.data(semanticData)
			.enter()
			.append('line')
			.attr('class', 'tm-edge-hit')
			.attr('stroke', 'transparent')
			.attr('stroke-width', 14)
			.attr('vector-effect', 'non-scaling-stroke')
			.style('cursor', 'pointer');

		// Edge kind labels — hidden by default, revealed on hover / selection / zoom.
		const edgeLabels = g
			.append('g')
			.selectAll('text.tm-edge-label')
			.data(semanticData)
			.enter()
			.append('text')
			.attr('class', 'tm-edge-label')
			.attr('font-family', themeColor('--font-primary', 'sans-serif'))
			.attr('font-size', '10px')
			.attr('font-weight', '700')
			.attr('fill', (d) => edgeColor(d.edgeKind!))
			.attr('stroke', p.surface)
			.attr('stroke-width', 3)
			.attr('stroke-linejoin', 'round')
			.attr('paint-order', 'stroke')
			.attr('text-anchor', 'middle')
			.attr('opacity', 0)
			.style('pointer-events', 'none')
			.style('transition', 'opacity 120ms ease')
			.text((d) => (d.edgeKind ?? '').replace(/_/g, ' '));

		function edgeLabelBaseOpacity(): number {
			// Hidden by default to keep the graph calm — a label reveals only on
			// hover of its edge or when its edge touches the selected node
			// (handled in refreshEdgeLabels). Showing them all at once is noise.
			return 0;
		}

		function refreshEdgeLabels(hoverId?: string) {
			const base = edgeLabelBaseOpacity();
			edgeLabels.attr('opacity', (d) => {
				if (d.id === hoverId) return 1;
				const src = (d.source as CanvasNode).id;
				const tgt = (d.target as CanvasNode).id;
				if (selectedNodeId && (src === selectedNodeId || tgt === selectedNodeId)) return 1;
				return base;
			});
		}

		function highlightEdge(d: CanvasLink | null) {
			semanticLink
				.attr('stroke-width', (l) => (d && l.id === d.id ? 4 : 2.5))
				.attr('opacity', (l) => (d && l.id === d.id ? 1 : 0.92));
			refreshEdgeLabels(d?.id);
		}

		edgeHit
			.on('mouseenter', (_e, d) => highlightEdge(d))
			.on('mouseleave', () => highlightEdge(null));

		// ── Nodes ─────────────────────────────────────────────────────────────────
		const dragBehavior = d3
			.drag<SVGGElement, CanvasNode>()
			.on('start', (event, d) => {
				// Keep the sim calm — pin directly so the node tracks the cursor 1:1.
				if (!event.active) sim.alphaTarget(0).restart();
				d.fx = d.x;
				d.fy = d.y;
				// sourceEvent.currentTarget is NULL once the native event finishes
				// dispatching (and on the window-level mouseup) — resolve the node
				// group from the target instead, and no-op when absent.
				const grabbed = (event.sourceEvent?.target as Element | null)?.closest?.('g.tm-node');
				if (grabbed) d3.select(grabbed).style('cursor', 'grabbing');
				svg.style('cursor', 'grabbing');
			})
			.on('drag', (event, d) => {
				d.fx = event.x;
				d.fy = event.y;
			})
			.on('end', (event, d) => {
				if (!event.active) sim.alphaTarget(0);
				const nx = event.x;
				const ny = event.y;
				// Keep the node pinned at its dropped spot; mark it anchored so an
				// incoming poll (which will carry this position) doesn't fight it.
				d.fx = nx;
				d.fy = ny;
				d.anchored = true;
				const released = (event.sourceEvent?.target as Element | null)?.closest?.('g.tm-node');
				if (released) d3.select(released).style('cursor', 'grab');
				svg.style('cursor', 'grab');
				// Only persist on drag END, letting the PAGE own the move_node op.
				dispatch('moveNode', { node_id: d.id, x: nx, y: ny });
			});

		const node = g
			.append('g')
			.selectAll('g.tm-node')
			.data(nodes)
			.enter()
			.append('g')
			.attr('class', 'tm-node')
			.style('cursor', readOnly ? 'default' : 'grab')
			.on('click', (event: MouseEvent, d: CanvasNode) => {
				event.stopPropagation();
				dispatch('selectNode', { nodeId: d.id });
			})
			.on('mouseenter', function () {
				d3.select(this).select('rect.tm-node-card').attr('filter', 'url(#tm-shadow)');
				d3.select(this).raise();
			})
			.on('mouseleave', function (_e, d) {
				if (d.id !== selectedNodeId)
					d3.select(this).select('rect.tm-node-card').attr('filter', null);
			});

		if (!readOnly) node.call(dragBehavior);

		// Animated selection ring (drawn behind the card; opacity+scale eased).
		node
			.append('rect')
			.attr('class', 'tm-select-ring')
			.attr('x', -(NODE_W / 2) - 5)
			.attr('y', -(NODE_H / 2) - 5)
			.attr('width', NODE_W + 10)
			.attr('height', NODE_H + 10)
			.attr('rx', 14)
			.attr('ry', 14)
			.attr('fill', 'none')
			.attr('stroke', (d) => nodeColor(d.node.kind))
			.attr('stroke-width', 2.5)
			.attr('opacity', (d) => (d.id === selectedNodeId ? 1 : 0))
			.style('transform-box', 'fill-box')
			.style('transform-origin', 'center')
			.style('transform', (d) => (d.id === selectedNodeId ? 'scale(1)' : 'scale(0.9)'))
			.style('transition', 'opacity 180ms ease, transform 180ms cubic-bezier(0.34,1.56,0.64,1)');

		// Node card (rounded rect with soft elevation).
		node
			.append('rect')
			.attr('class', 'tm-node-card')
			.attr('x', -(NODE_W / 2))
			.attr('y', -(NODE_H / 2))
			.attr('width', NODE_W)
			.attr('height', NODE_H)
			.attr('rx', 11)
			.attr('ry', 11)
			.attr('fill', p.surface)
			.attr('stroke', (d) => nodeColor(d.node.kind))
			.attr('stroke-width', (d) =>
				isEmphasized(d.node) ? 2.25 : isProvisional(d.node) ? 1.25 : 1.5
			)
			.attr('stroke-dasharray', (d) => (isProvisional(d.node) ? '5,4' : '0'))
			.attr('opacity', (d) => nodeOpacity(d.node))
			.attr('filter', (d) => (isEmphasized(d.node) ? 'url(#tm-shadow)' : null));

		// Kind tag (small caps).
		node
			.append('text')
			.attr('x', -(NODE_W / 2) + 14)
			.attr('y', -(NODE_H / 2) + 15)
			.attr('font-family', themeColor('--font-primary', 'sans-serif'))
			.attr('font-size', '8px')
			.attr('font-weight', '700')
			.attr('letter-spacing', '0.06em')
			.attr('fill', (d) => nodeColor(d.node.kind))
			.attr('opacity', (d) => (isFaded(d.node) ? 0.6 : 0.9))
			.style('pointer-events', 'none')
			.text((d) => d.node.kind.toUpperCase());

		// Node label.
		node
			.append('text')
			.attr('x', -(NODE_W / 2) + 14)
			.attr('y', 9)
			.attr('font-family', themeColor('--font-primary', 'sans-serif'))
			.attr('font-size', '11px')
			.attr('font-weight', '600')
			.attr('fill', p.text)
			.attr('opacity', (d) => (isFaded(d.node) ? 0.65 : 1))
			.style('pointer-events', 'none')
			.text((d) => truncate(d.node.label, 22));

		// AI-suggested spark for provisional / model-inferred nodes.
		node
			.filter((d) => isProvisional(d.node))
			.append('text')
			.attr('x', NODE_W / 2 - 11)
			.attr('y', -(NODE_H / 2) + 15)
			.attr('font-size', '10px')
			.attr('text-anchor', 'end')
			.attr('fill', (d) => nodeColor(d.node.kind))
			.attr('opacity', 0.85)
			.style('pointer-events', 'none')
			.text('✦');

		// ── Entrance micro-interaction (Thought-Pulse-lite) ──────────────────────
		if (!reducedMotion) {
			node
				.filter((d) => d.fresh)
				.style('opacity', 0)
				.style('transform-box', 'fill-box')
				.attr('data-fresh', '1')
				.each(function () {
					// A spring pop-in per fresh node, staggered lightly for a "typing on".
					const sel = d3.select(this);
					sel
						.style('transition', 'opacity 320ms cubic-bezier(0.34,1.56,0.64,1)')
						.style('opacity', 1);
				});
		}

		// ── Tick ─────────────────────────────────────────────────────────────────
		function positionsFor(d: CanvasLink) {
			const source = d.source as CanvasNode;
			const target = d.target as CanvasNode;
			const sx = source.x ?? 0;
			const sy = source.y ?? 0;
			const tx = target.x ?? 0;
			const ty = target.y ?? 0;
			const dx = tx - sx;
			const dy = ty - sy;
			if (dx === 0 && dy === 0) {
				return { x1: sx, y1: sy, x2: tx, y2: ty };
			}

			// Intersect the center-to-center ray with each rectangular card. This
			// keeps the line and its marker tip physically attached to card edges at
			// every zoom; marker `refX` never has to approximate half a node width.
			const boundaryScale = Math.min(
				(NODE_W / 2) / Math.max(Math.abs(dx), Number.EPSILON),
				(NODE_H / 2) / Math.max(Math.abs(dy), Number.EPSILON)
			);
			return {
				x1: sx + dx * boundaryScale,
				y1: sy + dy * boundaryScale,
				x2: tx - dx * boundaryScale,
				y2: ty - dy * boundaryScale
			};
		}

		sim.on('tick', () => {
			parentLink
				.attr('x1', (d) => positionsFor(d).x1)
				.attr('y1', (d) => positionsFor(d).y1)
				.attr('x2', (d) => positionsFor(d).x2)
				.attr('y2', (d) => positionsFor(d).y2);

			semanticLink
				.attr('x1', (d) => positionsFor(d).x1)
				.attr('y1', (d) => positionsFor(d).y1)
				.attr('x2', (d) => positionsFor(d).x2)
				.attr('y2', (d) => positionsFor(d).y2);

			edgeHit
				.attr('x1', (d) => positionsFor(d).x1)
				.attr('y1', (d) => positionsFor(d).y1)
				.attr('x2', (d) => positionsFor(d).x2)
				.attr('y2', (d) => positionsFor(d).y2);

			edgeLabels
				.attr('x', (d) => (positionsFor(d).x1 + positionsFor(d).x2) / 2)
				.attr('y', (d) => (positionsFor(d).y1 + positionsFor(d).y2) / 2 - 4);

			node.attr('transform', (d) => `translate(${d.x ?? 0},${d.y ?? 0})`);
		});

		sim.on('end', () => {
			// Auto-fit ONCE per map (its first layout). Later re-renders (polls,
			// mutations, drags) keep the user's current viewport steady.
			if (fittedMapId !== map.map_id && !lastTransform) {
				fittedMapId = map.map_id;
				fitToView(nodes, width, height);
			}
			refreshEdgeLabels();
		});

		// Deselect on background click; keep labels honest on zoom change.
		svg.on('click', () => dispatch('selectNode', { nodeId: '' }));

		// Double-click zooms toward the cursor (⌥/⇧ zooms out).
		svg.on('dblclick', (event: MouseEvent) => {
			if (!svgElement || !zoomBehavior) return;
			event.preventDefault();
			const out = event.altKey || event.shiftKey;
			const factor = out ? 1 / 1.6 : 1.6;
			const [px, py] = d3.pointer(event, svg.node());
			svgElement
				.transition()
				.duration(reducedMotion ? 0 : 260)
				.ease(d3.easeCubicOut)
				.call(zoomBehavior.scaleBy as never, factor, [px, py]);
		});

		// Keep edge labels in sync with zoom level changes.
		zoom.on('zoom.labels', () => refreshEdgeLabels());

		refreshEdgeLabels();
	}

	/** Update only the selection ring/label emphasis without a full re-layout. */
	function applySelection() {
		if (!svgContainer) return;
		const sel = d3.select(svgContainer);
		sel
			.selectAll<SVGRectElement, CanvasNode>('rect.tm-select-ring')
			.attr('opacity', (d) => (d && d.id === selectedNodeId ? 1 : 0))
			.style('transform', (d) => (d && d.id === selectedNodeId ? 'scale(1)' : 'scale(0.9)'));
		// Emphasize the selected node's card shadow.
		sel.selectAll<SVGGElement, CanvasNode>('g.tm-node').each(function (d) {
			const emphasized = d && (d.id === selectedNodeId || isEmphasized(d.node));
			d3.select(this)
				.select('rect.tm-node-card')
				.attr('filter', emphasized ? 'url(#tm-shadow)' : null);
		});
	}

	function fitToView(nodes: CanvasNode[], width: number, height: number) {
		if (!svgElement || !zoomBehavior || nodes.length === 0) return;

		let minX = Infinity,
			minY = Infinity,
			maxX = -Infinity,
			maxY = -Infinity;
		for (const n of nodes) {
			const x = n.x ?? 0;
			const y = n.y ?? 0;
			if (x < minX) minX = x;
			if (x > maxX) maxX = x;
			if (y < minY) minY = y;
			if (y > maxY) maxY = y;
		}

		const pad = NODE_W;
		minX -= pad;
		minY -= pad;
		maxX += pad;
		maxY += pad;

		const bboxW = maxX - minX;
		const bboxH = maxY - minY;
		if (bboxW <= 0 || bboxH <= 0) return;

		const scale = Math.min(width / bboxW, height / bboxH, 1.4);
		const tx = (width - bboxW * scale) / 2 - minX * scale;
		const ty = (height - bboxH * scale) / 2 - minY * scale;

		const transform = d3.zoomIdentity.translate(tx, ty).scale(scale);
		// Interruptible, eased fit — a settle curve, no jarring linear snap.
		svgElement
			.transition('fit')
			.duration(reducedMotion ? 0 : 620)
			.ease(d3.easeCubicInOut)
			.call(zoomBehavior.transform as never, transform);
	}

	// ── Public-ish zoom controls (used by the toolbar in markup) ──────────────────
	export function fit() {
		const w = svgContainer?.clientWidth || 800;
		const h = svgContainer?.clientHeight || 450;
		if (simulation) {
			fitToView(simulation.nodes(), w, h);
		} else {
			const { nodes } = buildData();
			if (nodes.length) fitToView(nodes, w, h);
		}
	}

	function zoomBy(factor: number) {
		if (svgElement && zoomBehavior) {
			svgElement
				.transition()
				.duration(reducedMotion ? 0 : 240)
				.ease(d3.easeCubicOut)
				.call(zoomBehavior.scaleBy as never, factor);
		}
	}

	function panBy(dx: number, dy: number) {
		if (svgElement && zoomBehavior) {
			svgElement
				.transition()
				.duration(reducedMotion ? 0 : 180)
				.ease(d3.easeCubicOut)
				.call(zoomBehavior.translateBy as never, dx, dy);
		}
	}

	// ── Export (SVG / PNG / Markdown) ─────────────────────────────────────────────

	/** Sanitized `<map-title>` base for download filenames. */
	function exportFileBase(): string {
		const title = (map?.title ?? '').replace(/[\\/:*?"<>|]/g, '-').trim();
		return title || map?.map_id || 'thinking-map';
	}

	function downloadBlob(blob: Blob, filename: string) {
		const url = URL.createObjectURL(blob);
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = filename;
		document.body.appendChild(anchor);
		anchor.click();
		anchor.remove();
		setTimeout(() => URL.revokeObjectURL(url), 1_000);
	}

	const SVG_XMLNS = 'http://www.w3.org/2000/svg';

	/**
	 * Serialize the LIVE rendered svg into standalone markup. The palette
	 * already inlines most theme colors as concrete values at render time; a
	 * `var(...)` can only survive through a `themeColor` fallback, so the clone
	 * pass resolves any color attribute still expressed as a CSS var against
	 * the live element's computed style. A themed opaque background rect is
	 * prepended so the file matches the canvas surface outside the app.
	 * Exports exactly what's rendered (current pan/zoom included) — the
	 * provisional/superseded inclusion toggles are a Markdown-export concern.
	 */
	function standaloneSvgMarkup(): { markup: string; width: number; height: number } | null {
		const live = svgContainer?.querySelector('svg');
		if (!live) return null;
		const width = svgContainer.clientWidth || 800;
		const height = svgContainer.clientHeight || 450;
		const clone = live.cloneNode(true) as SVGSVGElement;
		clone.setAttribute('xmlns', SVG_XMLNS);
		clone.setAttribute('width', String(width));
		clone.setAttribute('height', String(height));

		const liveEls: Element[] = [live, ...Array.from(live.querySelectorAll('*'))];
		const cloneEls: Element[] = [clone, ...Array.from(clone.querySelectorAll('*'))];
		const COLOR_ATTRS = ['fill', 'stroke', 'flood-color', 'stop-color'];
		for (let i = 0; i < cloneEls.length && i < liveEls.length; i++) {
			for (const attr of COLOR_ATTRS) {
				const value = cloneEls[i].getAttribute(attr);
				if (value && value.includes('var(')) {
					const computed = getComputedStyle(liveEls[i]).getPropertyValue(attr);
					if (computed && computed.trim()) cloneEls[i].setAttribute(attr, computed.trim());
				}
			}
		}

		const background = document.createElementNS(SVG_XMLNS, 'rect');
		background.setAttribute('x', '0');
		background.setAttribute('y', '0');
		background.setAttribute('width', String(width));
		background.setAttribute('height', String(height));
		background.setAttribute('fill', themeColor('--bg-card', '#ffffff'));
		clone.insertBefore(background, clone.firstChild);

		return { markup: new XMLSerializer().serializeToString(clone), width, height };
	}

	function exportSvg() {
		const standalone = standaloneSvgMarkup();
		if (!standalone) return;
		downloadBlob(
			new Blob([standalone.markup], { type: 'image/svg+xml;charset=utf-8' }),
			`${exportFileBase()}.svg`
		);
		exportOpen = false;
	}

	/** Rasterize the standalone SVG at 2× into an offscreen canvas → PNG. */
	async function exportPng() {
		const standalone = standaloneSvgMarkup();
		if (!standalone) return;
		exportBusy = 'png';
		const url = URL.createObjectURL(
			new Blob([standalone.markup], { type: 'image/svg+xml;charset=utf-8' })
		);
		try {
			const image = new Image();
			await new Promise<void>((resolve, reject) => {
				image.onload = () => resolve();
				image.onerror = () => reject(new Error('SVG rasterization failed'));
				image.src = url;
			});
			const scale = 2; // crisp on high-DPI displays
			const canvas = document.createElement('canvas');
			canvas.width = standalone.width * scale;
			canvas.height = standalone.height * scale;
			const ctx = canvas.getContext('2d');
			if (!ctx) return;
			ctx.scale(scale, scale);
			ctx.drawImage(image, 0, 0, standalone.width, standalone.height);
			const blob = await new Promise<Blob | null>((resolve) =>
				canvas.toBlob(resolve, 'image/png')
			);
			if (blob) downloadBlob(blob, `${exportFileBase()}.png`);
			exportOpen = false;
		} catch (error) {
			console.error('Thinking map PNG export failed', error);
		} finally {
			URL.revokeObjectURL(url);
			exportBusy = null;
		}
	}

	/** Markdown via the backend export endpoint, honoring the two toggles. */
	async function exportMd() {
		if (!map) return;
		exportBusy = 'md';
		try {
			const markdown = await exportMarkdown(map.map_id, {
				includeProvisional: exportIncludeProvisional,
				includeSuperseded: exportIncludeSuperseded
			});
			downloadBlob(
				new Blob([markdown], { type: 'text/markdown;charset=utf-8' }),
				`${exportFileBase()}.md`
			);
			exportOpen = false;
		} catch (error) {
			console.error('Thinking map Markdown export failed', error);
		} finally {
			exportBusy = null;
		}
	}

	/** Close the export dropdown on any click outside it. */
	function handleWindowClick(event: MouseEvent) {
		if (!exportOpen) return;
		if (exportWrap && event.target instanceof Node && exportWrap.contains(event.target)) return;
		exportOpen = false;
	}

	// ── Keyboard (only when the canvas has focus, never over a text input) ────────
	function onKeydown(event: KeyboardEvent) {
		const target = event.target as HTMLElement | null;
		if (target) {
			const tag = target.tagName;
			if (
				tag === 'INPUT' ||
				tag === 'TEXTAREA' ||
				tag === 'SELECT' ||
				target.isContentEditable
			) {
				return;
			}
		}
		const PAN = 80;
		switch (event.key) {
			case '+':
			case '=':
				event.preventDefault();
				zoomBy(1.3);
				break;
			case '-':
			case '_':
				event.preventDefault();
				zoomBy(1 / 1.3);
				break;
			case '0':
			case 'f':
			case 'F':
				event.preventDefault();
				fit();
				break;
			case 'ArrowUp':
				event.preventDefault();
				panBy(0, PAN);
				break;
			case 'ArrowDown':
				event.preventDefault();
				panBy(0, -PAN);
				break;
			case 'ArrowLeft':
				event.preventDefault();
				panBy(PAN, 0);
				break;
			case 'ArrowRight':
				event.preventDefault();
				panBy(-PAN, 0);
				break;
			case 'Escape':
				if (exportOpen) {
					event.preventDefault();
					exportOpen = false;
				} else if (selectedNodeId) {
					event.preventDefault();
					dispatch('selectNode', { nodeId: '' });
				}
				break;
		}
	}

	// ── Lifecycle + reactivity ────────────────────────────────────────────────────
	let mounted = false;
	let resizeObserver: ResizeObserver | null = null;

	onMount(() => {
		mounted = true;
		render();
		// Keep the viewBox honest when the surface resizes (rail collapse, window).
		if (typeof ResizeObserver !== 'undefined' && svgContainer) {
			let raf = 0;
			resizeObserver = new ResizeObserver(() => {
				if (!svgElement) return;
				cancelAnimationFrame(raf);
				raf = requestAnimationFrame(() => {
					const w = svgContainer.clientWidth || 800;
					const h = svgContainer.clientHeight || 450;
					svgElement?.attr('viewBox', `0 0 ${w} ${h}`);
				});
			});
			resizeObserver.observe(svgContainer);
		}
	});

	onDestroy(() => {
		simulation?.stop();
		simulation = null;
		resizeObserver?.disconnect();
		resizeObserver = null;
	});

	// Re-render when the map document identity/revision or app theme changes.
	// D3 resolves theme tokens into concrete SVG attributes, so CSS inheritance
	// alone cannot recolor an already-rendered graph.
	let lastKey = '';
	let lastMapId = '';
	$: {
		// readOnly is part of the key: entering history at the live revision must
		// still re-render to detach the drag behavior.
		const key = map
			? `${map.map_id}:${map.revision}:${readOnly ? 'ro' : 'rw'}:${$themeStore}`
			: '';
		if (mounted && key !== lastKey) {
			// Switching to a different map → forget the prior viewport so the new
			// map fits fresh (and its nodes animate in as first-seen).
			if (map && map.map_id !== lastMapId) {
				lastMapId = map.map_id;
				lastTransform = null;
				fittedMapId = null;
				seenNodeIds = new Set();
			}
			lastKey = key;
			render();
		}
	}

	// Selection changes only tweak the ring — no full re-layout.
	$: if (mounted) {
		selectedNodeId;
		applySelection();
	}

	$: liveNodeCount = map ? Object.values(map.nodes ?? {}).filter((n) => !n.tombstoned).length : 0;
</script>

<svelte:window on:click={handleWindowClick} />

<!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
<div
	class="tm-canvas"
	role="application"
	aria-label="Thinking map canvas"
	tabindex="0"
	on:keydown={onKeydown}
>
	<div class="tm-controls" role="toolbar" aria-label="Canvas controls">
		<button type="button" class="tm-ctrl" title="Zoom in (+)" on:click={() => zoomBy(1.3)}>
			<span aria-hidden="true">+</span>
		</button>
		<div class="tm-zoom-readout" title="Current zoom">{zoomPct}%</div>
		<button type="button" class="tm-ctrl" title="Zoom out (−)" on:click={() => zoomBy(1 / 1.3)}>
			<span aria-hidden="true">−</span>
		</button>
		<button type="button" class="tm-ctrl tm-ctrl--fit" title="Fit to view (f)" on:click={fit}>
			Fit
		</button>
		<div class="tm-export" bind:this={exportWrap}>
			<button
				type="button"
				class="tm-ctrl tm-ctrl--fit"
				title="Export this map"
				aria-haspopup="true"
				aria-expanded={exportOpen}
				on:click={() => (exportOpen = !exportOpen)}
			>
				Export
			</button>
			{#if exportOpen}
				<div class="tm-export-menu" role="group" aria-label="Export options">
					<button
						type="button"
						class="tm-export-item"
						disabled={exportBusy !== null}
						on:click={exportSvg}
					>
						SVG <span class="tm-export-item__hint">vector · as rendered</span>
					</button>
					<button
						type="button"
						class="tm-export-item"
						disabled={exportBusy !== null}
						on:click={exportPng}
					>
						PNG <span class="tm-export-item__hint">2× raster · as rendered</span>
					</button>
					<button
						type="button"
						class="tm-export-item"
						disabled={exportBusy !== null}
						on:click={exportMd}
					>
						Markdown <span class="tm-export-item__hint">outline document</span>
					</button>
					<div class="tm-export-opts">
						<label class="tm-export-opt">
							<input type="checkbox" bind:checked={exportIncludeProvisional} />
							Include provisional
						</label>
						<label class="tm-export-opt">
							<input type="checkbox" bind:checked={exportIncludeSuperseded} />
							Include superseded
						</label>
						<div class="tm-export-note">
							Options apply to Markdown. SVG/PNG export what's rendered.
						</div>
					</div>
				</div>
			{/if}
		</div>
	</div>

	<div class="tm-surface" bind:this={svgContainer}></div>

	{#if liveNodeCount === 0}
		<div class="tm-empty">
			<div class="tm-empty__icon" aria-hidden="true">✦</div>
			<div class="tm-empty__title">Nothing on the map yet</div>
			<div class="tm-empty__hint">
				Capture a thought or interpret an utterance to grow this map.
			</div>
		</div>
	{/if}
</div>

<style>
	.tm-canvas {
		position: relative;
		width: 100%;
		height: 100%;
		min-height: 320px;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background:
			radial-gradient(
				circle at 1px 1px,
				color-mix(in srgb, var(--text-primary) 6%, transparent) 1px,
				transparent 0
			);
		background-color: var(--bg-card);
		background-size: 22px 22px;
		box-shadow: var(--shadow-sm);
		overflow: hidden;
		transition: border-color var(--transition-base, 0.25s ease), box-shadow var(--transition-base, 0.25s ease);
	}

	.tm-canvas:focus-visible {
		outline: none;
		border-color: color-mix(in srgb, var(--accent-primary, currentColor) 45%, var(--border-soft));
		box-shadow: var(--shadow-glow, 0 0 0 3px color-mix(in srgb, var(--accent-primary) 20%, transparent));
	}

	.tm-surface {
		width: 100%;
		height: 100%;
	}

	.tm-controls {
		position: absolute;
		top: var(--space-sm, 0.6rem);
		right: var(--space-sm, 0.6rem);
		z-index: 2;
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.25rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--bg-elevated, var(--bg-card)) 88%, transparent);
		box-shadow: var(--shadow-md);
		backdrop-filter: blur(8px);
	}

	.tm-ctrl {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 1.85rem;
		height: 1.85rem;
		padding: 0 0.45rem;
		border: none;
		border-radius: var(--radius-full, 999px);
		background: transparent;
		color: var(--text-secondary);
		font-family: var(--font-primary);
		font-size: 0.95rem;
		font-weight: 700;
		line-height: 1;
		cursor: pointer;
		transition: background var(--transition-fast, 0.15s ease), color var(--transition-fast, 0.15s ease), transform var(--transition-fast, 0.15s ease);
	}

	.tm-ctrl--fit {
		font-size: 0.74rem;
		letter-spacing: 0.02em;
		padding: 0 0.6rem;
	}

	.tm-ctrl:hover {
		background: color-mix(in srgb, var(--accent-primary, currentColor) 14%, transparent);
		color: var(--accent-primary);
	}

	.tm-ctrl:active {
		transform: scale(0.92);
	}

	.tm-ctrl:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary, currentColor) 55%, transparent);
		outline-offset: 1px;
	}

	.tm-export {
		position: relative;
		display: inline-flex;
	}

	.tm-export-menu {
		position: absolute;
		top: calc(100% + 0.5rem);
		right: 0;
		z-index: 3;
		min-width: 14rem;
		padding: 0.35rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-elevated, var(--bg-card));
		box-shadow: var(--shadow-lg, var(--shadow-md));
	}

	.tm-export-item {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.6rem;
		width: 100%;
		padding: 0.4rem 0.55rem;
		border: none;
		border-radius: var(--radius-sm, 6px);
		background: transparent;
		color: var(--text-primary);
		font-family: var(--font-primary);
		font-size: 0.78rem;
		font-weight: 600;
		text-align: left;
		cursor: pointer;
		transition:
			background var(--transition-fast, 0.15s ease),
			color var(--transition-fast, 0.15s ease);
	}

	.tm-export-item:hover:not(:disabled) {
		background: color-mix(in srgb, var(--accent-primary, currentColor) 12%, transparent);
		color: var(--accent-primary);
	}

	.tm-export-item:disabled {
		opacity: 0.55;
		cursor: progress;
	}

	.tm-export-item__hint {
		font-size: 0.65rem;
		font-weight: 500;
		color: var(--text-muted);
		white-space: nowrap;
	}

	.tm-export-opts {
		display: grid;
		gap: 0.3rem;
		margin-top: 0.3rem;
		padding: 0.45rem 0.55rem 0.35rem;
		border-top: 1px solid var(--border-soft);
	}

	.tm-export-opt {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-family: var(--font-primary);
		font-size: 0.72rem;
		color: var(--text-secondary);
		cursor: pointer;
		user-select: none;
	}

	.tm-export-opt input {
		accent-color: var(--accent-primary);
	}

	.tm-export-note {
		font-family: var(--font-primary);
		font-size: 0.65rem;
		line-height: 1.4;
		color: var(--text-muted);
	}

	.tm-zoom-readout {
		min-width: 2.7rem;
		text-align: center;
		font-family: var(--font-mono, monospace);
		font-size: 0.7rem;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		color: var(--text-muted);
		user-select: none;
	}

	.tm-empty {
		position: absolute;
		inset: 0;
		display: grid;
		place-content: center;
		justify-items: center;
		gap: 0.4rem;
		pointer-events: none;
		text-align: center;
		padding: 1.5rem;
	}

	.tm-empty__icon {
		display: grid;
		place-items: center;
		width: 3rem;
		height: 3rem;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--accent-primary) 12%, transparent);
		font-size: 1.4rem;
		color: var(--accent-primary);
		margin-bottom: 0.35rem;
	}

	.tm-empty__title {
		font-family: var(--font-display, var(--font-primary));
		font-weight: 700;
		font-size: 0.95rem;
		color: var(--text-primary);
	}

	.tm-empty__hint {
		font-family: var(--font-primary);
		font-size: 0.8rem;
		color: var(--text-secondary);
		max-width: 22rem;
		line-height: 1.5;
	}
</style>
