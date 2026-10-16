/**
 * R273: Tests for muijStore reconnect merge semantics.
 *
 * Covers: applyDelta, applySnapshot, mergeSnapshot, handleMuijEvent,
 * handleMuijSnapshot, requestMuijSnapshots, and the SnapshotRequestDeltaTracker
 * logic that determines merge-vs-replace on reconnect.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import type { MuijComponent, MuijDocument, MuijDelta, AgentMuijState } from './muijStore';
import {
    applyDelta,
    applySnapshot,
    mergeSnapshot,
    handleMuijEvent,
    handleMuijSnapshot,
    handleMuijSnapshotError,
    requestMuijSnapshots,
    clearAgentMuij,
    onAgentCycleStarted,
    onDisconnect,
    getComponentsByAgent,
    activeAgentIds,
    _resetForTesting
} from './muijStore';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeComponent(id: string, overrides: Partial<MuijComponent> = {}): MuijComponent {
    return {
        id,
        component_type: 'Gauge',
        label: `Label ${id}`,
        props: { fill: 50 },
        ...overrides
    };
}

function makeDocument(agentId: string, components: MuijComponent[]): MuijDocument {
    return {
        muij_version: '0.1.0',
        agent_id: agentId,
        layout: components,
        generated_at: new Date().toISOString()
    };
}

function emptyState(): Map<string, AgentMuijState> {
    return new Map();
}

function stateWithAgent(agentId: string, components: MuijComponent[]): Map<string, AgentMuijState> {
    const map = new Map<string, MuijComponent>();
    const order: string[] = [];
    for (const c of components) {
        map.set(c.id, c);
        order.push(c.id);
    }
    const state = new Map<string, AgentMuijState>();
    state.set(agentId, { components: map, order });
    return state;
}

// ---------------------------------------------------------------------------
// applyDelta
// ---------------------------------------------------------------------------

describe('applyDelta', () => {
    beforeEach(() => _resetForTesting());

    it('upserts a new component into empty state', () => {
        const state = emptyState();
        const delta: MuijDelta = {
            op: 'upsert',
            component_id: 'c1',
            data: { component_type: 'Gauge', label: 'CPU', props: { fill: 75 } }
        };
        const next = applyDelta(state, 'agent-1', delta);
        const agent = next.get('agent-1')!;
        expect(agent).toBeDefined();
        expect(agent.components.has('c1')).toBe(true);
        expect(agent.order).toEqual(['c1']);
        expect(agent.components.get('c1')!.label).toBe('CPU');
    });

    it('merges into existing component — preserves fields not in delta', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1', { label: 'Original', props: { fill: 50, color: 'blue' } })
        ]);
        const delta: MuijDelta = {
            op: 'upsert',
            component_id: 'c1',
            data: { label: 'Updated', props: { fill: 80 } }
        };
        const next = applyDelta(state, 'agent-1', delta);
        const comp = next.get('agent-1')!.components.get('c1')!;
        expect(comp.label).toBe('Updated');
        expect(comp.props.fill).toBe(80);
        // color from original props should be preserved
        expect(comp.props.color).toBe('blue');
    });

    it('removes a component', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1'),
            makeComponent('c2')
        ]);
        const delta: MuijDelta = { op: 'remove', component_id: 'c1' };
        const next = applyDelta(state, 'agent-1', delta);
        const agent = next.get('agent-1')!;
        expect(agent.components.has('c1')).toBe(false);
        expect(agent.order).toEqual(['c2']);
    });

    it('ignores remove for unseen agent (R277)', () => {
        const state = emptyState();
        const delta: MuijDelta = { op: 'remove', component_id: 'c1' };
        const next = applyDelta(state, 'agent-1', delta);
        expect(next.has('agent-1')).toBe(false);
    });

    it('reorders components and deduplicates (R254)', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1'),
            makeComponent('c2'),
            makeComponent('c3')
        ]);
        // Reorder with duplicates and missing c3
        const delta: MuijDelta = { op: 'reorder', ids: ['c2', 'c1', 'c2'] };
        const next = applyDelta(state, 'agent-1', delta);
        const agent = next.get('agent-1')!;
        // c2 first, c1 second, c3 appended (not in reorder list)
        expect(agent.order).toEqual(['c2', 'c1', 'c3']);
    });

    it('new component defaults to Unknown type for missing component_type', () => {
        const state = emptyState();
        const delta: MuijDelta = {
            op: 'upsert',
            component_id: 'c1',
            data: { label: 'test', props: {} }
        };
        const next = applyDelta(state, 'agent-1', delta);
        expect(next.get('agent-1')!.components.get('c1')!.component_type).toBe('Unknown');
    });

    it('normalizes source/query — null clears, empty string becomes undefined (R262/R264)', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1', { source: 'old-source', query: 'old-query' })
        ]);

        // null clear
        const delta1: MuijDelta = {
            op: 'upsert',
            component_id: 'c1',
            data: { source: null, props: {} }
        };
        const next1 = applyDelta(state, 'agent-1', delta1);
        expect(next1.get('agent-1')!.components.get('c1')!.source).toBeUndefined();
        // query should be preserved (not in delta)
        expect(next1.get('agent-1')!.components.get('c1')!.query).toBe('old-query');

        // empty string normalization
        const delta2: MuijDelta = {
            op: 'upsert',
            component_id: 'c1',
            data: { query: '', props: {} }
        };
        const next2 = applyDelta(next1, 'agent-1', delta2);
        expect(next2.get('agent-1')!.components.get('c1')!.query).toBeUndefined();
    });

    it('rejects non-string source/query values (R269)', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1', { source: 'keep-this' })
        ]);
        const delta: MuijDelta = {
            op: 'upsert',
            component_id: 'c1',
            data: { source: 42, props: {} }
        };
        const next = applyDelta(state, 'agent-1', delta);
        // Non-string should be rejected, existing value preserved as fallback
        expect(next.get('agent-1')!.components.get('c1')!.source).toBe('keep-this');
    });
});

// ---------------------------------------------------------------------------
// applySnapshot
// ---------------------------------------------------------------------------

describe('applySnapshot', () => {
    beforeEach(() => _resetForTesting());

    it('replaces full agent state from document', () => {
        const state = stateWithAgent('agent-1', [makeComponent('old')]);
        const doc = makeDocument('agent-1', [
            makeComponent('c1', { label: 'New 1' }),
            makeComponent('c2', { label: 'New 2' })
        ]);
        const next = applySnapshot(state, 'agent-1', doc);
        const agent = next.get('agent-1')!;
        expect(agent.components.has('old')).toBe(false);
        expect(agent.components.has('c1')).toBe(true);
        expect(agent.components.has('c2')).toBe(true);
        expect(agent.order).toEqual(['c1', 'c2']);
    });

    it('deduplicates layout components by ID (R210)', () => {
        const doc = makeDocument('agent-1', [
            makeComponent('c1', { label: 'First' }),
            makeComponent('c1', { label: 'Duplicate' })
        ]);
        const next = applySnapshot(emptyState(), 'agent-1', doc);
        const agent = next.get('agent-1')!;
        expect(agent.order).toEqual(['c1']);
        expect(agent.components.get('c1')!.label).toBe('First');
    });

    it('skips invalid child components', () => {
        const doc = makeDocument('agent-1', [
            makeComponent('c1'),
            { id: '', component_type: 'Gauge', label: '', props: {} } as MuijComponent, // empty ID
            null as unknown as MuijComponent, // null
        ]);
        const next = applySnapshot(emptyState(), 'agent-1', doc);
        expect(next.get('agent-1')!.order).toEqual(['c1']);
    });
});

// ---------------------------------------------------------------------------
// mergeSnapshot — R257/R258 reconnect merge semantics
// ---------------------------------------------------------------------------

describe('mergeSnapshot', () => {
    beforeEach(() => _resetForTesting());

    it('falls back to applySnapshot when no existing state (R138)', () => {
        const doc = makeDocument('agent-1', [
            makeComponent('c1', { label: 'From snapshot' })
        ]);
        const next = mergeSnapshot(emptyState(), 'agent-1', doc);
        expect(next.get('agent-1')!.components.get('c1')!.label).toBe('From snapshot');
    });

    it('falls back to applySnapshot when existing state is empty', () => {
        const state = new Map<string, AgentMuijState>();
        state.set('agent-1', { components: new Map(), order: [] });
        const doc = makeDocument('agent-1', [makeComponent('c1')]);
        const next = mergeSnapshot(state, 'agent-1', doc);
        expect(next.get('agent-1')!.order).toEqual(['c1']);
    });

    it('preserves delta-applied component over stale snapshot version', () => {
        // Simulate: agent had c1 with fill=90 (from delta), snapshot has c1 with fill=50
        const state = stateWithAgent('agent-1', [
            makeComponent('c1', { label: 'Delta version', props: { fill: 90 } }),
            makeComponent('c2', { label: 'Original c2' })
        ]);
        const doc = makeDocument('agent-1', [
            makeComponent('c1', { label: 'Snapshot version', props: { fill: 50 } }),
            makeComponent('c2', { label: 'Snapshot c2' })
        ]);
        // Tracker says c1 was upserted since request
        const tracker = {
            upserts: new Set(['c1']),
            removed: new Set<string>(),
            reorderSeen: false
        };
        const next = mergeSnapshot(state, 'agent-1', doc, tracker);
        const agent = next.get('agent-1')!;
        // c1 should keep delta version (fresher)
        expect(agent.components.get('c1')!.label).toBe('Delta version');
        expect(agent.components.get('c1')!.props.fill).toBe(90);
        // c2 was NOT upserted since request — snapshot version wins
        expect(agent.components.get('c2')!.label).toBe('Snapshot c2');
    });

    it('honours post-request removes — R257', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1'),
            makeComponent('c2')
        ]);
        const doc = makeDocument('agent-1', [
            makeComponent('c1'),
            makeComponent('c2'),
            makeComponent('c3') // new in snapshot
        ]);
        // c2 was removed after snapshot request
        const tracker = {
            upserts: new Set<string>(),
            removed: new Set(['c2']),
            reorderSeen: false
        };
        const next = mergeSnapshot(state, 'agent-1', doc, tracker);
        const agent = next.get('agent-1')!;
        expect(agent.components.has('c1')).toBe(true);
        expect(agent.components.has('c2')).toBe(false); // removed wins
        expect(agent.components.has('c3')).toBe(true);  // new from snapshot
    });

    it('honours post-request reorder — R258', () => {
        // Existing state has order [c2, c1] from a reorder delta
        const state = new Map<string, AgentMuijState>();
        const components = new Map<string, MuijComponent>();
        components.set('c1', makeComponent('c1'));
        components.set('c2', makeComponent('c2'));
        state.set('agent-1', { components, order: ['c2', 'c1'] });

        // Snapshot has order [c1, c2] — stale
        const doc = makeDocument('agent-1', [
            makeComponent('c1'),
            makeComponent('c2')
        ]);
        const tracker = {
            upserts: new Set<string>(),
            removed: new Set<string>(),
            reorderSeen: true // reorder delta was seen
        };
        const next = mergeSnapshot(state, 'agent-1', doc, tracker);
        const agent = next.get('agent-1')!;
        // Existing order [c2, c1] should win over snapshot order [c1, c2]
        expect(agent.order).toEqual(['c2', 'c1']);
    });

    it('preserves delta-only components not in snapshot', () => {
        // c3 was upserted by delta after snapshot request but isn't in snapshot
        const state = stateWithAgent('agent-1', [
            makeComponent('c1'),
            makeComponent('c3', { label: 'Delta-only' })
        ]);
        const doc = makeDocument('agent-1', [makeComponent('c1')]);
        const tracker = {
            upserts: new Set(['c3']),
            removed: new Set<string>(),
            reorderSeen: false
        };
        const next = mergeSnapshot(state, 'agent-1', doc, tracker);
        const agent = next.get('agent-1')!;
        expect(agent.components.has('c3')).toBe(true);
        expect(agent.components.get('c3')!.label).toBe('Delta-only');
        expect(agent.order).toContain('c3');
    });

    it('without tracker, snapshot components replace existing', () => {
        const state = stateWithAgent('agent-1', [
            makeComponent('c1', { label: 'Existing' })
        ]);
        const doc = makeDocument('agent-1', [
            makeComponent('c1', { label: 'Snapshot' })
        ]);
        // No tracker — no information about which components were touched
        const next = mergeSnapshot(state, 'agent-1', doc);
        const agent = next.get('agent-1')!;
        // Without tracker, all snapshot components win (no upserts set to check)
        expect(agent.components.get('c1')!.label).toBe('Snapshot');
    });

    it('deduplicates merged order', () => {
        const state = stateWithAgent('agent-1', [makeComponent('c1')]);
        const doc = makeDocument('agent-1', [
            makeComponent('c1'),
            makeComponent('c1') // duplicate
        ]);
        const next = mergeSnapshot(state, 'agent-1', doc);
        expect(next.get('agent-1')!.order).toEqual(['c1']);
    });
});

// ---------------------------------------------------------------------------
// handleMuijEvent — event handler orchestration
// ---------------------------------------------------------------------------

describe('handleMuijEvent', () => {
    beforeEach(() => _resetForTesting());

    it('rejects malformed delta (missing op)', () => {
        // Should not throw
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-1',
            payload: { bad: true },
            timestamp: Date.now()
        });
    });

    it('rejects unknown op (R275)', () => {
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-1',
            payload: { op: 'unknown_op', component_id: 'c1', data: {} },
            timestamp: Date.now()
        });
    });

    it('rejects upsert with null data (R204)', () => {
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-1',
            payload: { op: 'upsert', component_id: 'c1', data: null },
            timestamp: Date.now()
        });
    });

    it('rejects upsert with array data (R308)', () => {
        const agentId = `agent-array-${Date.now()}-${Math.random()}`;
        onAgentCycleStarted(agentId);
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: agentId,
            payload: { op: 'upsert', component_id: 'c1', data: [] },
            timestamp: Date.now()
        });
        expect(get(activeAgentIds)).not.toContain(agentId);
    });

    it('ignores non-delta event types', () => {
        handleMuijEvent({
            event_type: 'agent.cycle.started',
            agent_id: 'agent-1',
            payload: {},
            timestamp: Date.now()
        });
    });
});

// ---------------------------------------------------------------------------
// handleMuijSnapshot — merge vs replace orchestration
// ---------------------------------------------------------------------------

describe('handleMuijSnapshot', () => {
    beforeEach(() => _resetForTesting());

    it('rejects invalid snapshot document', () => {
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        handleMuijSnapshot('agent-1', null as unknown as MuijDocument);
        expect(warn).toHaveBeenCalledWith(expect.stringContaining('invalid snapshot document'));
        warn.mockRestore();
    });

    it('rejects snapshot document with non-array layout', () => {
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        handleMuijSnapshot('agent-1', { muij_version: '0.1.0', agent_id: 'agent-1', layout: 'not-array', generated_at: '' } as unknown as MuijDocument);
        expect(warn).toHaveBeenCalled();
        warn.mockRestore();
    });
});

// ---------------------------------------------------------------------------
// requestMuijSnapshots — SnapshotRequestDeltaTracker setup
// ---------------------------------------------------------------------------

describe('requestMuijSnapshots', () => {
    beforeEach(() => _resetForTesting());

    it('calls ws.send for each tracked agent', () => {
        // First, make the store aware of an agent by sending a delta
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'snap-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });
        const send = vi.fn(() => true);
        requestMuijSnapshots({ send });
        // Should have been called at least once for snap-agent
        expect(send).toHaveBeenCalled();
        const matchingCall = send.mock.calls.find(
            (call: unknown[]) => {
                const arg = call[0] as Record<string, unknown> | undefined;
                return arg?.agent_id === 'snap-agent';
            }
        );
        expect(matchingCall).toBeDefined();
        expect((matchingCall as unknown[])[0]).toMatchObject({
            type: 'agent.ui.snapshot_request',
            agent_id: 'snap-agent'
        });
    });

    it('logs warning on send failure (R212)', () => {
        // Ensure agent is tracked
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'fail-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        const send = vi.fn(() => false);
        requestMuijSnapshots({ send });
        expect(warn).toHaveBeenCalledWith(expect.stringContaining('snapshot request send failed'));
        warn.mockRestore();
    });
});

// ---------------------------------------------------------------------------
// Reconnect flow integration — delta → request → delta → snapshot
// ---------------------------------------------------------------------------

describe('reconnect merge flow (integration)', () => {
    beforeEach(() => {
        _resetForTesting();
        // Start a fresh cycle
        onAgentCycleStarted('integ-agent', 'cycle-1');
    });

    it('merge path: delta after request preserves fresher component', () => {
        // Step 1: Initial delta
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'integ-agent',
            payload: {
                op: 'upsert',
                component_id: 'c1',
                data: { component_type: 'Gauge', label: 'Initial', props: { fill: 30 } }
            },
            timestamp: Date.now()
        });

        // Step 2: Simulate reconnect — request snapshots
        const send = vi.fn(() => true);
        requestMuijSnapshots({ send });

        // Step 3: Delta arrives AFTER snapshot request
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'integ-agent',
            payload: {
                op: 'upsert',
                component_id: 'c1',
                data: { label: 'Fresher', props: { fill: 95 } }
            },
            timestamp: Date.now()
        });

        // Step 4: Stale snapshot arrives (has old fill=30)
        const snapshotDoc = makeDocument('integ-agent', [
            makeComponent('c1', { label: 'Stale snapshot', props: { fill: 30 } })
        ]);
        handleMuijSnapshot('integ-agent', snapshotDoc);

        // Verify: merge path should have preserved the fresher delta version
        // We can't directly read muijMap, but we can use getComponentsByAgent
        // to verify the final state via the derived store.
        let components: MuijComponent[] = [];
        const unsubscribe = getComponentsByAgent('integ-agent').subscribe(value => {
            components = value;
        });
        expect(components.length).toBeGreaterThanOrEqual(1);
        const c1 = components.find(c => c.id === 'c1');
        expect(c1).toBeDefined();
        expect(c1!.label).toBe('Fresher');
        expect(c1!.props.fill).toBe(95);
        unsubscribe();
    });

    it('replace path: no delta after request uses snapshot as-is', () => {
        // Step 1: Initial delta
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'integ-agent',
            payload: {
                op: 'upsert',
                component_id: 'c1',
                data: { component_type: 'Gauge', label: 'Old', props: { fill: 10 } }
            },
            timestamp: Date.now()
        });

        // Step 2: Request snapshots (no delta after this)
        const send = vi.fn(() => true);
        requestMuijSnapshots({ send });

        // Step 3: Snapshot arrives (no delta in between)
        const snapshotDoc = makeDocument('integ-agent', [
            makeComponent('c1', { label: 'Authoritative', props: { fill: 100 } })
        ]);
        handleMuijSnapshot('integ-agent', snapshotDoc);

        let components: MuijComponent[] = [];
        const unsubscribe = getComponentsByAgent('integ-agent').subscribe(value => {
            components = value;
        });
        const c1 = components.find(c => c.id === 'c1');
        expect(c1).toBeDefined();
        expect(c1!.label).toBe('Authoritative');
        expect(c1!.props.fill).toBe(100);
        unsubscribe();
    });

    it('merge path: remove delta after request excludes component from snapshot', () => {
        // Step 1: Initial deltas for c1 and c2
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'integ-agent',
            payload: {
                op: 'upsert',
                component_id: 'c1',
                data: { component_type: 'Gauge', label: 'Keep', props: {} }
            },
            timestamp: Date.now()
        });
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'integ-agent',
            payload: {
                op: 'upsert',
                component_id: 'c2',
                data: { component_type: 'Gauge', label: 'Will remove', props: {} }
            },
            timestamp: Date.now()
        });

        // Step 2: Request snapshots
        const send = vi.fn(() => true);
        requestMuijSnapshots({ send });

        // Step 3: Remove c2 after request
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'integ-agent',
            payload: { op: 'remove', component_id: 'c2' },
            timestamp: Date.now()
        });

        // Step 4: Snapshot arrives with both c1 and c2
        const snapshotDoc = makeDocument('integ-agent', [
            makeComponent('c1', { label: 'Snap c1' }),
            makeComponent('c2', { label: 'Snap c2 (stale)' })
        ]);
        handleMuijSnapshot('integ-agent', snapshotDoc);

        let components: MuijComponent[] = [];
        const unsubscribe = getComponentsByAgent('integ-agent').subscribe(value => {
            components = value;
        });
        expect(components.find(c => c.id === 'c1')).toBeDefined();
        expect(components.find(c => c.id === 'c2')).toBeUndefined(); // R257: remove wins
        unsubscribe();
    });
});

// ---------------------------------------------------------------------------
// onDisconnect — R607 test coverage
// ---------------------------------------------------------------------------

describe('onDisconnect', () => {
    beforeEach(() => _resetForTesting());

    it('clears recentlyCleared so agents are not silenced after reconnect (R362)', () => {
        // Build up state: start cycle, send delta, complete cycle (sets recentlyCleared)
        onAgentCycleStarted('agent-x', 'cycle-1');
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-x',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });
        clearAgentMuij('agent-x', 'cycle-1');

        // Verify agent is silenced (recentlyCleared blocks deltas)
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-x',
            payload: { op: 'upsert', component_id: 'c2', data: { component_type: 'Gauge', label: 'blocked', props: {} } },
            timestamp: Date.now()
        });
        expect(get(activeAgentIds)).not.toContain('agent-x');

        // Disconnect should clear recentlyCleared
        onDisconnect();

        // Now deltas should be accepted again
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-x',
            payload: { op: 'upsert', component_id: 'c3', data: { component_type: 'Gauge', label: 'unblocked', props: {} } },
            timestamp: Date.now()
        });
        expect(get(activeAgentIds)).toContain('agent-x');
    });

    it('resets snapshot cooldown so reconnect can request snapshots (R332)', () => {
        // First request — should succeed
        const send1 = vi.fn(() => true);
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'cooldown-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });
        requestMuijSnapshots({ send: send1 });
        expect(send1).toHaveBeenCalled();

        // Immediate second request — should be throttled
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        const send2 = vi.fn(() => true);
        requestMuijSnapshots({ send: send2 });
        expect(send2).not.toHaveBeenCalled();
        expect(warn).toHaveBeenCalledWith(expect.stringContaining('throttled'));
        warn.mockRestore();

        // Disconnect resets cooldown
        onDisconnect();

        // Re-add agent state and request again — should succeed
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'cooldown-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });
        const send3 = vi.fn(() => true);
        requestMuijSnapshots({ send: send3 });
        expect(send3).toHaveBeenCalled();
    });

    it('clears activeCycleByAgent so stale cycle IDs do not block completions (R376)', () => {
        // Start a cycle before disconnect
        onAgentCycleStarted('agent-y', 'old-cycle');
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-y',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });

        // Disconnect clears activeCycleByAgent
        onDisconnect();

        // Start new cycle with different ID after reconnect
        onAgentCycleStarted('agent-y', 'new-cycle');
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'agent-y',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'new', props: {} } },
            timestamp: Date.now()
        });

        // Clear with new cycle ID should succeed (old-cycle not blocking)
        clearAgentMuij('agent-y', 'new-cycle');
        expect(get(activeAgentIds)).not.toContain('agent-y');
    });

    it('preserves muijMap state across disconnect for reconnect (R507)', () => {
        // Build up agent state
        onAgentCycleStarted('persist-agent', 'cycle-1');
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'persist-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'persisted', props: { fill: 42 } } },
            timestamp: Date.now()
        });
        expect(get(activeAgentIds)).toContain('persist-agent');

        // Disconnect — muijMap should NOT be cleared (only tracking state)
        onDisconnect();

        // muijMap retains rendered state for visual continuity
        expect(get(activeAgentIds)).toContain('persist-agent');

        let components: MuijComponent[] = [];
        const unsubscribe = getComponentsByAgent('persist-agent').subscribe(value => {
            components = value;
        });
        expect(components.length).toBe(1);
        expect(components[0].props.fill).toBe(42);
        unsubscribe();
    });
});

// ---------------------------------------------------------------------------
// R651: Reorder delta length cap
// ---------------------------------------------------------------------------

describe('reorder delta length cap (R651)', () => {
    beforeEach(() => _resetForTesting());

    it('drops reorder delta exceeding 10000 IDs', () => {
        onAgentCycleStarted('cap-agent');
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'cap-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'test', props: {} } },
            timestamp: Date.now()
        });

        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        const hugeIds = Array.from({ length: 10_001 }, (_, i) => `id-${i}`);
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'cap-agent',
            payload: { op: 'reorder', ids: hugeIds },
            timestamp: Date.now()
        });
        expect(warn).toHaveBeenCalledWith(expect.stringContaining('exceeds cap'));
        warn.mockRestore();

        // Agent should still have original order (reorder was dropped)
        let components: MuijComponent[] = [];
        const unsubscribe = getComponentsByAgent('cap-agent').subscribe(value => {
            components = value;
        });
        expect(components.length).toBe(1);
        expect(components[0].id).toBe('c1');
        unsubscribe();
    });
});

// ---------------------------------------------------------------------------
// R629/R740: Cycle start keeps agent tracked for reconnect
// ---------------------------------------------------------------------------

describe('onAgentCycleStarted reconnect safety (R629/R740)', () => {
    beforeEach(() => _resetForTesting());

    it('reconnect after cycle start can still request snapshot', () => {
        // Step 1: Build state
        onAgentCycleStarted('reconnect-agent', 'cycle-1');
        handleMuijEvent({
            event_type: 'agent.ui.delta',
            agent_id: 'reconnect-agent',
            payload: { op: 'upsert', component_id: 'c1', data: { component_type: 'Gauge', label: 'old', props: {} } },
            timestamp: Date.now()
        });

        // Step 2: New cycle starts — clears state but keeps tracked
        onAgentCycleStarted('reconnect-agent', 'cycle-2');

        // Step 3: Reconnect — snapshot request should include this agent
        const send = vi.fn(() => true);
        requestMuijSnapshots({ send });

        const matchingCall = send.mock.calls.find(
            (call: unknown[]) => {
                const arg = call[0] as Record<string, unknown> | undefined;
                return arg?.agent_id === 'reconnect-agent';
            }
        );
        expect(matchingCall).toBeDefined();
    });
});
