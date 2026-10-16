import { describe, expect, it } from 'vitest';

import { treeLayout, type TreeLayoutNode } from './treeLayout';

const SPACING = { nodeW: 138, nodeH: 50 };

function n(id: string, parent?: string, minute = 0): TreeLayoutNode {
	return {
		node_id: id,
		...(parent ? { parent_id: parent } : {}),
		created_at: `2026-10-06T00:${String(minute).padStart(2, '0')}:00Z`
	};
}

describe('treeLayout', () => {
	it('puts children below their parent in top-down, ordered by creation', () => {
		const at = treeLayout(
			[n('c2', 'root', 2), n('root', undefined, 0), n('c1', 'root', 1), n('g', 'c1', 3)],
			'top-down',
			SPACING
		);
		const root = at.get('root')!;
		const c1 = at.get('c1')!;
		const c2 = at.get('c2')!;
		const g = at.get('g')!;
		expect(c1.y).toBeGreaterThan(root.y);
		expect(c2.y).toBe(c1.y);
		expect(g.y).toBeGreaterThan(c1.y);
		// Earlier sibling on the left; the parent centred over its children.
		expect(c1.x).toBeLessThan(c2.x);
		expect(root.x).toBeCloseTo((c1.x + c2.x) / 2);
		// Cards in a band never overlap.
		expect(c2.x - c1.x).toBeGreaterThanOrEqual(SPACING.nodeW);
	});

	it('puts children to the right of their parent in left-right', () => {
		const at = treeLayout([n('root'), n('a', 'root', 1), n('b', 'root', 2)], 'left-right', SPACING);
		const root = at.get('root')!;
		expect(at.get('a')!.x).toBeGreaterThan(root.x);
		expect(at.get('a')!.x).toBe(at.get('b')!.x);
		expect(at.get('a')!.y).toBeLessThan(at.get('b')!.y);
		expect(at.get('b')!.y - at.get('a')!.y).toBeGreaterThanOrEqual(SPACING.nodeH);
	});

	it('lays a forest of roots out on one band', () => {
		const at = treeLayout(
			[n('a', undefined, 0), n('b', undefined, 1), n('c', undefined, 2)],
			'top-down',
			SPACING
		);
		const ys = new Set([...at.values()].map((p) => p.y));
		expect(ys.size).toBe(1);
		expect(at.size).toBe(3);
	});

	it('treats a missing parent as a root and survives a cycle', () => {
		const at = treeLayout(
			[n('orphan', 'gone', 0), n('x', 'y', 1), n('y', 'x', 2), n('z', 'x', 3)],
			'top-down',
			SPACING
		);
		expect(at.size).toBe(4);
		for (const p of at.values()) {
			expect(Number.isFinite(p.x)).toBe(true);
			expect(Number.isFinite(p.y)).toBe(true);
		}
		expect(at.get('orphan')!.y).toBe(at.get('x')!.y);
	});

	it('is deterministic regardless of input order', () => {
		const nodes = [n('r'), n('a', 'r', 1), n('b', 'r', 2), n('c', 'a', 3)];
		const forward = treeLayout(nodes, 'top-down', SPACING);
		const reversed = treeLayout([...nodes].reverse(), 'top-down', SPACING);
		expect([...reversed.entries()].sort()).toEqual([...forward.entries()].sort());
	});

	it('never places a node by its semantic edges', () => {
		// Flat by parent_id: the tree is authored, not guessed from links.
		const at = treeLayout(
			[n('hub', undefined, 0), n('a', undefined, 1), n('b', undefined, 2)],
			'top-down',
			SPACING
		);
		expect(new Set([...at.values()].map((p) => p.y)).size).toBe(1);
	});
});
