import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { mediaSessionStore } from '$lib/media/store';
import { defaultCapabilities, defaultPermissions } from '$lib/media/types';
import { screenLockStateStore } from './screenLock';
import { chatHarnessPreferenceStore } from '$lib/stores/chatHarnessPreferenceStore';
import type { RealtimeFrontendProvider, RealtimeProviderCallbacks } from './providers';
import type { VoiceAddressingConfig } from './voiceAddressing';
import { MockWebSocket, settleMicrotasks } from '../../../test/browser';

const provider = vi.hoisted(() => ({
	connect: vi.fn(async () => undefined),
	disconnect: vi.fn(),
	setMicEnabled: vi.fn(),
	setTurnDetection: vi.fn(),
	updateSession: vi.fn(),
	clearInputBuffer: vi.fn(),
	commitInputAndRespond: vi.fn(),
	respondWithTurnContext: vi.fn(),
	interruptResponse: vi.fn(),
	sendToolResult: vi.fn(),
	injectSystemMessage: vi.fn(),
	feedIncomingAudio: vi.fn()
}));
const notifications = vi.hoisted(() => ({ showError: vi.fn() }));
const concurrentInput = vi.hoisted(() => ({ settled: vi.fn() }));

vi.mock('./concurrentVoice', async () => {
	const actual = await vi.importActual<typeof import('./concurrentVoice')>('./concurrentVoice');
	return { ...actual, settleConcurrentVoiceInput: concurrentInput.settled };
});

vi.mock('$lib/shared/stores/notifications', () => notifications);

vi.mock('./providers', async () => {
	const actual = await vi.importActual<typeof import('./providers')>('./providers');
	return {
		...actual,
		pickRealtimeProvider: () => provider as unknown as RealtimeFrontendProvider
	};
});

import {
	clearVoiceTranscript,
	engagePushToTalk,
	pushToTalkActive,
	releasePushToTalk,
	rotateVoiceUpstream,
	sessionCapMsStore,
	setPushToTalkMode,
	startVoiceCall,
	stopVoiceCall,
	voiceCallStore,
	voiceMicAnalyser,
	voiceTranscriptStore
} from './realtimeVoiceClient';

const track = { stop: vi.fn(), getSettings: vi.fn(() => ({ echoCancellation: true })) };

class TestMediaStream {
	getTracks(): Array<{ stop: () => void }> {
		return [track];
	}
	getAudioTracks(): Array<typeof track> {
		return [track];
	}
}

const analyser = { fftSize: 0 };
const audioContext = {
	createMediaStreamSource: vi.fn(() => ({ connect: vi.fn() })),
	createAnalyser: vi.fn(() => analyser),
	close: vi.fn(async () => undefined)
};

class TestAudioContext {
	createMediaStreamSource = audioContext.createMediaStreamSource;
	createAnalyser = audioContext.createAnalyser;
	close = audioContext.close;
}

function registerSession(): void {
	mediaSessionStore.set({
		session_id: 'voice-session-1',
		principal: 'anonymous',
		workspace: 'default',
		thread_id: 'general',
		surface_type: 'web_desktop',
		transport: 'websocket',
		status: 'connected',
		capabilities: { ...defaultCapabilities(), realtime_voice: true, mic: true },
		permissions: { ...defaultPermissions(), mic: 'granted' },
		created_at_ms: 1,
		last_seen_at_ms: 1
	});
}

async function startAndOpen(
	mode: 'realtime' | 'hands_free' = 'realtime',
	realtimeProfile?: string
): Promise<MockWebSocket> {
	const start = startVoiceCall({ mode, realtimeProfile });
	await settleMicrotasks(5);
	const socket = MockWebSocket.instances.at(-1);
	if (!socket) throw new Error('voice control socket was not created');
	socket.open();
	await start;
	return socket;
}

async function makeProviderReady(
	socket: MockWebSocket,
	addressing?: VoiceAddressingConfig,
	perTurnContext?: { enabled: boolean; budget_ms: number }
): Promise<void> {
	socket.receive(
		JSON.stringify({
			kind: 'session.ready',
			payload: {
				voice_session_id: 'voice-session-1',
				descriptor: {
					provider: 'openai',
					model: 'gpt-realtime',
					topology: 'direct_peer_to_peer',
					voice: 'verse',
					transcription_model: 'whisper-1',
					max_session_duration_secs: 900
				},
				rotation_count: 0,
				instructions: 'Be concise.',
				tools: [],
				addressing,
				per_turn_context: perTurnContext
			}
		})
	);
	await settleMicrotasks(8);
}

function providerCallbacks(): RealtimeProviderCallbacks {
	const calls = provider.connect.mock.calls as unknown as Array<
		[{ callbacks: RealtimeProviderCallbacks }]
	>;
	const callbacks = calls.at(-1)?.[0].callbacks;
	if (!callbacks) throw new Error('provider callbacks were not registered');
	return callbacks;
}

beforeEach(async () => {
	stopVoiceCall();
	await settleMicrotasks(4);
	setPushToTalkMode(true);
	clearVoiceTranscript();
	mediaSessionStore.clear();
	screenLockStateStore.set('unknown');
	chatHarnessPreferenceStore.select('magician');
	MockWebSocket.instances = [];
	vi.clearAllMocks();
	track.stop.mockClear();
	Object.defineProperty(navigator, 'mediaDevices', {
		configurable: true,
		value: { getUserMedia: vi.fn(async () => new TestMediaStream()) }
	});
	vi.stubGlobal('MediaStream', TestMediaStream);
	vi.stubGlobal('AudioContext', TestAudioContext);
	registerSession();
});

describe('realtime voice lifecycle', () => {
	it('holds PTT capture until its transcript arrives and settles discarded short taps', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const now = vi.spyOn(Date, 'now').mockReturnValue(10_000);
		engagePushToTalk();
		expect(get(voiceTranscriptStore).userSpeaking).toBe(true);
		expect(socket.send.mock.calls.some(([value]) => JSON.parse(String(value)).kind === 'speech.started')).toBe(true);
		now.mockReturnValue(11_000);
		releasePushToTalk();
		expect(get(voiceTranscriptStore).userSpeaking).toBe(false);
		expect(provider.commitInputAndRespond).toHaveBeenCalledOnce();
		expect(concurrentInput.settled).not.toHaveBeenCalled();
		socket.receive(JSON.stringify({ kind: 'transcript.user', payload: { text: 'another question', item_id: 'ptt-1' } }));
		expect(concurrentInput.settled).toHaveBeenCalledOnce();
		engagePushToTalk();
		releasePushToTalk();
		expect(concurrentInput.settled).toHaveBeenCalledTimes(2);
		expect(provider.commitInputAndRespond).toHaveBeenCalledOnce();
		now.mockRestore();
	});

	it('releases discarded open-mic input when switching to PTT', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		setPushToTalkMode(false);
		providerCallbacks().onSpeechStarted();
		setPushToTalkMode(true);
		expect(get(voiceTranscriptStore).userSpeaking).toBe(false);
		expect(concurrentInput.settled).toHaveBeenCalledOnce();
	});

	it('settles empty transcription without releasing a newer active capture', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();
		callbacks.onSpeechStarted(); callbacks.onSpeechStopped();
		callbacks.onTranscriptUserFinal({ text: '', itemId: 'noise' });
		expect(concurrentInput.settled).toHaveBeenCalledOnce();
		callbacks.onSpeechStarted();
		callbacks.onTranscriptUserFinal({ text: '', itemId: 'older-noise' });
		expect(concurrentInput.settled).toHaveBeenCalledOnce();
	});

	it('opens one scoped control socket and becomes connected after session.ready', async () => {
		const socket = await startAndOpen();

		expect(MockWebSocket.instances).toHaveLength(1);
		expect(socket.url).toContain('/media/voice/voice-session-1/control');
		expect(socket.url).not.toContain('principal=');
		expect(socket.url).not.toContain('workspace=');
		expect(JSON.parse(String(socket.send.mock.calls[0]?.[0]))).toEqual({
			kind: 'session.start',
			payload: {
				ui_thread_id: 'general',
				thread_id: 'general',
				voice_mode: 'realtime',
				concurrent_requests: true,
				turn_boundary: 'push_to_talk',
				echo_cancellation: true,
				require_voice_prefix: false,
				chat_choice: { engine: 'magician', model: 'default' }
			}
		});

		await makeProviderReady(socket);
		expect(provider.connect).toHaveBeenCalledOnce();
		expect(get(voiceCallStore)).toMatchObject({
			state: 'connected',
			model: 'gpt-realtime',
			voice: 'verse',
			error: null
		});
		expect(get(sessionCapMsStore)).toBe(900_000);
		expect(get(voiceMicAnalyser)).toBe(analyser);
	});

	it("carries the composer's chat engine so the call's chat turns think with it", async () => {
		chatHarnessPreferenceStore.select('claude_code', 'opus');
		const socket = await startAndOpen();
		expect(JSON.parse(String(socket.send.mock.calls[0]?.[0])).payload.chat_choice).toEqual({
			engine: 'claude_code',
			model: 'opus'
		});
	});

	it('reports exact initial and live screen-lock transitions to the backend', async () => {
		screenLockStateStore.set('locked');
		const socket = await startAndOpen();
		expect(JSON.parse(String(socket.send.mock.calls[0]?.[0]))).toMatchObject({
			kind: 'session.start',
			payload: { screen_locked: true }
		});

		screenLockStateStore.set('unlocked');
		expect(socket.send.mock.calls.some(([value]) => {
			const envelope = JSON.parse(String(value));
			return envelope.kind === 'screen.state' && envelope.payload.locked === false;
		})).toBe(true);
	});

	it('sends an explicitly selected realtime profile without changing the default path', async () => {
		const socket = await startAndOpen('realtime', 'voice_realtime_gemini_live');
		expect(JSON.parse(String(socket.send.mock.calls[0]?.[0])).payload).toMatchObject({
			voice_mode: 'realtime',
			turn_boundary: 'push_to_talk',
			realtime_profile: 'voice_realtime_gemini_live'
		});
	});

	it('negotiates server VAD for a continuous realtime profile', async () => {
		setPushToTalkMode(false);
		const socket = await startAndOpen('realtime', 'voice_realtime_gemini_translate_en');
		expect(JSON.parse(String(socket.send.mock.calls[0]?.[0])).payload).toMatchObject({
			voice_mode: 'realtime',
			turn_boundary: 'server_vad',
			realtime_profile: 'voice_realtime_gemini_translate_en'
		});
	});

	it('accepts backend-proxied provider transcripts without a frontend transcription model', async () => {
		const socket = await startAndOpen('realtime', 'voice_realtime_gemini_live');
		socket.receive(JSON.stringify({
			kind: 'session.ready',
			payload: {
				voice_session_id: 'voice-session-1',
				descriptor: {
					provider: 'gemini',
					model: 'gemini-3.1-flash-live-preview',
					topology: 'backend_proxied',
					mode: 'assistant'
				},
				instructions: 'Be concise.',
				tools: []
			}
		}));
		await settleMicrotasks(8);
		expect(provider.connect).toHaveBeenCalledOnce();
		expect(get(voiceCallStore).state).toBe('connected');
	});

	it('rotates a backend-proxied call when its live PTT mode changes', async () => {
		const socket = await startAndOpen('realtime', 'voice_realtime_gemini_live');
		socket.receive(JSON.stringify({
			kind: 'session.ready',
			payload: {
				voice_session_id: 'voice-session-1',
				descriptor: {
					provider: 'gemini',
					model: 'gemini-3.1-flash-live-preview',
					topology: 'backend_proxied',
					mode: 'assistant',
					turn_detection_mode: 'none'
				},
				instructions: 'Be concise.',
				tools: []
			}
		}));
		await settleMicrotasks(8);

		setPushToTalkMode(false);

		expect(provider.setTurnDetection).not.toHaveBeenCalled();
		expect(get(voiceCallStore).state).toBe('rotating');
		expect(
			socket.send.mock.calls.some(([value]) => {
				const envelope = JSON.parse(String(value));
				return envelope.kind === 'session.turn_boundary'
					&& envelope.payload.turn_boundary === 'server_vad';
			})
		).toBe(true);
	});

	it('negotiates hands-free mode and clears/mutes playback for half-duplex events', async () => {
		setPushToTalkMode(true);
		const socket = await startAndOpen('hands_free');
		expect(JSON.parse(String(socket.send.mock.calls[0]?.[0])).payload).toMatchObject({
			voice_mode: 'hands_free',
			echo_cancellation: true
		});

		socket.receive(
			JSON.stringify({
				kind: 'session.ready',
				payload: {
					voice_session_id: 'voice-session-1',
					descriptor: {
						provider: 'hands_free',
						model: 'hands-free-local-fluid-v1',
						topology: 'backend_proxied',
						transcription_model: 'fluid-parakeet-eou-en',
						half_duplex: true
					},
					rotation_count: 0,
					instructions: '',
					tools: []
				}
			})
		);
		await settleMicrotasks(8);
		expect(provider.connect).toHaveBeenCalledWith(
			expect.objectContaining({ pushToTalkMode: false })
		);
		engagePushToTalk();
		releasePushToTalk();
		expect(provider.clearInputBuffer).not.toHaveBeenCalled();
		socket.receive(
			JSON.stringify({ kind: 'audio.output.started', payload: { response_id: 'r1' } })
		);
		socket.receive(
			JSON.stringify({ kind: 'audio.output.started', payload: { response_id: 'r2' } })
		);
		socket.receive(
			JSON.stringify({ kind: 'audio.output.ended', payload: { response_id: 'r1' } })
		);
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user',
				payload: { item_id: 'u1', text: 'What is next?' }
			})
		);
		socket.receive(
			JSON.stringify({ kind: 'response.interrupted', payload: { response_id: 'r2' } })
		);
		await settleMicrotasks(5);

		expect(provider.setMicEnabled).toHaveBeenCalledWith(false);
		expect(provider.setMicEnabled).toHaveBeenCalledTimes(3);
		expect(provider.interruptResponse).toHaveBeenCalledOnce();
		expect(provider.setMicEnabled).toHaveBeenLastCalledWith(true);
		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'u1', text: 'What is next?', done: true })
		]);
	});

	it('keeps the live call visible when the server reports a recoverable error', async () => {
		const socket = await startAndOpen('hands_free');
		await makeProviderReady(socket);

		socket.receive(
			JSON.stringify({
				kind: 'session.error',
				payload: { message: 'Temporary transcription delay.', recoverable: true }
			})
		);
		await settleMicrotasks(5);

		expect(get(voiceCallStore)).toMatchObject({
			state: 'connected',
			error: 'Temporary transcription delay.'
		});
		expect(track.stop).not.toHaveBeenCalled();
		expect(socket.readyState).toBe(MockWebSocket.OPEN);
	});

	it('keeps the assistant marked as working between an utterance and its answer', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);

		// Gemini 3.8 Live: "let me check…" ends, the tool is still running.
		socket.receive(
			JSON.stringify({ kind: 'audio.output.started', payload: { response_id: 'r1' } })
		);
		socket.receive(
			JSON.stringify({ kind: 'audio.output.ended', payload: { response_id: 'r1' } })
		);
		socket.receive(
			JSON.stringify({ kind: 'interaction.status', payload: { status: 'in_progress' } })
		);
		await settleMicrotasks(3);
		expect(get(voiceTranscriptStore)).toMatchObject({
			assistantSpeaking: false,
			assistantWorking: true
		});

		// The answer arrives and the interaction closes.
		socket.receive(
			JSON.stringify({ kind: 'audio.output.started', payload: { response_id: 'r2' } })
		);
		socket.receive(
			JSON.stringify({ kind: 'audio.output.ended', payload: { response_id: 'r2' } })
		);
		socket.receive(JSON.stringify({ kind: 'interaction.status', payload: { status: 'idle' } }));
		await settleMicrotasks(3);
		expect(get(voiceTranscriptStore)).toMatchObject({
			assistantSpeaking: false,
			assistantWorking: false
		});

		// An unknown status never claims the assistant is working.
		socket.receive(
			JSON.stringify({ kind: 'interaction.status', payload: { status: 'pondering' } })
		);
		await settleMicrotasks(3);
		expect(get(voiceTranscriptStore).assistantWorking).toBe(false);
	});

	it('turns a guided-takeover failure into a speakable response', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);

		socket.receive(
			JSON.stringify({
				kind: 'tutor.takeover.failed',
				payload: { error: 'screen capture permission denied' }
			})
		);
		await settleMicrotasks(5);

		expect(provider.injectSystemMessage).toHaveBeenCalledWith({
			text: "I couldn't capture the screen, so I didn't start that guided flow. Please check Screen Recording permission and try again.",
			requestResponse: true
		});
		expect(notifications.showError).toHaveBeenCalledWith(
			'Guided voice flow could not start',
			"I couldn't capture the screen, so I didn't start that guided flow. Please check Screen Recording permission and try again."
		);
	});

	it('does not duplicate a failure announcement already injected by the backend', async () => {
		const socket = await startAndOpen('hands_free');
		await makeProviderReady(socket);

		socket.receive(
			JSON.stringify({
				kind: 'tutor.takeover.failed',
				payload: {
					error: 'screen capture permission denied',
					message: 'I could not capture the screen.',
					backend_announced: true
				}
			})
		);
		await settleMicrotasks(5);

		expect(provider.injectSystemMessage).not.toHaveBeenCalled();
		expect(notifications.showError).toHaveBeenCalledWith(
			'Guided voice flow could not start',
			'I could not capture the screen.'
		);
	});

	it('does not swallow a direct-provider failure announcement after takeover', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		socket.receive(JSON.stringify({ kind: 'tutor.takeover.started', payload: {} }));
		socket.receive(JSON.stringify({
			kind: 'tutor.takeover.failed',
			payload: { error: 'screen capture permission denied' }
		}));
		callbacks.onTranscriptAssistantFinal({
			responseId: 'failure-announcement',
			text: "I couldn't capture the screen."
		});

		expect(get(voiceTranscriptStore).turns).toContainEqual(
			expect.objectContaining({
				id: 'assistant-failure-announcement',
				text: "I couldn't capture the screen.",
				done: true
			})
		);
	});

	it('interrupts the realtime answer when a guided visual flow takes over', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();
		callbacks.onTranscriptUserFinal({
			text: 'Tutor screen explain this error',
			itemId: 'guided-user'
		});
		callbacks.onTranscriptAssistantDelta({
			responseId: 'racing-answer',
			text: 'Here is an ordinary answer.'
		});
		// Production echoes the admitted transcript before takeover ack. That
		// envelope must not erase the provider response identity learned above.
		socket.receive(JSON.stringify({
			kind: 'transcript.user',
			payload: { text: 'Tutor screen explain this error', item_id: 'guided-user' }
		}));

		socket.receive(
			JSON.stringify({
				kind: 'tutor.takeover.started',
				payload: {
					feature_mode: 'tutor',
					canvas_mode: 'screen_overlay',
					quick: true
				}
			})
		);
		await settleMicrotasks(5);

		expect(provider.interruptResponse).toHaveBeenCalledOnce();
		expect(get(voiceTranscriptStore).assistantSpeaking).toBe(false);
		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'guided-user', speaker: 'user' })
		]);

		callbacks.onResponseDone({ responseId: 'racing-answer' });
		callbacks.onTranscriptUserFinal({ text: 'What should I do next?', itemId: 'next-user' });
		// The interrupted final may arrive after both terminal provider state and
		// a newer admitted utterance. Its pre-ack delta identity must keep it
		// fenced without consuming the newer response.
		callbacks.onTranscriptAssistantFinal({
			responseId: 'racing-answer',
			text: 'Here is an ordinary answer.'
		});
		callbacks.onTranscriptAssistantFinal({
			responseId: 'next-answer',
			text: 'Here is the next answer.'
		});
		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'guided-user', speaker: 'user' }),
			expect.objectContaining({ id: 'next-user', speaker: 'user' }),
			expect.objectContaining({ id: 'assistant-next-answer', speaker: 'assistant' })
		]);
		expect(socket.send.mock.calls.map(([value]) => JSON.parse(String(value)))).toContainEqual({
			kind: 'transcript.assistant',
			payload: { text: 'Here is the next answer.', response_id: 'next-answer' }
		});
	});

	it('keeps a newer response identity across late events from the prior takeover', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onTranscriptUserFinal({ text: 'Tutor screen first issue', itemId: 'guided-one' });
		callbacks.onTranscriptAssistantDelta({ responseId: 'old-response', text: 'Old answer' });
		socket.receive(JSON.stringify({
			kind: 'transcript.user',
			payload: { text: 'Tutor screen first issue', item_id: 'guided-one' }
		}));
		socket.receive(JSON.stringify({ kind: 'tutor.takeover.started', payload: {} }));
		socket.receive(JSON.stringify({ kind: 'tutor.takeover.completed', payload: {} }));

		callbacks.onTranscriptUserFinal({ text: 'Tutor screen second issue', itemId: 'guided-two' });
		callbacks.onResponseStarted?.({ responseId: 'new-response' });
		callbacks.onTranscriptAssistantDelta({ responseId: 'new-response', text: 'New answer' });
		callbacks.onTranscriptAssistantDelta({ responseId: 'old-response', text: 'Late old answer' });
		callbacks.onTranscriptAssistantFinal({ responseId: 'old-response', text: 'Late old answer' });
		callbacks.onResponseDone({ responseId: 'old-response' });
		expect(get(voiceTranscriptStore).assistantSpeaking).toBe(true);
		socket.receive(JSON.stringify({
			kind: 'transcript.user',
			payload: { text: 'Tutor screen second issue', item_id: 'guided-two' }
		}));
		socket.receive(JSON.stringify({ kind: 'tutor.takeover.started', payload: {} }));
		callbacks.onTranscriptAssistantFinal({ responseId: 'new-response', text: 'New answer' });
		callbacks.onTranscriptAssistantFinal({ responseId: 'later-response', text: 'Allowed later answer' });

		const turns = get(voiceTranscriptStore).turns;
		expect(turns).not.toContainEqual(expect.objectContaining({ id: 'assistant-old-response' }));
		expect(turns).not.toContainEqual(expect.objectContaining({ id: 'assistant-new-response' }));
		expect(turns).toContainEqual(expect.objectContaining({
			id: 'assistant-later-response',
			text: 'Allowed later answer'
		}));
	});

	it('keeps newer response state until its matching lifecycle terminal', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onTranscriptUserFinal({ text: 'Tutor screen first issue', itemId: 'guided-old' });
		callbacks.onTranscriptAssistantDelta({ responseId: 'old-response', text: 'Old answer' });
		socket.receive(JSON.stringify({ kind: 'tutor.takeover.started', payload: {} }));
		socket.receive(JSON.stringify({ kind: 'tutor.takeover.completed', payload: {} }));

		callbacks.onTranscriptUserFinal({ text: 'What is next?', itemId: 'ordinary-new' });
		callbacks.onResponseStarted?.({ responseId: 'new-response' });
		callbacks.onTranscriptAssistantDelta({ responseId: 'new-response', text: 'New answer' });
		callbacks.onTranscriptAssistantFinal({ responseId: 'old-response', text: 'Late old answer' });
		callbacks.onTranscriptAssistantFinal({ responseId: 'new-response', text: 'New answer' });
		callbacks.onResponseDone({ responseId: 'old-response' });

		expect(get(voiceTranscriptStore).assistantSpeaking).toBe(true);
		expect(get(voiceTranscriptStore).turns)
			.not.toContainEqual(expect.objectContaining({ id: 'assistant-old-response' }));
		expect(get(voiceTranscriptStore).turns).toContainEqual(expect.objectContaining({
			id: 'assistant-new-response',
			text: 'New answer'
		}));

		callbacks.onResponseDone({ responseId: 'new-response' });
		expect(get(voiceTranscriptStore).assistantSpeaking).toBe(false);
	});

	it('rejects a late direct-provider tool call owned by the interrupted response', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		socket.receive(JSON.stringify({ kind: 'tutor.takeover.started', payload: {} }));
		callbacks.onFunctionCall({
			responseId: 'cancelled-response',
			callId: 'late-tool',
			name: 'create_task',
			argumentsJson: '{}'
		});

		expect(provider.sendToolResult).toHaveBeenCalledWith({
			callId: 'late-tool',
			output: JSON.stringify({ error: 'guided voice flow owns this turn' })
		});
		expect(socket.send.mock.calls.map(([value]) => JSON.parse(String(value))))
			.not.toContainEqual(expect.objectContaining({ kind: 'tool.dispatch' }));
	});

	it('forwards direct tool calls with their provider response identity', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onFunctionCall({
			responseId: 'current-response',
			callId: 'current-tool',
			name: 'create_task',
			argumentsJson: '{"title":"Investigate"}'
		});

		expect(socket.send.mock.calls.map(([value]) => JSON.parse(String(value)))).toContainEqual({
			kind: 'tool.dispatch',
			payload: {
				response_id: 'current-response',
				tool_name: 'create_task',
				arguments_json: '{"title":"Investigate"}',
				call_id: 'current-tool'
			}
		});
	});

	it('prevents duplicate starts while a call is already active', async () => {
		await startAndOpen();
		await startVoiceCall();

		expect(MockWebSocket.instances).toHaveLength(1);
	});

	it('stops microphone, provider, audio context, and control socket together', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);

		stopVoiceCall();
		await settleMicrotasks(8);

		expect(provider.disconnect).toHaveBeenCalledOnce();
		expect(track.stop).toHaveBeenCalled();
		expect(audioContext.close).toHaveBeenCalled();
		expect(socket.readyState).toBe(MockWebSocket.CLOSED);
		expect(get(voiceCallStore).state).toBe('idle');
		expect(get(voiceMicAnalyser)).toBeNull();
		expect(get(sessionCapMsStore)).toBeNull();
	});

	it('reconnects a dropped control socket without releasing the microphone', async () => {
		vi.useFakeTimers();
		const socket = await startAndOpen();
		await makeProviderReady(socket);

		socket.serverClose();
		await settleMicrotasks(4);

		expect(provider.disconnect).toHaveBeenCalledOnce();
		expect(track.stop).not.toHaveBeenCalled();
		expect(get(voiceCallStore).state).toBe('reconnecting');

		await vi.advanceTimersByTimeAsync(250);
		const replacement = MockWebSocket.instances.at(-1);
		if (!replacement || replacement === socket) throw new Error('replacement socket missing');
		replacement.open();
		await settleMicrotasks(5);
		expect(JSON.parse(String(replacement.send.mock.calls[0]?.[0])).kind).toBe('session.start');
		await makeProviderReady(replacement);
		expect(get(voiceCallStore).state).toBe('connected');
		expect(track.stop).not.toHaveBeenCalled();
		vi.useRealTimers();
	});

	it('retries a transient initial control-socket reset without releasing the microphone', async () => {
		vi.useFakeTimers();
		const start = startVoiceCall({ mode: 'realtime' });
		await settleMicrotasks(5);
		const first = MockWebSocket.instances.at(-1);
		if (!first) throw new Error('initial voice control socket was not created');

		first.serverClose();
		await settleMicrotasks(4);
		expect(MockWebSocket.instances).toHaveLength(1);
		expect(track.stop).not.toHaveBeenCalled();

		await vi.advanceTimersByTimeAsync(250);
		const replacement = MockWebSocket.instances.at(-1);
		if (!replacement || replacement === first) throw new Error('startup retry socket missing');
		replacement.open();
		await start;

		expect(MockWebSocket.instances).toHaveLength(2);
		expect(track.stop).not.toHaveBeenCalled();
		expect(get(voiceCallStore).state).toBe('connecting');
		vi.useRealTimers();
	});

	it('cleans microphone startup when the control socket closes before opening', async () => {
		vi.useFakeTimers();
		const start = startVoiceCall({ mode: 'hands_free' });
		await settleMicrotasks(5);
		const first = MockWebSocket.instances.at(-1);
		if (!first) throw new Error('voice control socket was not created');

		first.serverClose();
		await vi.advanceTimersByTimeAsync(250);
		const second = MockWebSocket.instances.at(-1);
		if (!second || second === first) throw new Error('first startup retry socket missing');
		second.serverClose();
		await vi.advanceTimersByTimeAsync(750);
		const third = MockWebSocket.instances.at(-1);
		if (!third || third === second) throw new Error('second startup retry socket missing');
		third.serverClose();
		await start;

		expect(MockWebSocket.instances).toHaveLength(3);
		expect(track.stop).toHaveBeenCalled();
		expect(audioContext.close).toHaveBeenCalled();
		expect(get(voiceCallStore)).toMatchObject({
			state: 'error',
			error: 'voice control WebSocket closed while opening'
		});
		expect(get(voiceMicAnalyser)).toBeNull();
		vi.useRealTimers();
	});

	it('cancels an in-flight start without allowing a late socket to take ownership', async () => {
		const start = startVoiceCall({ mode: 'hands_free' });
		await settleMicrotasks(5);
		const socket = MockWebSocket.instances.at(-1);
		if (!socket) throw new Error('voice control socket was not created');

		stopVoiceCall();
		await start;

		expect(track.stop).toHaveBeenCalled();
		expect(audioContext.close).toHaveBeenCalled();
		expect(socket.readyState).toBe(MockWebSocket.CLOSED);
		expect(get(voiceCallStore).state).toBe('idle');
		expect(get(voiceMicAnalyser)).toBeNull();
	});

	it('queues push-to-talk before provider readiness and applies it once connected', async () => {
		const socket = await startAndOpen();
		engagePushToTalk();
		expect(get(pushToTalkActive)).toBe(true);

		await makeProviderReady(socket);
		expect(provider.clearInputBuffer).toHaveBeenCalledOnce();
		expect(provider.setMicEnabled).toHaveBeenCalledWith(true);
		releasePushToTalk();
		expect(provider.setMicEnabled).toHaveBeenLastCalledWith(false);
		expect(get(pushToTalkActive)).toBe(false);
	});

	it('rotates and changes turn detection without opening another call', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);

		expect(await rotateVoiceUpstream('manual')).toBe(true);
		setPushToTalkMode(false);

		expect(provider.setTurnDetection).toHaveBeenCalledWith(false);
		expect(
			socket.send.mock.calls.some(([value]) =>
				String(value).includes('"kind":"session.rotate"')
			)
		).toBe(true);
		expect(MockWebSocket.instances).toHaveLength(1);
	});

	it('acknowledges a live tool catalog only after the provider confirms it', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		socket.receive(
			JSON.stringify({
				kind: 'tool.catalog.update',
				payload: {
					update_id: 'catalog-1',
					previous_policy_snapshot_id: 'policy-a',
					policy_snapshot_id: 'policy-b',
					working_set_generation: 1,
					tools: [
						{
							name: 'browser__open',
							description: 'Open a page',
							parameters: { type: 'object' }
						}
					]
				}
			})
		);
		await settleMicrotasks(3);

		expect(provider.updateSession).toHaveBeenCalledWith(
			expect.objectContaining({
				updateId: 'catalog-1',
				sessionConfig: expect.objectContaining({
					tools: [expect.objectContaining({ name: 'browser__open' })]
				})
			})
		);
		expect(
			socket.send.mock.calls
				.map(([value]) => JSON.parse(String(value)))
				.some((frame) => frame.kind === 'tool.catalog.ack')
		).toBe(false);

		callbacks.onSessionConfigured({ updateId: 'catalog-1' });

		expect(
			socket.send.mock.calls
				.map(([value]) => JSON.parse(String(value)))
				.find((frame) => frame.kind === 'tool.catalog.ack')
		).toEqual({
			kind: 'tool.catalog.ack',
			payload: { update_id: 'catalog-1' }
		});
		expect(provider.sendToolResult).not.toHaveBeenCalled();
	});

	it('holds a direct-provider response until bounded current-turn context is ready', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket, undefined, { enabled: true, budget_ms: 300 });
		const callbacks = providerCallbacks();

		expect(provider.connect).toHaveBeenCalledWith(
			expect.objectContaining({
				sessionConfig: expect.objectContaining({ deferResponseUntilContext: true })
			})
		);

		callbacks.onSpeechStarted();
		callbacks.onTranscriptUserFinal({
			text: 'What did we decide about the launch?',
			itemId: 'voice-user-7'
		});

		const controlsBeforeContext = socket.send.mock.calls.map(([value]) =>
			JSON.parse(String(value))
		);
		expect(controlsBeforeContext).toContainEqual({ kind: 'speech.started', payload: {} });
		expect(controlsBeforeContext).toContainEqual({
			kind: 'transcript.user',
			payload: {
				text: 'What did we decide about the launch?',
				item_id: 'voice-user-7'
			}
		});
		expect(provider.respondWithTurnContext).not.toHaveBeenCalled();

		socket.receive(
			JSON.stringify({
				kind: 'turn.context.ready',
				payload: {
					generation: 7,
					context_item_id: 'voice-context-7',
					context: 'Launch decision: stage the rollout.',
					retrieval: { status: 'ready', elapsed_ms: 42, budget_ms: 300 }
				}
			})
		);
		await settleMicrotasks(3);

		expect(provider.respondWithTurnContext).toHaveBeenCalledOnce();
		expect(provider.respondWithTurnContext).toHaveBeenCalledWith({
			contextItemId: 'voice-context-7',
			context: 'Launch decision: stage the rollout.'
		});
	});

	it('preserves the established direct-provider behavior when the gate is disabled', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket, undefined, { enabled: false, budget_ms: 300 });

		expect(provider.connect).toHaveBeenCalledWith(
			expect.objectContaining({
				sessionConfig: expect.objectContaining({ deferResponseUntilContext: false })
			})
		);
		socket.receive(
			JSON.stringify({
				kind: 'turn.context.ready',
				payload: { context_item_id: 'must-be-ignored', context: 'stale context' }
			})
		);
		await settleMicrotasks(3);

		expect(provider.respondWithTurnContext).not.toHaveBeenCalled();
	});

	it('forwards direct response timing and exact billing buckets to the scoped backend', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onSpeechStopped();
		callbacks.onResponseStarted?.({ responseId: 'billing-response' });
		callbacks.onResponseDone({
			responseId: 'billing-response',
			inputTokens: 14,
			outputTokens: 9,
			usage: {
				text_input_tokens: 3,
				text_cached_input_tokens: 2,
				text_output_tokens: 1,
				audio_input_tokens: 5,
				audio_cached_input_tokens: 4,
				audio_output_tokens: 8
			}
		});

		const frames = socket.send.mock.calls.map(([value]) => JSON.parse(String(value)));
		expect(frames).toContainEqual({ kind: 'speech.stopped', payload: {} });
		expect(frames).toContainEqual({
			kind: 'response.started',
			payload: { response_id: 'billing-response' }
		});
		expect(frames).toContainEqual({
			kind: 'token.usage',
			payload: {
				response_id: 'billing-response',
				input_tokens: 14,
				output_tokens: 9,
				usage: {
					text_input_tokens: 3,
					text_cached_input_tokens: 2,
					text_output_tokens: 1,
					audio_input_tokens: 5,
					audio_cached_input_tokens: 4,
					audio_output_tokens: 8
				}
			}
		});
	});

	it('closes successful direct responses even when the provider omits usage', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onResponseStarted?.();
		callbacks.onResponseDone({});

		const frames = socket.send.mock.calls.map(([value]) => JSON.parse(String(value)));
		expect(frames).toContainEqual({ kind: 'response.started', payload: {} });
		expect(frames).toContainEqual({ kind: 'token.usage', payload: {} });
	});

	it.each(['failed', 'cancelled', 'incomplete'] as const)(
		'forwards content-free direct response terminal state %s',
		async (terminalState) => {
			const socket = await startAndOpen();
			await makeProviderReady(socket);
			const callbacks = providerCallbacks();

			callbacks.onResponseStarted?.({ responseId: 'terminal-response' });
			callbacks.onResponseFailed?.({
				responseId: 'terminal-response',
				terminalState
			});

			const frames = socket.send.mock.calls.map(([value]) => JSON.parse(String(value)));
			expect(frames).toContainEqual({
				kind: 'response.failed',
				payload: {
					response_id: 'terminal-response',
					terminal_state: terminalState
				}
			});
		}
	);

	it('rotates once for a terminal provider failure without leaking provider detail', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onResponseStarted?.();
		callbacks.onProviderError({ message: 'sensitive upstream detail', recoverable: false });
		callbacks.onProviderError({ message: 'duplicate terminal event', recoverable: false });

		const frames = socket.send.mock.calls.map(([value]) => JSON.parse(String(value)));
		expect(frames.filter((frame) => frame.kind === 'response.failed')).toEqual([{
			kind: 'response.failed',
			payload: { terminal_state: 'failed' }
		}]);
		expect(frames.filter((frame) => frame.kind === 'session.rotate')).toEqual([{
			kind: 'session.rotate',
			payload: { reason: 'reconnect' }
		}]);
		expect(get(voiceCallStore)).toMatchObject({
			state: 'reconnecting',
			error: null
		});
		expect(frames).not.toContainEqual(expect.objectContaining({
			payload: expect.objectContaining({ message: 'sensitive upstream detail' })
		}));
	});

	it('keeps a recoverable provider turn error on the existing peer', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const callbacks = providerCallbacks();

		callbacks.onProviderError({
			message: 'Input audio buffer is empty.',
			recoverable: true,
			code: 'input_audio_buffer_commit_empty'
		});

		const frames = socket.send.mock.calls.map(([value]) => JSON.parse(String(value)));
		expect(frames.some((frame) => frame.kind === 'session.rotate')).toBe(false);
		expect(frames.some((frame) => frame.kind === 'response.failed')).toBe(false);
		expect(provider.disconnect).not.toHaveBeenCalled();
		expect(get(voiceCallStore)).toMatchObject({
			state: 'connected',
			error: 'That voice turn could not be completed. Please try again.'
		});
	});

	it('preserves the original call timer across an audio rebind', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket);
		const connectedAt = get(voiceCallStore).connectedAt;
		expect(connectedAt).not.toBeNull();

		socket.receive(JSON.stringify({ kind: 'session.rotating', payload: { reason: 'reconnect' } }));
		socket.receive(JSON.stringify({
			kind: 'audio.rebind',
			payload: {
				descriptor: {
					provider: 'openai',
					model: 'gpt-realtime',
					topology: 'direct_peer_to_peer',
					voice: 'verse',
					transcription_model: 'whisper-1'
				},
				resume: { summary: null, recent_turns: [], total_turns: 0 },
				rotation_count: 1
			}
		}));
		await settleMicrotasks(8);

		expect(provider.connect).toHaveBeenCalledTimes(2);
		expect(get(voiceCallStore)).toMatchObject({
			state: 'connected',
			connectedAt
		});
	});

	it('promotes a backend local partial to one finalized user caption', async () => {
		const socket = await startAndOpen();
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.partial',
				payload: { item_id: 'local-turn-1', text: 'send the' }
			})
		);
		await settleMicrotasks(2);

		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'local-turn-1', text: 'send the', done: false })
		]);

		socket.receive(
			JSON.stringify({
				kind: 'transcript.user',
				payload: { item_id: 'local-turn-1', text: 'send the update' }
			})
		);
		await settleMicrotasks(2);

		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'local-turn-1', text: 'send the update', done: true })
		]);
	});

	it('removes the exact local partial when the backend ignores it', async () => {
		const socket = await startAndOpen();
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.partial',
				payload: { item_id: 'local-turn-ignored', turn_generation: 4, text: 'room speech' }
			})
		);
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.ignored',
				payload: {
					item_id: 'local-turn-ignored',
					turn_generation: 4,
					reason: 'address_prefix_required'
				}
			})
		);
		await settleMicrotasks(3);

		expect(get(voiceTranscriptStore).turns).toEqual([]);
		expect(get(voiceTranscriptStore).lastIgnoredTurn).toMatchObject({
			reason: 'address_prefix_required'
		});
	});

	it('silently clears a textless local turn without reporting a rejection', async () => {
		const socket = await startAndOpen();
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.partial',
				payload: { item_id: 'local-turn-empty', turn_generation: 5, text: 'background' }
			})
		);
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.cleared',
				payload: {
					item_id: 'local-turn-empty',
					turn_generation: 5,
					reason: 'no_final_transcript',
					had_partial: true
				}
			})
		);
		await settleMicrotasks(3);

		expect(get(voiceTranscriptStore).turns).toEqual([]);
		expect(get(voiceTranscriptStore).lastIgnoredTurn).toBeNull();
		expect(provider.interruptResponse).not.toHaveBeenCalled();
	});

	it('keeps legacy textless cleanup silent when the backend still sends ignored', async () => {
		const socket = await startAndOpen();
		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.ignored',
				payload: { item_id: 'legacy-empty', reason: 'no_final_transcript' }
			})
		);
		await settleMicrotasks(3);

		expect(get(voiceTranscriptStore).lastIgnoredTurn).toBeNull();
	});

	it.each(['session.ended', 'fatal-error'])(
		'clears unfinished user captions on %s',
		async (ending) => {
			const socket = await startAndOpen();
			socket.receive(
				JSON.stringify({
					kind: 'transcript.user.partial',
					payload: { item_id: 'unfinished', text: 'not final' }
				})
			);
			if (ending === 'session.ended') {
				socket.receive(JSON.stringify({ kind: 'session.ended', payload: {} }));
			} else {
				socket.receive(
					JSON.stringify({
						kind: 'session.error',
						payload: { message: 'fatal', recoverable: false }
					})
				);
			}
			await settleMicrotasks(5);

			expect(get(voiceTranscriptStore).turns).toEqual([]);
		}
	);

	it('publishes every backend activation phrase, not just the first', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket, {
			required: true,
			activation_phrases: ['Hey magical', 'Hey magician']
		});

		const call = get(voiceCallStore);
		expect(call.activationPhrases).toEqual(['Hey magical', 'Hey magician']);
		expect(call.activationPhrase).toBe('Hey magical'); // first, for compact surfaces
		expect(call.addressingRequired).toBe(true);
	});

	it('reports the gate as NOT applied when the backend advertises no phrases', async () => {
		const socket = await startAndOpen();
		// The backend leaves a call ungated when it has no name to listen for,
		// even though the user asked for the prefix. The UI must be able to tell
		// that apart from "the user turned the gate off".
		await makeProviderReady(socket, { required: false, activation_phrases: [] });

		const call = get(voiceCallStore);
		expect(call.addressingRequired).toBe(false);
		expect(call.activationPhrases).toEqual([]);
		expect(call.activationPhrase).toBeNull();
	});

	it('strips the required assistant prefix before rendering an addressed turn', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket, {
			required: true,
			activation_phrases: ['Hey Sam']
		});
		const callbacks = providerCallbacks();

		callbacks.onTranscriptUserFinal({ text: 'Hey, Sam: send the update.', itemId: 'u-prefix' });

		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'u-prefix', text: 'send the update.', done: true })
		]);
		expect(get(voiceCallStore).activationPhrase).toBe('Hey Sam');
		expect(
			socket.send.mock.calls
				.map(([value]) => JSON.parse(String(value)))
				.find((frame) => frame.kind === 'transcript.user')
		).toEqual({
			kind: 'transcript.user',
			payload: { text: 'Hey, Sam: send the update.', item_id: 'u-prefix' }
		});
	});

	it('interrupts and hides ambient direct-provider speech when a prefix is required', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket, {
			required: true,
			activation_phrases: ['Hey Sam']
		});
		const callbacks = providerCallbacks();

		callbacks.onTranscriptUserFinal({ text: 'Someone close the door.', itemId: 'ambient' });
		callbacks.onTranscriptAssistantDelta({ responseId: 'ambient-response', text: 'Sure.' });
		callbacks.onFunctionCall({
			callId: 'ambient-call',
			name: 'create_task',
			argumentsJson: '{}'
		});

		expect(provider.interruptResponse).toHaveBeenCalledTimes(2);
		expect(provider.sendToolResult).toHaveBeenCalledWith({
			callId: 'ambient-call',
			output: JSON.stringify({ error: 'voice address phrase required' })
		});
		expect(
			socket.send.mock.calls
				.map(([value]) => JSON.parse(String(value)))
				.some((frame) => frame.kind === 'tool.dispatch')
		).toBe(false);
		expect(get(voiceTranscriptStore).turns).toEqual([]);

		socket.receive(
			JSON.stringify({
				kind: 'transcript.user.ignored',
				payload: { item_id: 'ambient', reason: 'address_prefix_required' }
			})
		);
		await settleMicrotasks(3);
		expect(provider.interruptResponse).toHaveBeenCalledTimes(3);
		expect(get(voiceTranscriptStore).turns).toEqual([]);
	});

	it('accepts one command after a prefix-only recognition turn', async () => {
		const socket = await startAndOpen();
		await makeProviderReady(socket, {
			required: true,
			activation_phrases: ['Hey Sam'],
			follow_up_window_ms: 8_000
		});
		const callbacks = providerCallbacks();

		callbacks.onTranscriptUserFinal({ text: 'Hey Sam', itemId: 'prefix-only' });
		callbacks.onTranscriptUserFinal({ text: 'send the update', itemId: 'after-prefix' });

		expect(get(voiceTranscriptStore).turns).toEqual([
			expect.objectContaining({ id: 'after-prefix', text: 'send the update', done: true })
		]);
		expect(provider.interruptResponse).toHaveBeenCalledOnce();
	});

	it('surfaces microphone permission failures without leaking a partial call', async () => {
		Object.defineProperty(navigator, 'mediaDevices', {
			configurable: true,
			value: {
				getUserMedia: vi.fn(async () => {
					throw new DOMException('Permission denied', 'NotAllowedError');
				})
			}
		});

		await startVoiceCall();

		expect(get(voiceCallStore)).toMatchObject({
			state: 'error',
			error: 'Permission denied'
		});
		expect(MockWebSocket.instances).toHaveLength(0);
		expect(get(voiceMicAnalyser)).toBeNull();
	});
});
