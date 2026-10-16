import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it, vi } from 'vitest';

import Graph from './Graph.svelte';

const diamondNodes = [
	{ id: 'root', label: 'Root' },
	{ id: 'left', label: 'Left' },
	{ id: 'right', label: 'Right' },
	{ id: 'sink', label: 'Sink' }
];
const diamondEdges = [
	{ from: 'root', to: 'left' },
	{ from: 'root', to: 'right' },
	{ from: 'left', to: 'sink' },
	{ from: 'right', to: 'sink' }
];

describe('Graph hostile-input bounds and deterministic layouts', () => {
	it('skips malformed nodes and dangling edges, and tiers the graph deterministically', () => {
		render(Graph, {
			props: {
				nodes: [...diamondNodes, null, { id: 'root', label: 'Duplicate id' }],
				edges: [...diamondEdges, { from: 'ghost', to: 'sink' }, 'not-an-object']
			}
		});

		expect(screen.getByText('Root')).toBeInTheDocument();
		expect(screen.queryByText('Duplicate id')).not.toBeInTheDocument();
		expect(screen.getByText('Sink')).toBeInTheDocument();

		const tierOf = (id: string) =>
			document.querySelector(`[data-node-id="${id}"]`)?.getAttribute('data-tier');
		expect(tierOf('root')).toBe('0');
		expect(tierOf('left')).toBe('1');
		expect(tierOf('right')).toBe('1');
		expect(tierOf('sink')).toBe('2');
	});

	it('cycles land every member in one shared final tier', () => {
		render(Graph, {
			props: {
				nodes: [
					{ id: 'entry', label: 'Entry' },
					{ id: 'loopA', label: 'Loop A' },
					{ id: 'loopB', label: 'Loop B' }
				],
				edges: [
					{ from: 'entry', to: 'loopA' },
					{ from: 'loopA', to: 'loopB' },
					{ from: 'loopB', to: 'loopA' }
				]
			}
		});

		const tierOf = (id: string) =>
			document.querySelector(`[data-node-id="${id}"]`)?.getAttribute('data-tier');
		expect(tierOf('entry')).toBe('0');
		expect(tierOf('loopA')).toBe('1');
		expect(tierOf('loopB')).toBe('1');
	});

	it('selects a node on click, shows bounded metadata, and emits the node id', async () => {
		const selected = vi.fn();
		render(Graph, {
			props: {
				nodes: [
					{
						id: 'record_1',
						label: 'One',
						kind: 'task',
						metadata: { status: 'active', attempts: 2 }
					}
				],
				edges: []
			},
			events: { select: selected }
		});

		await fireEvent.click(screen.getByText('One'));
		expect(selected).toHaveBeenCalledTimes(1);
		expect(selected.mock.calls[0][0].detail).toEqual({ nodeId: 'record_1' });

		const detail = screen.getByTestId('muij-graph-detail');
		expect(detail).toHaveTextContent('One');
		expect(detail).toHaveTextContent('status');
		expect(detail).toHaveTextContent('active');
		expect(detail).toHaveTextContent('attempts');
	});

	it('focus_node_id selects the focused node before any interaction', () => {
		render(Graph, {
			props: {
				nodes: diamondNodes,
				edges: diamondEdges,
				focusNodeId: 'sink'
			}
		});

		const detail = screen.getByTestId('muij-graph-detail');
		expect(detail).toHaveTextContent('Sink');
		expect(
			document.querySelector('[data-node-id="sink"]')?.classList.contains('muij-graph-node-selected')
		).toBe(true);
	});

	it('list layout renders a vertical stack with an adjacency summary', () => {
		render(Graph, {
			props: {
				nodes: [
					{ id: 'a', label: 'Alpha' },
					{ id: 'b', label: 'Beta' }
				],
				edges: [{ from: 'a', to: 'b', label: 'next' }],
				layout: 'list'
			}
		});

		expect(screen.getByText('Alpha')).toBeInTheDocument();
		expect(screen.getByText('Beta')).toBeInTheDocument();
		expect(screen.getByText('→ Beta')).toBeInTheDocument();
	});

	it('reveal_order staggers node animation in sequence', () => {
		render(Graph, {
			props: {
				nodes: diamondNodes,
				edges: diamondEdges,
				revealOrder: ['sink', 'root', 'left', 'right']
			}
		});

		const delayOf = (id: string) =>
			document.querySelector(`[data-node-id="${id}"]`)?.getAttribute('style');
		expect(delayOf('sink')).toContain('animation-delay: 0ms');
		expect(delayOf('root')).toContain('animation-delay: 120ms');
		expect(delayOf('left')).toContain('animation-delay: 240ms');
		expect(delayOf('right')).toContain('animation-delay: 360ms');
	});

	it('empty graphs render the shared empty state', () => {
		render(Graph, { props: { nodes: [], edges: [] } });
		expect(screen.getByText('No items')).toBeInTheDocument();
	});

	it('compares ids exactly so Rust-valid spaced ids stay connected', () => {
		// Ids compare exactly like the Rust validator (no trim): a node id
		// with surrounding spaces and an edge naming that same untrimmed id
		// is a Rust-valid document and must render connected.
		render(Graph, {
			props: {
				nodes: [
					{ id: ' a', label: 'Alpha' },
					{ id: 'b', label: 'Beta' }
				],
				edges: [{ from: ' a', to: 'b' }],
				layout: 'list'
			}
		});

		expect(document.querySelector('[data-node-id=" a"]')).not.toBeNull();
		expect(screen.getByText('→ Beta')).toBeInTheDocument();
	});

	it('drops nodes whose ids exceed the 128-character cap', () => {
		const overlongId = 'a'.repeat(129);
		render(Graph, {
			props: {
				nodes: [
					{ id: overlongId, label: 'Overlong' },
					{ id: 'keep', label: 'Keep' }
				],
				edges: [{ from: overlongId, to: 'keep' }]
			}
		});

		expect(screen.queryByText('Overlong')).not.toBeInTheDocument();
		expect(screen.getByText('Keep')).toBeInTheDocument();
		expect(document.querySelectorAll('.muij-graph-node')).toHaveLength(1);
	});
});
