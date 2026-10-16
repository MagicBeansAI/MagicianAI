import { describe, it, expect } from 'vitest';
import { categorizeEvent, jumpTargetIndex, diffMaps } from './timeLoom';
import type {
	MapEvent,
	MapOperation,
	ThinkingMap,
	ThinkingNode,
	NodeKind
} from '$lib/types/thinkingMap';

// ── Fixtures ─────────────────────────────────────────────────────────────────

function node(id: string, overrides: Partial<ThinkingNode> = {}): ThinkingNode {
	return {
		node_id: id,
		kind: 'idea',
		label: `Node ${id}`,
		epistemic_state: 'asserted',
		assertion_origin: 'owner_spoken',
		confidence: 0.8,
		source_refs: [],
		position_locked: false,
		promoted_refs: [],
		tombstoned: false,
		created_at: '2026-07-22T00:00:00Z',
		updated_at: '2026-07-22T00:00:00Z',
		...overrides
	};
}

function event(
	sequence: number,
	operations: MapOperation[],
	utteranceId?: string
): MapEvent {
	return {
		sequence,
		envelope: {
			schema_version: 1,
			envelope_id: `env-${sequence}`,
			map_id: 'm1',
			base_revision: sequence - 1,
			...(utteranceId !== undefined ? { utterance_id: utteranceId } : {}),
			actor: { actor: 'owner', principal: 'anonymous' },
			idempotency_key: `key-${sequence}`,
			operations,
			created_at: '2026-07-22T00:00:00Z'
		},
		resulting_revision: sequence,
		semantic_hash: `hash-${sequence}`,
		applied_at: '2026-07-22T00:00:00Z'
	};
}

function mapWith(nodes: ThinkingNode[]): ThinkingMap {
	return {
		schema_version: 1,
		map_id: 'm1',
		principal: 'anonymous',
		workspace: 'default',
		title: 'Test map',
		source: { kind: 'solo' },
		lifecycle: 'active',
		revision: nodes.length,
		view_state: { lens: 'graph' },
		nodes: Object.fromEntries(nodes.map((n) => [n.node_id, n])),
		edges: {},
		clarifications: {},
		proposals: {},
		applied_envelopes: [],
		created_at: '2026-07-22T00:00:00Z',
		updated_at: '2026-07-22T00:00:00Z'
	};
}

const addNode = (id: string, kind: NodeKind = 'idea'): MapOperation => ({
	op: 'add_node',
	node: node(id, { kind })
});

// ── categorizeEvent ──────────────────────────────────────────────────────────

describe('categorizeEvent', () => {
	it('marks an event with a non-empty utterance_id as utterance', () => {
		expect(categorizeEvent(event(1, [addNode('a')], 'utt-1'))).toEqual(['utterance']);
	});

	it('ignores an absent or blank utterance_id', () => {
		expect(categorizeEvent(event(1, [addNode('a')]))).toEqual([]);
		expect(categorizeEvent(event(1, [addNode('a')], '  '))).toEqual([]);
	});

	it('marks update_node and set_epistemic_state ops as correction', () => {
		expect(categorizeEvent(event(1, [{ op: 'update_node', node_id: 'a', label: 'x' }]))).toEqual([
			'correction'
		]);
		expect(
			categorizeEvent(event(1, [{ op: 'set_epistemic_state', node_id: 'a', state: 'superseded' }]))
		).toEqual(['correction']);
	});

	it('marks add_node of a decision node as decision — but not other kinds', () => {
		expect(categorizeEvent(event(1, [addNode('d', 'decision')]))).toEqual(['decision']);
		expect(categorizeEvent(event(1, [addNode('i', 'idea')]))).toEqual([]);
	});

	it('marks set_node_kind → decision as decision (and only to decision)', () => {
		expect(
			categorizeEvent(event(1, [{ op: 'set_node_kind', node_id: 'a', kind: 'decision' }]))
		).toEqual(['decision']);
		expect(
			categorizeEvent(event(1, [{ op: 'set_node_kind', node_id: 'a', kind: 'risk' }]))
		).toEqual([]);
	});

	it('marks create/resolve_clarification as clarification', () => {
		expect(
			categorizeEvent(
				event(1, [
					{
						op: 'create_clarification',
						clarification: {
							clarification_id: 'c1',
							node_id: 'a',
							question: 'Why?',
							state: 'open',
							created_at: '2026-07-22T00:00:00Z'
						}
					}
				])
			)
		).toEqual(['clarification']);
		expect(
			categorizeEvent(
				event(1, [{ op: 'resolve_clarification', clarification_id: 'c1', state: 'answered' }])
			)
		).toEqual(['clarification']);
	});

	it('marks link_promoted_object as promotion', () => {
		expect(
			categorizeEvent(
				event(1, [
					{
						op: 'link_promoted_object',
						node_id: 'a',
						promoted: {
							destination_kind: 'task',
							object_id: 't1',
							linked_at: '2026-07-22T00:00:00Z'
						}
					}
				])
			)
		).toEqual(['promotion']);
	});

	it('returns multiple categories in stable EVENT_CATEGORIES order, deduped', () => {
		const multi = event(
			1,
			[
				{
					op: 'link_promoted_object',
					node_id: 'a',
					promoted: { destination_kind: 'memory', object_id: 'm1', linked_at: 'x' }
				},
				{ op: 'update_node', node_id: 'a', label: 'y' },
				{ op: 'update_node', node_id: 'b', label: 'z' },
				addNode('d', 'decision')
			],
			'utt-9'
		);
		// Order is the canonical category order, NOT op order; no duplicates.
		expect(categorizeEvent(multi)).toEqual(['utterance', 'correction', 'decision', 'promotion']);
	});

	it('does NOT categorize ops nested inside a propose_restructure', () => {
		expect(
			categorizeEvent(
				event(1, [
					{
						op: 'propose_restructure',
						proposal: {
							proposal_id: 'p1',
							proposed_by: { actor: 'model' },
							rationale: 'tidy',
							operations: [{ op: 'update_node', node_id: 'a', label: 'nested' }],
							state: 'proposed',
							affected_node_ids: ['a'],
							created_at: '2026-07-22T00:00:00Z'
						}
					}
				])
			)
		).toEqual([]);
	});
});

// ── jumpTargetIndex ──────────────────────────────────────────────────────────

describe('jumpTargetIndex', () => {
	// idx:        0            1               2            3            4
	const timeline: MapEvent[] = [
		event(1, [addNode('a')], 'utt-1'), // utterance
		event(2, [addNode('d', 'decision')]), // decision
		event(3, [{ op: 'update_node', node_id: 'a', label: 'x' }]), // correction
		event(4, [addNode('b')], 'utt-2'), // utterance
		event(5, [addNode('c')]) // (none)
	];

	it('jumps backward from "now" (null) to the latest match strictly before the end', () => {
		// "now" ≡ index 4 (last event's state), so backward scans 3..0.
		expect(jumpTargetIndex(timeline, null, 'utterance', -1)).toBe(3);
		expect(jumpTargetIndex(timeline, null, 'decision', -1)).toBe(1);
	});

	it('jumps forward from a middle position to the next match', () => {
		expect(jumpTargetIndex(timeline, 0, 'utterance', 1)).toBe(3);
		expect(jumpTargetIndex(timeline, 1, 'correction', 1)).toBe(2);
	});

	it('is exclusive of the current position in both directions', () => {
		// Sitting ON an utterance: backward finds the OTHER utterance.
		expect(jumpTargetIndex(timeline, 3, 'utterance', -1)).toBe(0);
		expect(jumpTargetIndex(timeline, 0, 'utterance', -1)).toBeNull();
		expect(jumpTargetIndex(timeline, 3, 'utterance', 1)).toBeNull();
	});

	it('returns null when nothing matches in that direction', () => {
		expect(jumpTargetIndex(timeline, 2, 'decision', 1)).toBeNull();
		expect(jumpTargetIndex(timeline, null, 'promotion', -1)).toBeNull();
	});

	it('forward from "now" never finds anything (now is the end)', () => {
		expect(jumpTargetIndex(timeline, null, 'utterance', 1)).toBeNull();
	});

	it('handles an empty timeline', () => {
		expect(jumpTargetIndex([], null, 'utterance', -1)).toBeNull();
		expect(jumpTargetIndex([], 0, 'utterance', 1)).toBeNull();
	});
});

// ── diffMaps ─────────────────────────────────────────────────────────────────

describe('diffMaps', () => {
	it('reports nothing for identical maps', () => {
		const m = mapWith([node('a'), node('b')]);
		expect(diffMaps(m, m)).toEqual({ added: [], removed: [], changed: [] });
	});

	it('reports nodes present only in now as added, only in then as removed', () => {
		const then = mapWith([node('a'), node('gone')]);
		const now = mapWith([node('a'), node('new1'), node('new2')]);
		expect(diffMaps(then, now)).toEqual({
			added: ['new1', 'new2'],
			removed: ['gone'],
			changed: []
		});
	});

	it('treats a node tombstoned in now as removed, and one revived in now as added', () => {
		const then = mapWith([node('a'), node('b', { tombstoned: true })]);
		const now = mapWith([node('a', { tombstoned: true }), node('b')]);
		expect(diffMaps(then, now)).toEqual({ added: ['b'], removed: ['a'], changed: [] });
	});

	it('reports semantic field changes (label/state/kind/confidence/parent/detail)', () => {
		const then = mapWith([
			node('a'),
			node('b'),
			node('c'),
			node('d', { detail_markdown: 'old' })
		]);
		const now = mapWith([
			node('a', { label: 'renamed' }),
			node('b', { epistemic_state: 'confirmed' }),
			node('c', { parent_id: 'a' }),
			node('d', { detail_markdown: undefined })
		]);
		expect(diffMaps(then, now).changed).toEqual(['a', 'b', 'c', 'd']);
	});

	it('counts a new promotion link as a change', () => {
		const then = mapWith([node('a')]);
		const now = mapWith([
			node('a', {
				promoted_refs: [{ destination_kind: 'task', object_id: 't1', linked_at: 'x' }]
			})
		]);
		expect(diffMaps(then, now).changed).toEqual(['a']);
	});

	it('ignores layout-only and bookkeeping differences', () => {
		const then = mapWith([node('a')]);
		const now = mapWith([
			node('a', {
				position: { x: 120, y: -40 },
				position_locked: true,
				updated_at: '2026-07-23T09:00:00Z'
			})
		]);
		expect(diffMaps(then, now)).toEqual({ added: [], removed: [], changed: [] });
	});

	it('sorts every id list lexicographically', () => {
		const then = mapWith([node('z-old'), node('a-old')]);
		const now = mapWith([node('z-new'), node('a-new')]);
		expect(diffMaps(then, now)).toEqual({
			added: ['a-new', 'z-new'],
			removed: ['a-old', 'z-old'],
			changed: []
		});
	});
});
