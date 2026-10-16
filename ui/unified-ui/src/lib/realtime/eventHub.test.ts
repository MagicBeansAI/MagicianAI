import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	EventHub,
	canonicalEventLine,
	matchesHubFilter,
	v3EventsPredicate,
	parseNdjsonFrames,
	subscribeWithBackfill,
	toHubEvent,
	type HubEvent
} from './eventHub';

class FakeSocket {
	static instances: FakeSocket[] = [];
	readyState = 0;
	onopen: ((e: Event) => void) | null = null;
	onmessage: ((e: MessageEvent) => void) | null = null;
	onerror: ((e: Event) => void) | null = null;
	onclose: ((e: CloseEvent) => void) | null = null;
	sent: unknown[] = [];
	closed = false;
	constructor(
		public url: string,
		public protocols?: string[]
	) {
		FakeSocket.instances.push(this);
	}
	open() {
		this.readyState = 1;
		this.onopen?.({} as Event);
	}
	receive(frame: unknown) {
		this.onmessage?.({ data: JSON.stringify(frame) } as MessageEvent);
	}
	serverClose() {
		this.readyState = 3;
		this.onclose?.({} as CloseEvent);
	}
	send(data: string) {
		this.sent.push(JSON.parse(data));
	}
	close() {
		this.closed = true;
		this.readyState = 3;
	}
}

function makeHub(opts: { helloTimeoutMs?: number } = {}) {
	return new EventHub({
		webSocket: () => FakeSocket as unknown as typeof WebSocket,
		url: (query) => `ws://test/api/magician/v2/realtime/ws${query}`,
		protocols: () => [],
		bindEnvironment: false,
		helloTimeoutMs: opts.helloTimeoutMs ?? 2_000
	});
}

const last = () => FakeSocket.instances.at(-1)!;
const params = (socket: FakeSocket) => new URL(socket.url).searchParams;

beforeEach(() => {
	FakeSocket.instances = [];
	vi.useFakeTimers();
});

afterEach(() => {
	vi.useRealTimers();
});

describe('hub filter matching', () => {
	it('matches categories and user relevance from the client-side taxonomy', () => {
		const event = toHubEvent({ event_type: 'HitlRequested', data: { correlation_id: 'c1' } });
		expect(event.classified).toBe(true);
		expect(event.categories).toEqual(['hitl']);
		expect(matchesHubFilter(event, { categories: ['hitl'], userRelevant: true })).toBe(true);
		expect(matchesHubFilter(event, { categories: ['execution'] })).toBe(false);
	});

	it('classifies AgentEvent envelopes on their inner type and unwraps them', () => {
		const event = toHubEvent({
			event_type: 'AgentEvent',
			data: { event: { event_type: 'tool.call.started', agent_id: 'a1', payload: { task_id: 't1' } } }
		});
		expect(event.type).toBe('tool.call.started');
		expect(event.inner?.event_type).toBe('tool.call.started');
		expect(event.payload).toEqual({ task_id: 't1' });
		expect(event.categories).toEqual(['tool']);
		expect(event.ref('agent_id')).toBe('a1');
		expect(matchesHubFilter(event, { eventTypes: ['tool.'] })).toBe(true);
		expect(matchesHubFilter(event, { eventTypes: ['AgentEvent'] })).toBe(true);
		expect(matchesHubFilter(event, { eventTypes: ['reasoning.'] })).toBe(false);
	});

	it('soft-passes taxonomy filters for unclassified types, as /v3/events does', () => {
		const event = toHubEvent({ event_type: 'SomethingNew', data: {} });
		expect(event.classified).toBe(false);
		expect(matchesHubFilter(event, { categories: ['hitl'], userRelevant: true })).toBe(true);
	});

	it('prefers server categories, user_relevant and refs when present', () => {
		const event = toHubEvent({
			event_type: 'HitlRequested',
			data: { task_id: 'payload-task' },
			categories: ['agentic'],
			user_relevant: false,
			refs: { task_id: 'server-task' }
		});
		expect(event.categories).toEqual(['agentic']);
		expect(matchesHubFilter(event, { categories: ['hitl'] })).toBe(false);
		expect(matchesHubFilter(event, { userRelevant: true })).toBe(false);
		expect(event.ref('task_id')).toBe('server-task');
	});

	it('falls back to the payload for refs the server did not send', () => {
		const event = toHubEvent({
			event_type: 'AgentEvent',
			data: {
				event: {
					event_type: 'plan.snapshot',
					agent_id: 'agent-x',
					payload: { task_id: 'task-1', execution_id: 'exec-1', nested: { chat_turn_id: 'turn-9' } }
				}
			},
			refs: { session_id: 's1' }
		});
		expect(event.refs).toEqual({
			task_id: 'task-1',
			execution_id: 'exec-1',
			chat_turn_id: 'turn-9',
			agent_id: 'agent-x',
			session_id: 's1'
		});
		expect(matchesHubFilter(event, { taskId: 'task-1', executionId: ['exec-0', 'exec-1'] })).toBe(true);
		expect(matchesHubFilter(event, { taskId: 'task-2' })).toBe(false);
		expect(matchesHubFilter(event, { chatTurnId: 'turn-9' })).toBe(true);
	});

	it('rejects an id filter when the event carries no such id', () => {
		const event = toHubEvent({ event_type: 'TaskUpdated', data: {} });
		expect(matchesHubFilter(event, { taskId: 't' })).toBe(false);
	});

	it('reads ids from data.item and ProgressEvent message metadata', () => {
		expect(toHubEvent({ event_type: 'FeedItemCreated', data: { item: { task_id: 'ti' } } }).ref('task_id')).toBe('ti');
		expect(
			toHubEvent({ event_type: 'ProgressEvent', data: { message: { metadata: { chat_turn_id: 'ct' } } } }).ref(
				'chat_turn_id'
			)
		).toBe('ct');
		expect(
			toHubEvent({ event_type: 'ChatMessageReceived', data: { chat_session_id: 'cs' } }).ref('session_id')
		).toBe('cs');
	});
});

describe('hub delivery', () => {
	it('delivers matching events, raw and unwrapped, and ignores the rest', () => {
		const hub = makeHub();
		const got: HubEvent[] = [];
		hub.subscribe({ eventTypes: ['tool.'] }, (e) => got.push(e));
		last().open();
		last().receive({ event_type: 'HitlRequested', data: {} });
		const frame = {
			event_type: 'AgentEvent',
			data: { event: { event_type: 'tool.call.finished', agent_id: 'a', payload: { call_id: 'c' } } }
		};
		last().receive(frame);
		expect(got).toHaveLength(1);
		expect(got[0].raw).toEqual(frame);
		expect(got[0].type).toBe('tool.call.finished');
		expect(got[0].payload).toEqual({ call_id: 'c' });
	});

	it('accepts a predicate and stops after unsubscribe', () => {
		const hub = makeHub();
		const got: string[] = [];
		const off = hub.subscribe((e) => e.type === 'TaskUpdated', (e) => got.push(e.type));
		last().open();
		last().receive({ event_type: 'TaskUpdated', data: {} });
		off();
		last().receive({ event_type: 'TaskUpdated', data: {} });
		expect(got).toEqual(['TaskUpdated']);
	});

	it('opens exactly one socket however many features subscribe', () => {
		const hub = makeHub();
		hub.subscribe({}, () => {});
		hub.subscribe({ categories: ['hitl'] }, () => {});
		hub.ensureConnected();
		hub.setReplayTarget('exec-1');
		hub.ensureConnected();
		expect(FakeSocket.instances).toHaveLength(1);
	});

	it('dedupes by (epoch, seq)', () => {
		const hub = makeHub();
		const seqs: number[] = [];
		hub.subscribe({}, (e) => seqs.push(e.seq ?? -1));
		last().open();
		last().receive({ type: 'hub.hello', epoch: 'e1', seq: 0, resumed: false });
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'e1', seq: 1 });
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'e1', seq: 2 });
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'e1', seq: 1 });
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'e1', seq: 3 });
		expect(seqs).toEqual([1, 2, 3]);
		// Frames without seq (pre-hub server) are never deduped.
		last().receive({ event_type: 'TaskUpdated', data: {} });
		last().receive({ event_type: 'TaskUpdated', data: {} });
		expect(seqs).toHaveLength(5);
	});

	it('still feeds raw frames (MUIJ snapshots, acks) to frame listeners', () => {
		const hub = makeHub();
		const frames: unknown[] = [];
		hub.addFrameListener((f) => frames.push(f));
		hub.ensureConnected();
		last().open();
		last().receive({ type: 'agent.ui.snapshot', agent_id: 'a' });
		last().receive({ type: 'hub.hello', epoch: 'e', seq: 0, resumed: false });
		expect(frames).toEqual([{ type: 'agent.ui.snapshot', agent_id: 'a' }]);
	});
});

describe('hub reconnect, resume and resync', () => {
	it('reconnects forever with jittered backoff capped at 30s', () => {
		const hub = makeHub();
		hub.ensureConnected();
		for (let i = 0; i < 25; i += 1) {
			last().serverClose();
			vi.advanceTimersByTime(30_000);
		}
		// Initial dial + one per close: no attempt cap.
		expect(FakeSocket.instances).toHaveLength(26);
		expect(hub.reconnectAttempts()).toBe(25);
	});

	it('never waits longer than 30s between attempts', () => {
		vi.spyOn(Math, 'random').mockReturnValue(0.999);
		const hub = makeHub();
		hub.ensureConnected();
		for (let i = 0; i < 12; i += 1) {
			const before = FakeSocket.instances.length;
			last().serverClose();
			vi.advanceTimersByTime(30_000);
			expect(FakeSocket.instances.length).toBe(before + 1);
		}
		vi.mocked(Math.random).mockRestore();
	});

	it('resumes from the last epoch and seq after the server said hello', () => {
		const hub = makeHub();
		const resyncs: string[] = [];
		hub.subscribe({}, () => {}, (r) => resyncs.push(r));
		expect(params(last()).has('resume_epoch')).toBe(false);
		last().open();
		last().receive({ type: 'hub.hello', epoch: 'boot-1', seq: 10, resumed: false });
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'boot-1', seq: 11 });
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'boot-1', seq: 12 });
		last().serverClose();
		vi.advanceTimersByTime(30_000);
		expect(params(last()).get('resume_epoch')).toBe('boot-1');
		expect(params(last()).get('resume_seq')).toBe('12');
		last().open();
		last().receive({ type: 'hub.hello', epoch: 'boot-1', seq: 14, resumed: true });
		vi.advanceTimersByTime(5_000);
		expect(resyncs).toEqual([]);
	});

	it('resyncs once when the resume could not be satisfied', () => {
		const hub = makeHub();
		const resyncs: string[] = [];
		hub.subscribe({}, () => {}, (r) => resyncs.push(r));
		last().open();
		last().receive({ type: 'hub.hello', epoch: 'boot-1', seq: 5, resumed: false });
		last().serverClose();
		vi.advanceTimersByTime(30_000);
		last().open();
		last().receive({ type: 'hub.hello', epoch: 'boot-2', seq: 0, resumed: false });
		last().receive({ type: 'hub.resync', reason: 'epoch' });
		expect(resyncs).toEqual(['epoch']);
		// A later server resync (lag) is fanned out again.
		last().receive({ event_type: 'TaskUpdated', data: {}, epoch: 'boot-2', seq: 1 });
		last().receive({ type: 'hub.resync', reason: 'lagged' });
		expect(resyncs).toEqual(['epoch', 'lagged']);
	});

	it('fans hub.resync out to every subscriber without closing the socket', () => {
		const hub = makeHub();
		const a = vi.fn();
		const b = vi.fn();
		hub.subscribe({}, () => {}, a);
		hub.subscribe({ categories: ['hitl'] }, () => {}, b);
		hub.subscribe({}, () => {});
		last().open();
		last().receive({ type: 'hub.hello', epoch: 'e', seq: 0, resumed: false });
		last().receive({ type: 'hub.resync', reason: 'lagged' });
		expect(a).toHaveBeenCalledWith('lagged');
		expect(b).toHaveBeenCalledWith('lagged');
		expect(last().closed).toBe(false);
		expect(FakeSocket.instances).toHaveLength(1);
	});

	it('against a pre-hub server: no resume params and a resync on every reconnect', () => {
		const hub = makeHub();
		const resyncs: string[] = [];
		hub.subscribe({}, () => {}, (r) => resyncs.push(r));
		last().open();
		last().receive({ event_type: 'TaskUpdated', data: {} });
		expect(resyncs).toEqual([]); // first connection never resyncs

		last().serverClose();
		vi.advanceTimersByTime(30_000);
		expect(params(last()).has('resume_epoch')).toBe(false);
		last().open();
		last().receive({ event_type: 'TaskUpdated', data: {} });
		expect(resyncs).toEqual(['reconnect']);

		// A quiet legacy server: the hello timeout decides.
		last().serverClose();
		vi.advanceTimersByTime(30_000);
		last().open();
		expect(resyncs).toHaveLength(1);
		vi.advanceTimersByTime(2_000);
		expect(resyncs).toEqual(['reconnect', 'reconnect']);
	});

	it('retries at once when the app returns to the foreground', () => {
		const listeners = new Map<string, () => void>();
		const fakeWindow = {
			addEventListener: (type: string, fn: () => void) => listeners.set(type, fn),
			location: { protocol: 'http:', hostname: 'x', port: '' }
		};
		vi.stubGlobal('window', fakeWindow);
		vi.stubGlobal('document', { visibilityState: 'visible', addEventListener: (t: string, fn: () => void) => listeners.set(t, fn) });
		try {
			const hub = new EventHub({
				webSocket: () => FakeSocket as unknown as typeof WebSocket,
				url: (q) => `ws://test/ws${q}`,
				protocols: () => [],
				bindEnvironment: true
			});
			hub.ensureConnected();
			last().serverClose();
			expect(FakeSocket.instances).toHaveLength(1);
			listeners.get('online')?.();
			expect(FakeSocket.instances).toHaveLength(2);
			last().serverClose();
			listeners.get('visibilitychange')?.();
			expect(FakeSocket.instances).toHaveLength(3);
		} finally {
			vi.unstubAllGlobals();
		}
	});

	it('names a replay target on the next dial without re-targeting an open socket', () => {
		const hub = makeHub();
		hub.ensureConnected();
		last().open();
		hub.setReplayTarget('exec-7', 'agent-3');
		hub.ensureConnected();
		expect(FakeSocket.instances).toHaveLength(1);
		last().serverClose();
		vi.advanceTimersByTime(30_000);
		expect(params(last()).get('execution_id')).toBe('exec-7');
		expect(params(last()).get('agent_id')).toBe('agent-3');
	});
});

describe('subscribeWithBackfill', () => {
	it('applies live events after the backfill settles, and re-backfills on resync', async () => {
		const hub = makeHub();
		const applied: string[] = [];
		let release!: () => void;
		const backfill = vi.fn(
			({ reason }: { reason: string }) =>
				new Promise<void>((resolve) => {
					release = () => {
						applied.push(`backfill:${reason}`);
						resolve();
					};
				})
		);
		const off = subscribeWithBackfill({
			hub,
			filter: { eventTypes: ['TaskUpdated'] },
			backfill,
			onEvent: (e) => applied.push(`live:${(e.data as { n: number }).n}`)
		});
		last().open();
		last().receive({ event_type: 'TaskUpdated', data: { n: 1 } });
		expect(applied).toEqual([]);
		release();
		await vi.advanceTimersByTimeAsync(1);
		expect(applied).toEqual(['backfill:mount', 'live:1']);

		last().receive({ type: 'hub.hello', epoch: 'e', seq: 0, resumed: false });
		last().receive({ type: 'hub.resync', reason: 'lagged' });
		last().receive({ event_type: 'TaskUpdated', data: { n: 2 } });
		release();
		await vi.advanceTimersByTimeAsync(1);
		expect(applied).toEqual(['backfill:mount', 'live:1', 'backfill:lagged', 'live:2']);
		expect(backfill).toHaveBeenCalledTimes(2);
		off();
		last().receive({ event_type: 'TaskUpdated', data: { n: 3 } });
		expect(applied).toHaveLength(4);
	});

	it('still delivers held events when the backfill fails', async () => {
		const hub = makeHub();
		const applied: number[] = [];
		subscribeWithBackfill({
			hub,
			filter: {},
			backfill: async () => {
				throw new Error('boom');
			},
			onEvent: (e) => applied.push((e.data as { n: number }).n)
		});
		last().open();
		last().receive({ event_type: 'TaskUpdated', data: { n: 1 } });
		await vi.advanceTimersByTimeAsync(1);
		expect(applied).toEqual([1]);
	});
});

describe('parseNdjsonFrames', () => {
	it('parses rows and drops control and broken rows', () => {
		const body = [
			JSON.stringify({ event_type: 'HitlRequested', data: {} }),
			'not json',
			JSON.stringify({ event_type: '__events_partial__', data: {} }),
			'',
			JSON.stringify({ event_type: 'HitlResolved', data: {} })
		].join('\n');
		expect(parseNdjsonFrames(body).map((f) => f.event_type)).toEqual(['HitlRequested', 'HitlResolved']);
	});
});

describe('v3EventsPredicate (the /v3/events live-tail filters)', () => {
	const tool = toHubEvent({
		event_type: 'AgentEvent',
		data: { event: { event_type: 'tool.call.started', agent_id: 'a1', payload: { tool_name: 'Web_Fetch' } } }
	});
	it('applies taxonomy filters to classified types and soft-passes unknown ones', () => {
		expect(v3EventsPredicate({ categories: ['tool'] })(tool)).toBe(true);
		expect(v3EventsPredicate({ categories: ['hitl'] })(tool)).toBe(false);
		expect(v3EventsPredicate({ severities: ['error'] })(tool)).toBe(false);
		expect(v3EventsPredicate({ categories: ['hitl'] })(toHubEvent({ event_type: 'Brand.New', data: {} }))).toBe(true);
	});
	it('substring-matches type and search, case-insensitively, and matches the envelope agent', () => {
		expect(v3EventsPredicate({ eventType: 'CALL.start' })(tool)).toBe(true);
		expect(v3EventsPredicate({ search: 'web_fetch' })(tool)).toBe(true);
		expect(v3EventsPredicate({ search: 'nope' })(tool)).toBe(false);
		expect(v3EventsPredicate({ agentId: 'a1' })(tool)).toBe(true);
		expect(v3EventsPredicate({ agentId: 'a2' })(tool)).toBe(false);
	});
	it('serializes a live frame like the backfill line, without hub fields', () => {
		const frame = { event_type: 'X', data: { a: 1 }, seq: 4, epoch: 'e', categories: ['tool'], refs: {} };
		expect(canonicalEventLine(frame)).toBe('{"event_type":"X","data":{"a":1}}');
	});
});
