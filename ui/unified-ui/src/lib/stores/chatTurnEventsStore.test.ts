import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import {
	chatTurnUsageBaselineStore,
	eventDedupeKey,
	eventTimestampMs,
	fetchEventsPage,
	getEventsForChatTurn,
	parseTurnUsage,
	subscribeToChatTurn,
	releaseChatTurn
} from './chatTurnEventsStore';
import { timedFetch } from '$lib/shared/fetch';
import { eventHub } from '$lib/realtime/eventHub';
import { resetTaskUsage, taskUsage } from '$lib/magician/chat/next/taskUsage';

vi.mock('$lib/shared/fetch', () => ({ timedFetch: vi.fn(), LONG_FETCH_TIMEOUT_MS: 600_000 }));

beforeEach(resetTaskUsage);

describe('chat turn live ownership', () => {
	beforeEach(() => {
		// No real socket in unit tests: frames are injected into the hub.
		vi.stubGlobal('WebSocket', undefined);
		vi.mocked(timedFetch).mockReset();
	});
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	function pendingFetches(signals: AbortSignal[], urls: string[] = []) {
		vi.mocked(timedFetch).mockImplementation((url, init) => {
			const signal = init!.signal!;
			signals.push(signal);
			urls.push(String(url));
			return new Promise((_resolve, reject) =>
				signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')))
			);
		});
	}

	it('backfills each turn with one REST request and opens no stream', () => {
		const signals: AbortSignal[] = [];
		const urls: string[] = [];
		pendingFetches(signals, urls);
		const scope = { principal: 'test', workspace: 'test' };
		for (let i = 0; i < 10; i++) {
			const off = subscribeToChatTurn(scope, 'session', `voice-${i}`);
			off();
			expect(signals[i].aborted).toBe(true);
		}
		const off = subscribeToChatTurn(scope, 'session', 'voice-0');
		expect(signals).toHaveLength(11);
		expect(signals[10].aborted).toBe(false);
		expect(urls.every((u) => /\/turns\/voice-\d\/events\?$/.test(u))).toBe(true);
		expect(urls.some((u) => u.includes('/events/stream'))).toBe(false);
		off();
	});

	it('keeps shared turns until the last reader leaves and fences stale cleanup', () => {
		const signals: AbortSignal[] = [];
		pendingFetches(signals);
		const scope = { principal: 'test', workspace: 'test' };
		const offA = subscribeToChatTurn(scope, 'session', 'shared');
		const offB = subscribeToChatTurn(scope, 'session', 'shared');
		offA();
		expect(signals[0].aborted).toBe(false);
		releaseChatTurn('session', 'shared');
		const offNew = subscribeToChatTurn(scope, 'session', 'shared');
		offB();
		expect(signals[0].aborted).toBe(true);
		expect(signals[1].aborted).toBe(false);
		offNew();
		expect(signals[1].aborted).toBe(true);
	});

	it('routes hub events by chat_turn_id, then by the task the turn named', async () => {
		vi.mocked(timedFetch).mockResolvedValue({ ok: true, json: async () => ({ events: [] }) } as Response);
		const scope = { principal: 'test', workspace: 'test' };
		const off = subscribeToChatTurn(scope, 'session', 'turn-live');
		await Promise.resolve();
		eventHub.ingestFrame({
			event_type: 'AgentEvent',
			data: { event: { event_type: 'tool.call.started', agent_id: 'a', payload: { event_id: 'e1', chat_turn_id: 'turn-live', task_id: 'task-9' } } }
		});
		// No turn id, but the task the turn named.
		eventHub.ingestFrame({ event_type: 'AgenticIterationStarted', data: { task_id: 'task-9', iteration: 1, timestamp: 5 } });
		// Another turn, another task: ignored.
		eventHub.ingestFrame({
			event_type: 'AgentEvent',
			data: { event: { event_type: 'tool.call.started', agent_id: 'a', payload: { event_id: 'e2', chat_turn_id: 'turn-other' } } }
		});
		eventHub.ingestFrame({ event_type: 'AgenticIterationStarted', data: { task_id: 'task-0', iteration: 1, timestamp: 6 } });
		const events = getEventsForChatTurn('turn-live');
		expect(events).toHaveLength(2);
		expect(getEventsForChatTurn('turn-other')).toHaveLength(0);
		off();
		eventHub.ingestFrame({ event_type: 'AgenticIterationStarted', data: { task_id: 'task-9', iteration: 2, timestamp: 7 } });
		expect(getEventsForChatTurn('turn-live')).toHaveLength(2);
		releaseChatTurn('session', 'turn-live');
	});

	it('reconciles stored task usage with live calls and panel deltas through the same hub', async () => {
		const state = {
			overview: { task_id: 'task-usage', execution_id: 'exec-usage' },
			run: { activity_log: [{ id: 'stored', status: 'done', created_at: 1, metadata: {
				event_type: 'llm.succeeded', llm_call_id: 'call-a', input_tokens: 1000, output_tokens: 200
			} }] }, output: {}, debug: {}
		};
		vi.mocked(timedFetch).mockResolvedValue({ ok: true, json: async () => ({ events: [{
			event_type: 'ExecutionPanelDelta', data: {
				chat_turn_id: 'usage-turn', task_id: 'task-usage', execution_id: 'exec-usage', state
			}
		}] }) } as Response);
		const off = subscribeToChatTurn({ principal: 'test', workspace: 'test' }, 'session', 'usage-turn');
		await vi.waitFor(() => expect(get(taskUsage).get('task-usage')?.tokens).toBe(1200));
		for (const callId of ['call-a', 'call-b']) {
			eventHub.ingestFrame({ event_type: 'LLMResponseReceived', data: {
				task_id: 'task-usage', execution_id: 'exec-usage', timestamp: callId === 'call-a' ? 2 : 3,
				correlation: { llm_call_id: callId }, input_tokens: 1000, output_tokens: 200
			} });
		}
		expect(get(taskUsage).get('task-usage')).toMatchObject({ calls: 2, tokens: 2400, activityCount: 1 });
		eventHub.ingestFrame({ event_type: 'ExecutionPanelDelta', data: {
			chat_turn_id: 'usage-turn', task_id: 'task-usage', timestamp: 4,
			state: { ...state, run: { activity_log: [...state.run.activity_log, {
				id: 'new', status: 'done', created_at: 3, metadata: {
					event_type: 'llm.succeeded', llm_call_id: 'call-b', input_tokens: 1000, output_tokens: 200
				}
			}] } }
		} });
		expect(get(taskUsage).get('task-usage')).toMatchObject({ calls: 2, tokens: 2400, activityCount: 2 });
		off();
		releaseChatTurn('session', 'usage-turn');
	});

	it('re-reads every watched turn when the hub resyncs', async () => {
		const urls: string[] = [];
		vi.mocked(timedFetch).mockImplementation(async (url) => {
			urls.push(String(url));
			return { ok: true, json: async () => ({ events: [] }) } as Response;
		});
		const scope = { principal: 'test', workspace: 'test' };
		const off = subscribeToChatTurn(scope, 'session', 'turn-resync');
		expect(urls).toHaveLength(1);
		eventHub.ingestFrame({ type: 'hub.resync', reason: 'lagged' });
		expect(urls).toHaveLength(2);
		expect(urls[1]).toContain('/turns/turn-resync/events');
		off();
	});
});

describe('chat turn event identity', () => {
	it('prefers an AgentEvent payload event id', () => {
		expect(eventDedupeKey({
			event_type: 'AgentEvent',
			data: { event: { payload: { event_id: 'agent-event-1' } } }
		})).toBe('agent-event-1');
	});

	it('uses ProgressMessage id and then a direct data event id', () => {
		expect(eventDedupeKey({
			event_type: 'ProgressEvent',
			data: { message: { id: 'progress-1' } }
		})).toBe('progress-1');
		expect(eventDedupeKey({
			event_type: 'CustomEvent',
			data: { event_id: 'direct-1' }
		})).toBe('direct-1');
	});

	it('builds a stable typed-transport fallback from execution discriminators', () => {
		expect(eventDedupeKey({
			event_type: 'LLMRequestSent',
			data: {
				agent_id: 'forge',
				execution_id: 'exec-1',
				iteration: 3,
				timestamp_ms: 1234
			}
		})).toBe('forge|LLMRequestSent|exec-1|3|1234');
	});

	it('uses inner envelope type and agent when present', () => {
		expect(eventDedupeKey({
			event_type: 'AgentEvent',
			data: { event: { event_type: 'StepStarted', agent_id: 'sleuth', timestamp: 88 } }
		})).toBe('sleuth|StepStarted|||88');
	});

	it('supports call and step discriminators', () => {
		expect(eventDedupeKey({
			event_type: 'ToolCalled',
			data: { execution_id: 'e', call_id: 'call-1', timestamp: 7 }
		})).toBe('|ToolCalled|e|call-1|7');
		expect(eventDedupeKey({
			event_type: 'StepUpdated',
			data: { execution_id: 'e', step_index: 0, timestamp: 8 }
		})).toBe('|StepUpdated|e|0|8');
	});

	it('returns null only when no event type or explicit id exists', () => {
		expect(eventDedupeKey({ data: { timestamp: 1 } })).toBeNull();
		expect(eventDedupeKey({})).toBeNull();
	});
});

describe('chat turn event timestamps', () => {
	it.each([
		[
			{ data: { event: { payload: { timestamp_ms: 11 }, timestamp: 12 } } },
			11
		],
		[
			{ data: { event: { timestamp: 12 } } },
			12
		],
		[
			{ data: { event: { timestamp_ms: 13 } } },
			13
		],
		[
			{ data: { timestamp_ms: 14 } },
			14
		],
		[
			{ data: { timestamp: 15 } },
			15
		],
		[
			{ timestamp_ms: 16 },
			16
		]
	] as const)('reads timestamp from supported envelope %j', (event, expected) => {
		expect(eventTimestampMs(event)).toBe(expected);
	});

	it('ignores strings, infinities, and missing values', () => {
		expect(eventTimestampMs({ data: { timestamp_ms: '17' } })).toBeNull();
		expect(eventTimestampMs({ data: { timestamp_ms: Number.POSITIVE_INFINITY } })).toBeNull();
		expect(eventTimestampMs({})).toBeNull();
	});
});

describe('server whole-turn usage', () => {
	const scope = { principal: 'test', workspace: 'test' };
	const row = (id: string, ts: number) => ({
		event_type: 'AgentEvent',
		data: { event: { event_type: 'llm.succeeded', payload: { event_id: id, timestamp_ms: ts } } }
	});
	function respond(body: unknown) {
		vi.mocked(timedFetch).mockResolvedValueOnce({
			ok: true,
			json: async () => body
		} as unknown as Response);
	}
	beforeEach(() => vi.mocked(timedFetch).mockReset());

	it('parses the usage object and ignores a missing or malformed one', () => {
		expect(
			parseTurnUsage(
				{
					calls: 25,
					input_tokens: 10,
					output_tokens: 4,
					cost_usd: 0.5,
					cache_read_tokens: 6,
					cache_creation_tokens: null
				},
				true
			)
		).toEqual({
			calls: 25,
			inputTokens: 10,
			outputTokens: 4,
			costUsd: 0.5,
			cacheReadTokens: 6,
			cacheCreationTokens: null,
			truncated: true
		});
		expect(parseTurnUsage({ calls: 2 }, undefined)).toEqual({
			calls: 2,
			inputTokens: 0,
			outputTokens: 0,
			costUsd: null,
			cacheReadTokens: null,
			cacheCreationTokens: null,
			truncated: false
		});
		expect(parseTurnUsage(undefined, true)).toBeNull();
		expect(parseTurnUsage({ input_tokens: 3 }, true)).toBeNull();
	});

	it('records the totals with every merged row marked as covered', async () => {
		respond({
			events: [row('e-1', 1), row('e-2', 2)],
			truncated: true,
			usage: { calls: 25, input_tokens: 1000, output_tokens: 200, cost_usd: 1.25 }
		});
		await fetchEventsPage(scope, 'session', 'usage-turn');
		const baseline = get(chatTurnUsageBaselineStore).get('usage-turn');
		expect(baseline?.calls).toBe(25);
		expect(baseline?.truncated).toBe(true);
		expect(baseline?.costUsd).toBe(1.25);
		expect([...(baseline?.coveredKeys ?? [])].sort()).toEqual(['e-1', 'e-2']);
		releaseChatTurn('session', 'usage-turn');
		expect(get(chatTurnUsageBaselineStore).has('usage-turn')).toBe(false);
	});

	it('records nothing for an older server without usage', async () => {
		respond({ events: [row('e-3', 3)] });
		await fetchEventsPage(scope, 'session', 'legacy-turn');
		expect(get(chatTurnUsageBaselineStore).has('legacy-turn')).toBe(false);
		releaseChatTurn('session', 'legacy-turn');
	});
});
