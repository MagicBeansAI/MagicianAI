import { cleanup, render, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import type {
	EdgeKind,
	ThinkingEdge,
	ThinkingMap,
	ThinkingNode
} from '$lib/types/thinkingMap';
import { themeStore } from '$lib/shared/stores/themeStore';
import ThinkingMapCanvas from './ThinkingMapCanvas.svelte';

const NOW = '2026-07-25T00:00:00Z';
const THEME_TOKENS: Record<string, string> = {
	'--text-primary': '#1a1612',
	'--text-muted': '#6a5944',
	'--bg-card': '#faf3e0',
	'--bg-soft': '#f4ead0',
	'--border-default': 'rgba(26, 22, 18, 0.26)',
	'--border-soft': 'rgba(26, 22, 18, 0.14)',
	'--accent-primary': '#a04020',
	'--accent-secondary': '#4a6a3a',
	'--color-success': '#287a4b',
	'--color-warning': '#a56f00',
	'--color-error': '#a4262c',
	'--color-info': '#356b9a'
};

const EDGE_EXPECTATIONS: Record<EdgeKind, { color: string; dash: string | null }> = {
	related_to: { color: '#6a5944', dash: '3,5' },
	supports: { color: '#287a4b', dash: null },
	contradicts: { color: '#a4262c', dash: '9,4' },
	answers: { color: '#356b9a', dash: null },
	depends_on: { color: '#a56f00', dash: '2,4' },
	leads_to: { color: '#a04020', dash: null },
	alternative_to: { color: '#a56f00', dash: '5,4' },
	measures: { color: '#356b9a', dash: '2,4' },
	grouped_under: { color: '#4a6a3a', dash: '7,4' }
};

function node(id: string, index: number, parentId?: string): ThinkingNode {
	return {
		node_id: id,
		kind: 'idea',
		label: `Node ${id}`,
		epistemic_state: 'asserted',
		assertion_origin: 'owner_spoken',
		confidence: 0.9,
		source_refs: [],
		...(parentId ? { parent_id: parentId } : {}),
		position: { x: 80 + index * 90, y: 80 + (index % 3) * 90 },
		position_locked: false,
		promoted_refs: [],
		tombstoned: false,
		created_at: NOW,
		updated_at: NOW
	};
}

function edge(kind: EdgeKind, index: number): ThinkingEdge {
	return {
		edge_id: `edge-${kind}`,
		from_node: 'root',
		to_node: `semantic-${index}`,
		kind,
		assertion_origin: 'owner_spoken',
		tombstoned: false,
		created_at: NOW,
		updated_at: NOW
	};
}

function fixture(): ThinkingMap {
	const kinds = Object.keys(EDGE_EXPECTATIONS) as EdgeKind[];
	const nodes = [node('root', 0), node('child', 1, 'root')];
	const edges = kinds.map((kind, index) => {
		nodes.push(node(`semantic-${index}`, index + 2));
		return edge(kind, index);
	});
	return {
		schema_version: 1,
		map_id: 'edge-contrast-map',
		principal: 'anonymous',
		workspace: 'default',
		title: 'Edge contrast',
		source: { kind: 'solo' },
		lifecycle: 'active',
		revision: 1,
		view_state: { lens: 'graph' },
		nodes: Object.fromEntries(nodes.map((item) => [item.node_id, item])),
		edges: Object.fromEntries(edges.map((item) => [item.edge_id, item])),
		clarifications: {},
		proposals: {},
		applied_envelopes: [],
		created_at: NOW,
		updated_at: NOW
	};
}

afterEach(() => {
	cleanup();
	themeStore.applyRemoteTheme('longhand');
	for (const token of Object.keys(THEME_TOKENS)) {
		document.documentElement.style.removeProperty(token);
	}
});

describe('ThinkingMapCanvas edge contrast and semantics', () => {
	it('keeps hierarchy and semantic strokes legible independently of canvas zoom', async () => {
		for (const [token, value] of Object.entries(THEME_TOKENS)) {
			document.documentElement.style.setProperty(token, value);
		}
		render(ThinkingMapCanvas, { map: fixture() });

		await waitFor(() => {
			expect(document.querySelectorAll('line.tm-edge')).toHaveLength(9);
		});
		const redundantAccentBars = Array.from(
			document.querySelectorAll('g.tm-node > rect')
		).filter(
			(element) =>
				element.getAttribute('width') === '4' &&
				element.getAttribute('height') === '38'
		);
		expect(redundantAccentBars).toHaveLength(0);

		const parent = document.querySelector('line.tm-parent-link');
		expect(parent).not.toBeNull();
		expect(parent).toHaveAttribute('stroke', '#6a5944');
		expect(parent).toHaveAttribute('stroke-width', '2');
		expect(parent).toHaveAttribute('stroke-dasharray', '7,5');
		expect(parent).toHaveAttribute('stroke-linecap', 'round');
		expect(parent).toHaveAttribute('vector-effect', 'non-scaling-stroke');
		expect(parent).toHaveAttribute('opacity', '0.78');
		expect(parent).toHaveAttribute('marker-end', 'url(#tm-parent-arrow)');
		expect(parent?.querySelector('title')).toHaveTextContent('Parent → child');
		const parentMarker = document.querySelector('marker#tm-parent-arrow');
		expect(parentMarker).toHaveAttribute('refX', '10');
		expect(parentMarker).toHaveAttribute('markerUnits', 'userSpaceOnUse');

		await waitFor(() => expect(parent).toHaveAttribute('x1'));
		// Root=(80,80), child=(170,170). The diagonal hits each 138×50 card's
		// horizontal side after 25 units, not the card centers as before.
		expect(Number(parent?.getAttribute('x1'))).toBeCloseTo(105, 3);
		expect(Number(parent?.getAttribute('y1'))).toBeCloseTo(105, 3);
		expect(Number(parent?.getAttribute('x2'))).toBeCloseTo(145, 3);
		expect(Number(parent?.getAttribute('y2'))).toBeCloseTo(145, 3);

		for (const [kind, expected] of Object.entries(EDGE_EXPECTATIONS) as Array<
			[EdgeKind, { color: string; dash: string | null }]
		>) {
			const line = document.querySelector(`line.tm-edge[data-edge-kind="${kind}"]`);
			expect(line, kind).not.toBeNull();
			expect(line, kind).toHaveAttribute('stroke', expected.color);
			expect(line, kind).toHaveAttribute('stroke-width', '2.5');
			expect(line, kind).toHaveAttribute('vector-effect', 'non-scaling-stroke');
			expect(line, kind).toHaveAttribute('opacity', '0.92');
			expect(line?.getAttribute('stroke-dasharray'), kind).toBe(expected.dash);
			expect(line?.querySelector('title'), kind).toHaveTextContent(kind.replace(/_/g, ' '));
		}
		for (const marker of document.querySelectorAll('marker[id^="tm-arrow-"]')) {
			expect(marker).toHaveAttribute('refX', '10');
			expect(marker).toHaveAttribute('markerUnits', 'userSpaceOnUse');
		}

		// D3 inlines resolved colors, so a theme-store change must actively
		// re-render rather than leaving light-theme strokes on a dark canvas.
		document.documentElement.style.setProperty('--text-muted', '#b9a58d');
		themeStore.applyRemoteTheme('longhand-dark');
		await waitFor(() => {
			expect(document.querySelector('line.tm-parent-link')).toHaveAttribute(
				'stroke',
				'#b9a58d'
			);
		});
	});
});
