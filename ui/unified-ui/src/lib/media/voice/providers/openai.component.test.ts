import { beforeEach, describe, expect, it, vi } from 'vitest';

import { OpenAiRealtimeFrontendProvider } from './openai';
import type { ProviderSessionConfig, RealtimeProviderCallbacks } from './types';

interface OpenAiProviderHarness {
	state: {
		peer: { getSenders(): never[] };
		dataChannel: { readyState: string; send(payload: string): void };
		assistantBuffers: Map<string, string>;
		userBuffers: Map<string, string>;
	};
	callbacks: RealtimeProviderCallbacks;
	deferResponseUntilContext: boolean;
	handleDataChannelEvent(event: Record<string, unknown>): void;
	replayResume(resume: {
		summary: string | null;
		recent_turns: Array<{ role: string; text: string }>;
		tool_exchanges: Array<{
			call_id: string;
			tool_name: string;
			arguments: unknown;
			projected_result: unknown;
		}>;
	}): void;
}

function callbacks(): RealtimeProviderCallbacks {
	return {
		onSessionConfigured: vi.fn(),
		onPlaybackStateChanged: vi.fn(),
		onSpeechStarted: vi.fn(),
		onSpeechStopped: vi.fn(),
		onTranscriptUserFinal: vi.fn(),
		onTranscriptAssistantDelta: vi.fn(),
		onTranscriptAssistantFinal: vi.fn(),
		onResponseStarted: vi.fn(),
		onResponseDone: vi.fn(),
		onResponseFailed: vi.fn(),
		onFunctionCall: vi.fn(),
		onProviderError: vi.fn()
	};
}

function harness(deferResponseUntilContext = true): {
	provider: OpenAiRealtimeFrontendProvider;
	wire: Array<Record<string, unknown>>;
	callbacks: RealtimeProviderCallbacks;
} {
	const wire: Array<Record<string, unknown>> = [];
	const provider = new OpenAiRealtimeFrontendProvider();
	const providerCallbacks = callbacks();
	const internals = provider as unknown as OpenAiProviderHarness;
	internals.state = {
		peer: { getSenders: () => [] },
		dataChannel: {
			readyState: 'open',
			send(payload: string) {
				wire.push(JSON.parse(payload));
			}
		},
		assistantBuffers: new Map(),
		userBuffers: new Map()
	};
	internals.callbacks = providerCallbacks;
	internals.deferResponseUntilContext = deferResponseUntilContext;
	return { provider, wire, callbacks: providerCallbacks };
}

const sessionConfig = (deferResponseUntilContext: boolean): ProviderSessionConfig => ({
	instructions: 'Stable cached session instructions',
	tools: [],
	transcriptionModel: 'whisper-1',
	turnDetectionMode: 'server_vad',
	deferResponseUntilContext
});

describe('OpenAI realtime current-turn response gate', () => {
	beforeEach(() => vi.clearAllMocks());

	it('reports empty completed transcription so capture can settle', () => {
		const { provider, callbacks } = harness();
		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'conversation.item.input_audio_transcription.completed', item_id: 'noise', transcript: ''
		});
		expect(callbacks.onTranscriptUserFinal).toHaveBeenCalledWith({ text: '', itemId: 'noise' });
	});

	it('uses server VAD for open mic even when the minted mode is none', () => {
		const { provider, wire } = harness(false);

		provider.updateSession?.({
			sessionConfig: {
				...sessionConfig(false),
				turnDetectionMode: 'none'
			},
			pushToTalkMode: false,
			updateId: 'open-mic-from-none'
		});

		expect(wire[0]).toMatchObject({
			type: 'session.update',
			event_id: 'open-mic-from-none',
			session: {
				output_modalities: ['audio'],
				audio: {
					input: {
						turn_detection: { type: 'server_vad', create_response: true }
					}
				}
			}
		});
	});

	it('suppresses automatic VAD and PTT responses until context is supplied', () => {
		const { provider, wire } = harness();

		provider.updateSession?.({
			sessionConfig: sessionConfig(true),
			pushToTalkMode: false,
			updateId: 'configure-context-gate'
		});
		provider.commitInputAndRespond();

		expect(wire[0]).toMatchObject({
			type: 'session.update',
			event_id: 'configure-context-gate',
			session: {
				audio: { input: { turn_detection: { create_response: false } } }
			}
		});
		expect(wire.slice(1)).toEqual([{ type: 'input_audio_buffer.commit' }]);
	});

	it('replaces response-scoped context and creates exactly one response per ready signal', () => {
		const { provider, wire, callbacks: providerCallbacks } = harness();

		provider.respondWithTurnContext({
			contextItemId: 'voice-context-1',
			context: 'Relevant memory one'
		});
		provider.respondWithTurnContext({
			contextItemId: 'voice-context-2',
			context: 'Relevant memory two'
		});

		expect(wire).toEqual([
			expect.objectContaining({
				type: 'conversation.item.create',
				item: expect.objectContaining({ id: 'voice-context-1', role: 'system' })
			}),
			{ type: 'response.create' },
			{ type: 'response.cancel' },
			{ type: 'conversation.item.delete', item_id: 'voice-context-1' },
			expect.objectContaining({
				type: 'conversation.item.create',
				item: expect.objectContaining({ id: 'voice-context-2', role: 'system' })
			}),
			{ type: 'response.create' }
		]);
		expect(wire.filter((frame) => frame.type === 'response.create')).toHaveLength(2);

		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'response.done',
			response: {
				id: 'resp-usage',
				status: 'completed',
				usage: {
					input_tokens: 13,
					output_tokens: 7,
					input_token_details: {
						text_tokens: 5,
						audio_tokens: 8,
						cached_tokens_details: { text_tokens: 2, audio_tokens: 3 }
					},
					output_token_details: { text_tokens: 1, audio_tokens: 6 }
				}
			}
		});
		expect(wire.at(-1)).toEqual({
			type: 'conversation.item.delete',
			item_id: 'voice-context-2'
		});
		expect(providerCallbacks.onResponseDone).toHaveBeenCalledWith({
			responseId: 'resp-usage',
			inputTokens: 13,
			outputTokens: 7,
			usage: {
				text_input_tokens: 3,
				text_cached_input_tokens: 2,
				text_output_tokens: 1,
				audio_input_tokens: 5,
				audio_cached_input_tokens: 3,
				audio_output_tokens: 6
			}
		});
	});

	it('surfaces response creation as the direct-transport timing anchor', () => {
		const { provider, callbacks: providerCallbacks } = harness();
		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'response.created',
			response: { id: 'resp-1' }
		});
		expect(providerCallbacks.onResponseStarted).toHaveBeenCalledWith({
			responseId: 'resp-1'
		});
	});

	it('preserves response identity on direct function calls', () => {
		const { provider, callbacks: providerCallbacks } = harness();
		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'response.function_call_arguments.done',
			response_id: 'resp-tool',
			call_id: 'call-1',
			name: 'lookup',
			arguments: '{"q":"coffee"}'
		});

		expect(providerCallbacks.onFunctionCall).toHaveBeenCalledWith({
			responseId: 'resp-tool',
			callId: 'call-1',
			name: 'lookup',
			argumentsJson: '{"q":"coffee"}'
		});
	});

	it('aliases overlong durable call IDs only for balanced OpenAI resume replay', () => {
		const { provider, wire } = harness();
		const canonicalCallId = `call_${'x'.repeat(55)}`;
		expect(canonicalCallId).toHaveLength(60);

		(provider as unknown as OpenAiProviderHarness).replayResume({
			summary: null,
			recent_turns: [],
			tool_exchanges: [{
				call_id: canonicalCallId,
				tool_name: 'search_memory',
				arguments: { query: 'birthday' },
				projected_result: { status: 'ok' }
			}]
		});

		const callId = String((wire[0].item as Record<string, unknown>).call_id);
		expect(callId).not.toBe(canonicalCallId);
		expect(callId.length).toBeLessThanOrEqual(32);
		expect((wire[1].item as Record<string, unknown>).call_id).toBe(callId);
		expect(wire.map((frame) => (frame.item as Record<string, unknown>).type)).toEqual([
			'function_call',
			'function_call_output'
		]);
	});

	it('keeps valid coarse totals but omits an incomplete or inconsistent modality split', () => {
		const { provider, callbacks: providerCallbacks } = harness();
		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'response.done',
			response: {
				id: 'resp-coarse',
				status: 'completed',
				usage: {
					input_tokens: 13,
					output_tokens: 7,
					input_token_details: {
						text_tokens: 5,
						audio_tokens: 8,
						cached_tokens_details: { text_tokens: 2 }
					},
					output_token_details: { text_tokens: 1, audio_tokens: 6 }
				}
			}
		});

		expect(providerCallbacks.onResponseDone).toHaveBeenCalledWith({
			responseId: 'resp-coarse',
			inputTokens: 13,
			outputTokens: 7,
			usage: undefined
		});
	});

	it.each([
		['failed', 'failed'],
		['cancelled', 'cancelled'],
		['incomplete', 'incomplete'],
		['unexpected', 'failed']
	] as const)('normalizes response.done status %s without reporting success', (status, expected) => {
		const { provider, callbacks: providerCallbacks } = harness();
		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'response.done',
			response: { id: 'resp-terminal', status }
		});

		expect(providerCallbacks.onResponseFailed).toHaveBeenCalledWith({
			responseId: 'resp-terminal',
			terminalState: expected
		});
		expect(providerCallbacks.onResponseDone).not.toHaveBeenCalled();
	});

	it('treats response.done without explicit completed status as failed', () => {
		const { provider, callbacks: providerCallbacks } = harness();
		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'response.done',
			response: { id: 'resp-malformed' }
		});

		expect(providerCallbacks.onResponseFailed).toHaveBeenCalledWith({
			responseId: 'resp-malformed',
			terminalState: 'failed'
		});
		expect(providerCallbacks.onResponseDone).not.toHaveBeenCalled();
	});

	it('fails open with one response and no context item after timeout or empty retrieval', () => {
		const { provider, wire } = harness();

		provider.respondWithTurnContext({ contextItemId: 'unused', context: null });

		expect(wire).toEqual([{ type: 'response.create' }]);
	});

	it('keeps the pre-feature response path unchanged when deferral is off', () => {
		const { provider, wire } = harness(false);

		provider.commitInputAndRespond();

		expect(wire).toEqual([
			{ type: 'input_audio_buffer.commit' },
			{ type: 'response.create' }
		]);
	});

	it('does not cancel a response that context-gated VAD has not created', () => {
		const { provider, wire } = harness(true);

		provider.interruptResponse();
		provider.interruptResponse();

		expect(wire).toEqual([]);
	});

	it('cancels a requested response at most once before its terminal event', () => {
		const { provider, wire } = harness(true);

		provider.respondWithTurnContext({ contextItemId: 'empty-context', context: null });
		provider.interruptResponse();
		provider.interruptResponse();

		expect(wire).toEqual([
			{ type: 'response.create' },
			{ type: 'response.cancel' }
		]);
	});

	it('classifies Realtime server errors as recoverable request errors', () => {
		const { provider, callbacks: providerCallbacks } = harness();

		(provider as unknown as OpenAiProviderHarness).handleDataChannelEvent({
			type: 'error',
			error: {
				type: 'invalid_request_error',
				code: 'input_audio_buffer_commit_empty',
				message: 'Input audio buffer is empty.',
				event_id: 'client-event-7'
			}
		});

		expect(providerCallbacks.onProviderError).toHaveBeenCalledWith({
			message: 'Input audio buffer is empty.',
			recoverable: true,
			code: 'input_audio_buffer_commit_empty',
			errorType: 'invalid_request_error',
			clientEventId: 'client-event-7'
		});
	});
});

it('keeps physical playback separate from generation completion', () => {
    const f = harness();
    const internals = f.provider as unknown as OpenAiProviderHarness;
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.started', response_id: 'a' });
    internals.handleDataChannelEvent({ type: 'response.done', response: { id: 'a', status: 'completed' } });
    expect(f.callbacks.onResponseDone).toHaveBeenCalledOnce();
    expect(f.callbacks.onPlaybackStateChanged).toHaveBeenCalledTimes(1);
    expect(f.callbacks.onPlaybackStateChanged).toHaveBeenLastCalledWith(true);
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.stopped', response_id: 'a' });
    expect(f.callbacks.onPlaybackStateChanged).toHaveBeenLastCalledWith(false);
});

it('ignores a late playback stop from an earlier response', () => {
    const f = harness();
    const internals = f.provider as unknown as OpenAiProviderHarness;
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.started', response_id: 'a' });
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.started', response_id: 'b' });
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.stopped', response_id: 'a' });
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.cleared', response_id: 'a' });
    expect(f.callbacks.onPlaybackStateChanged).toHaveBeenCalledTimes(2);
    expect(f.callbacks.onPlaybackStateChanged).toHaveBeenLastCalledWith(true);
    internals.handleDataChannelEvent({ type: 'output_audio_buffer.stopped', response_id: 'b' });
    expect(f.callbacks.onPlaybackStateChanged).toHaveBeenLastCalledWith(false);
});
