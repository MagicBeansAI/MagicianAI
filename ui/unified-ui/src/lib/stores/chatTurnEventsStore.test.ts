import { describe, expect, it, vi } from 'vitest';
import { eventDedupeKey, eventTimestampMs, subscribeToChatTurn, releaseChatTurn } from './chatTurnEventsStore';
import { timedFetch } from '$lib/shared/fetch';

vi.mock('$lib/shared/fetch', () => ({ timedFetch: vi.fn(), LONG_FETCH_TIMEOUT_MS: 600_000 }));

describe('chat turn stream ownership', () => {
	it('releases each obsolete voice stream immediately and can resubscribe', async () => {
		const signals: AbortSignal[] = [];
		vi.mocked(timedFetch).mockImplementation((_url, init) => {
			const signal = init!.signal!;
			signals.push(signal);
			return Promise.resolve({ ok: true, body: new ReadableStream({
				start(controller) { signal.addEventListener('abort', () => controller.error(new DOMException('Aborted', 'AbortError'))); }
			}) } as Response);
		});
		const scope = { principal: 'test', workspace: 'test' };
		for (let i = 0; i < 10; i++) {
			const off = subscribeToChatTurn(scope, 'session', `voice-${i}`);
			await Promise.resolve();
			off();
			expect(signals[i].aborted).toBe(true);
		}
		const off = subscribeToChatTurn(scope, 'session', 'voice-0');
		expect(signals).toHaveLength(11);
		expect(signals[10].aborted).toBe(false);
		off();
	});

	it('keeps shared streams until the last reader leaves and fences stale cleanup', () => {
		const signals: AbortSignal[] = [];
		vi.mocked(timedFetch).mockImplementation((_url, init) => {
			const signal = init!.signal!;
			signals.push(signal);
			return new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError'))));
		});
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
