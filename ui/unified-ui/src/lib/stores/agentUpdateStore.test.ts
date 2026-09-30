import { describe, expect, it, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import type { AgentEventEnvelope } from '$lib/realtime/v2-websocket';
import type { AgentUpdate } from '$lib/types/agentUpdate';
import {
	AGENT_UPDATE_EVENT_TYPE,
	__resetForTests,
	clearScope,
	handleAgentUpdateEnvelopeEvent,
	seedScope,
	updatesForScope,
	updatesForThread
} from './agentUpdateStore';

function makeUpdate(id: string, ts: number, extra: Partial<AgentUpdate> = {}): AgentUpdate {
	return {
		id,
		ts,
		workspace_id: 'ws_a',
		agent_id: 'cfo',
		kind: 'agent_resumed',
		...extra
	} as AgentUpdate;
}

function makeEnvelope(update: AgentUpdate, principal = 'alpha', workspace = 'ws_a'): AgentEventEnvelope {
	return {
		event_type: AGENT_UPDATE_EVENT_TYPE,
		agent_id: update.agent_id,
		principal,
		workspace,
		payload: update,
		timestamp: update.ts
	};
}

describe('agentUpdateStore', () => {
	beforeEach(() => {
		__resetForTests();
	});

	it('seedScope populates the scope newest-first', () => {
		seedScope('alpha', 'ws_a', [
			makeUpdate('a', 10),
			makeUpdate('b', 30),
			makeUpdate('c', 20)
		]);
		const events = get(updatesForScope('alpha', 'ws_a'));
		expect(events.map((e) => e.id)).toEqual(['b', 'c', 'a']);
	});

	it('seedScope deduplicates by id', () => {
		seedScope('alpha', 'ws_a', [makeUpdate('a', 10)]);
		seedScope('alpha', 'ws_a', [makeUpdate('a', 10), makeUpdate('b', 20)]);
		const events = get(updatesForScope('alpha', 'ws_a'));
		expect(events.map((e) => e.id)).toEqual(['b', 'a']);
	});

	it('handleAgentUpdateEnvelopeEvent inserts events for the envelope scope', () => {
		const update = makeUpdate('live-1', 100);
		handleAgentUpdateEnvelopeEvent(makeEnvelope(update));
		const events = get(updatesForScope('alpha', 'ws_a'));
		expect(events).toHaveLength(1);
		expect(events[0].id).toBe('live-1');
	});

	it('ignores envelopes with the wrong event_type', () => {
		const update = makeUpdate('x', 1);
		const env = { ...makeEnvelope(update), event_type: 'agent.ui.delta' };
		handleAgentUpdateEnvelopeEvent(env);
		expect(get(updatesForScope('alpha', 'ws_a'))).toHaveLength(0);
	});

	it('drops envelopes missing principal or workspace', () => {
		const update = makeUpdate('x', 1);
		handleAgentUpdateEnvelopeEvent({ ...makeEnvelope(update), principal: null });
		handleAgentUpdateEnvelopeEvent({ ...makeEnvelope(update), workspace: '' });
		expect(get(updatesForScope('alpha', 'ws_a'))).toHaveLength(0);
	});

	it('different scopes stay isolated', () => {
		seedScope('alpha', 'ws_a', [makeUpdate('a', 1)]);
		seedScope('alpha', 'ws_b', [makeUpdate('b', 2)]);
		expect(get(updatesForScope('alpha', 'ws_a')).map((e) => e.id)).toEqual(['a']);
		expect(get(updatesForScope('alpha', 'ws_b')).map((e) => e.id)).toEqual(['b']);
	});

	it('clearScope removes the scope window', () => {
		seedScope('alpha', 'ws_a', [makeUpdate('a', 1)]);
		clearScope('alpha', 'ws_a');
		expect(get(updatesForScope('alpha', 'ws_a'))).toHaveLength(0);
	});

	it('live event after seed appears at the top when newer', () => {
		seedScope('alpha', 'ws_a', [makeUpdate('old', 10)]);
		handleAgentUpdateEnvelopeEvent(makeEnvelope(makeUpdate('new', 50)));
		const events = get(updatesForScope('alpha', 'ws_a'));
		expect(events.map((e) => e.id)).toEqual(['new', 'old']);
	});

	it('duplicate live event after seed is ignored', () => {
		const update = makeUpdate('same', 10);
		seedScope('alpha', 'ws_a', [update]);
		handleAgentUpdateEnvelopeEvent(makeEnvelope(update));
		expect(get(updatesForScope('alpha', 'ws_a'))).toHaveLength(1);
	});

	it('updatesForThread returns only matching thread_id events', () => {
		seedScope('alpha', 'ws_a', [
			makeUpdate('a', 10, { thread_id: 'th_1' }),
			makeUpdate('b', 20, { thread_id: 'th_2' }),
			makeUpdate('c', 30, { thread_id: 'th_1' }),
			makeUpdate('d', 40) // no thread_id
		]);
		const th1 = get(updatesForThread('alpha', 'ws_a', 'th_1'));
		expect(th1.map((e) => e.id).sort()).toEqual(['a', 'c']);
		const th2 = get(updatesForThread('alpha', 'ws_a', 'th_2'));
		expect(th2.map((e) => e.id)).toEqual(['b']);
	});

	it('updatesForThread reacts when a new thread-scoped event arrives', () => {
		seedScope('alpha', 'ws_a', [makeUpdate('a', 10, { thread_id: 'th_1' })]);
		expect(get(updatesForThread('alpha', 'ws_a', 'th_1'))).toHaveLength(1);

		handleAgentUpdateEnvelopeEvent(
			makeEnvelope(makeUpdate('b', 20, { thread_id: 'th_1' }))
		);
		expect(get(updatesForThread('alpha', 'ws_a', 'th_1'))).toHaveLength(2);

		// Different thread — same workspace: does not affect th_1.
		handleAgentUpdateEnvelopeEvent(
			makeEnvelope(makeUpdate('c', 30, { thread_id: 'th_2' }))
		);
		expect(get(updatesForThread('alpha', 'ws_a', 'th_1'))).toHaveLength(2);
		expect(get(updatesForThread('alpha', 'ws_a', 'th_2'))).toHaveLength(1);
	});

	it('updatesForThread with empty thread id returns all events (no filter)', () => {
		seedScope('alpha', 'ws_a', [
			makeUpdate('a', 10, { thread_id: 'th_1' }),
			makeUpdate('b', 20)
		]);
		expect(get(updatesForThread('alpha', 'ws_a', ''))).toHaveLength(2);
	});
});
