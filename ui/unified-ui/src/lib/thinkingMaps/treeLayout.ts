/**
 * Deterministic tree layout for the Thinking Map canvas.
 *
 * The tree is `parent_id` and nothing else — the same field iOS
 * (`ThinkingGraphLayout`) and Android (`graphLayout`) read, so one map is one
 * shape on every client. Depth decides the band, creation order decides the
 * place within it, and each parent sits centred over its own children. Semantic
 * edges are drawn but never place anything: the interpreter sets `parent` when
 * it adds a node, and Reorganize can re-home old flat nodes, so the tree is
 * authored rather than guessed from edges here. A force simulation used to
 * place nodes, which flared them out from the centre in any direction.
 *
 * Stored `position`s are not consulted: both mobile clients ignore them, and
 * positions saved under the old force layout would scatter the tree again.
 *
 * Pure, so the placement is testable without an SVG — including the cases that
 * break naive trees: a cycle, a missing parent, a forest of roots.
 */
import { hierarchy, tree } from 'd3';

export type TreeOrientation = 'top-down' | 'left-right';

export interface TreeLayoutNode {
	node_id: string;
	parent_id?: string;
	created_at: string;
}

export interface TreeLayoutSpacing {
	/** Card width and height; gaps are added on top. */
	nodeW: number;
	nodeH: number;
}

export interface Point {
	x: number;
	y: number;
}

interface Branch {
	id: string;
	children: Branch[];
}

const VIRTUAL_ROOT = '\u0000root';

function byCreation(a: TreeLayoutNode, b: TreeLayoutNode): number {
	if (a.created_at !== b.created_at) return a.created_at < b.created_at ? -1 : 1;
	return a.node_id < b.node_id ? -1 : a.node_id > b.node_id ? 1 : 0;
}

/**
 * Each node's effective parent: its `parent_id` when that node is present,
 * with one link of every cycle dropped so the cycle hangs off a root instead
 * of making the tree infinite.
 */
function effectiveParents(ordered: TreeLayoutNode[]): Map<string, string | undefined> {
	const ids = new Set(ordered.map((n) => n.node_id));
	const parent = new Map<string, string | undefined>();
	for (const n of ordered) {
		parent.set(n.node_id, n.parent_id && ids.has(n.parent_id) ? n.parent_id : undefined);
	}
	for (const n of ordered) {
		const seen = new Set<string>([n.node_id]);
		let cursor = parent.get(n.node_id);
		while (cursor !== undefined) {
			if (cursor === n.node_id) {
				parent.set(n.node_id, undefined);
				break;
			}
			// A cycle further up that does not include `n`; its own member breaks it.
			if (seen.has(cursor)) break;
			seen.add(cursor);
			cursor = parent.get(cursor);
		}
	}
	return parent;
}

/** Centre point of every node, keyed by `node_id`, with the first root at the origin band. */
export function treeLayout(
	nodes: TreeLayoutNode[],
	orientation: TreeOrientation,
	spacing: TreeLayoutSpacing
): Map<string, Point> {
	const out = new Map<string, Point>();
	if (nodes.length === 0) return out;

	const ordered = [...nodes].sort(byCreation);
	const parent = effectiveParents(ordered);

	const branches = new Map<string, Branch>();
	for (const n of ordered) branches.set(n.node_id, { id: n.node_id, children: [] });
	// A virtual root joins the forest into one tree so sibling roots share a band.
	const root: Branch = { id: VIRTUAL_ROOT, children: [] };
	for (const n of ordered) {
		const p = parent.get(n.node_id);
		(p ? branches.get(p)! : root).children.push(branches.get(n.node_id)!);
	}

	const topDown = orientation === 'top-down';
	// Breadth runs across a band, depth runs between bands.
	const breadthGap = topDown ? spacing.nodeW + 34 : spacing.nodeH + 22;
	const depthGap = topDown ? spacing.nodeH + 80 : spacing.nodeW + 90;

	const laidOut = tree<Branch>()
		.nodeSize([breadthGap, depthGap])
		// Keep separate subtrees visibly apart, siblings tight.
		.separation((a, b) => (a.parent === b.parent ? 1 : 1.3))(hierarchy(root));

	for (const h of laidOut.descendants()) {
		if (h.data.id === VIRTUAL_ROOT) continue;
		const breadth = h.x;
		const depth = h.y - depthGap; // real roots sit at depth 0, not under the virtual root
		out.set(h.data.id, topDown ? { x: breadth, y: depth } : { x: depth, y: breadth });
	}
	return out;
}
