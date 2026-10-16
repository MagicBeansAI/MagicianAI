/**
 * OpenAI Realtime frontend provider — `direct_peer_to_peer` topology.
 *
 * Owns everything OpenAI-specific that used to live inline in the
 * voice client:
 *   - the `oai-events` data channel + its event-kind switch
 *   - the `/v1/realtime/calls` SDP exchange (Bearer + application/sdp)
 *   - every `conversation.item.create` / `input_audio_buffer.*` /
 *     `response.create` shape
 *   - the `session.update` shape (instructions, tools, transcription
 *     model, turn detection mode)
 *
 * Future Gemini Live provider lands as a sibling file
 * (`gemini.ts`) implementing the same `RealtimeFrontendProvider`
 * interface; the client picks via descriptor.provider.
 */

import type {
	ProviderDescriptor,
	ProviderResumeContext,
	ProviderSessionConfig,
	RealtimeFrontendProvider,
	RealtimeProviderCallbacks,
	RealtimeUsageBreakdown
} from './types';

function finiteTokenCount(value: unknown): number | undefined {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
		? value
		: undefined;
}

/** Mirror `magicllm`'s OpenAI Realtime usage normalization at the direct
 * WebRTC boundary. The provider reports modality totals plus a nested cached
 * split; canonical billing stores uncached and cached input independently. */
function parseRealtimeUsage(usage: Record<string, unknown>): RealtimeUsageBreakdown | undefined {
	const input = usage.input_token_details as Record<string, unknown> | undefined;
	const output = usage.output_token_details as Record<string, unknown> | undefined;
	if (!input || !output) return undefined;
	const cached = input?.cached_tokens_details as Record<string, unknown> | undefined;
	if (!cached) return undefined;
	const inputTotal = finiteTokenCount(usage.input_tokens);
	const outputTotal = finiteTokenCount(usage.output_tokens);
	const inputText = finiteTokenCount(input?.text_tokens);
	const inputAudio = finiteTokenCount(input?.audio_tokens);
	const cachedText = finiteTokenCount(cached?.text_tokens);
	const cachedAudio = finiteTokenCount(cached?.audio_tokens);
	const outputText = finiteTokenCount(output?.text_tokens);
	const outputAudio = finiteTokenCount(output?.audio_tokens);
	if (
		inputTotal === undefined ||
		outputTotal === undefined ||
		inputText === undefined ||
		inputAudio === undefined ||
		cachedText === undefined ||
		cachedAudio === undefined ||
		outputText === undefined ||
		outputAudio === undefined ||
		inputText + inputAudio !== inputTotal ||
		outputText + outputAudio !== outputTotal ||
		cachedText > inputText ||
		cachedAudio > inputAudio
	) return undefined;
	return {
		text_input_tokens: Math.max(0, inputText - cachedText),
		text_cached_input_tokens: cachedText,
		text_output_tokens: outputText,
		audio_input_tokens: Math.max(0, inputAudio - cachedAudio),
		audio_cached_input_tokens: cachedAudio,
		audio_output_tokens: outputAudio
	};
}

/** OpenAI Realtime's data-channel label. Stable per the SDK spec. */
const OAI_DATA_CHANNEL_LABEL = 'oai-events';
const OAI_REPLAY_CALL_ID_MAX_LENGTH = 32;

/**
 * Client-created historical function calls have a stricter OpenAI Realtime
 * identity limit than durable tool calls from Responses and other providers.
 * Compact only replay IDs; current-session tool results must retain the exact
 * ID issued by the active realtime provider.
 */
function openAiReplayCallId(canonicalCallId: string): string {
	if (
		canonicalCallId.length > 0 &&
		canonicalCallId.length <= OAI_REPLAY_CALL_ID_MAX_LENGTH &&
		/^[\x20-\x7e]+$/.test(canonicalCallId)
	) return canonicalCallId;

	let first = 0x811c9dc5;
	let second = 0x9e3779b9;
	let third = 0x85ebca6b;
	for (let index = 0; index < canonicalCallId.length; index += 1) {
		const code = canonicalCallId.charCodeAt(index);
		first = Math.imul(first ^ code, 0x01000193) >>> 0;
		second = Math.imul(second ^ code, 0x85ebca6b) >>> 0;
		third = Math.imul(third ^ code, 0xc2b2ae35) >>> 0;
	}
	const hex = (value: number) => value.toString(16).padStart(8, '0');
	return `resume_${hex(first)}${hex(second)}${hex(third)}`;
}

interface InternalState {
	peer: RTCPeerConnection;
	dataChannel: RTCDataChannel;
	assistantBuffers: Map<string, string>;
	userBuffers: Map<string, string>;
}

export class OpenAiRealtimeFrontendProvider implements RealtimeFrontendProvider {
	private state: InternalState | null = null;
	private callbacks: RealtimeProviderCallbacks | null = null;
	/** Profile-supplied VAD mode (e.g. `server_vad`), captured at connect so a
	 *  LIVE push-to-talk ⇄ hands-free toggle can rebuild `turn_detection`
	 *  without a reconnect. */
	private turnDetectionMode = 'server_vad';
	private deferResponseUntilContext = false;
	private voice: string | null = null;
	private activeTurnContextItemId: string | null = null;
	private pendingSessionUpdateIds: Array<string | undefined> = [];
	/** Set from the instant we request a response until its terminal event. This
	 *  prevents `response.cancel` from manufacturing a recoverable provider
	 *  error when context-gated VAD has not created a response yet. */
	private responsePendingOrActive = false;
	private outputAudioActive = false;
	private outputAudioResponseId: string | null = null;

	async connect(args: {
		descriptor: ProviderDescriptor;
		mic: MediaStream;
		playbackEl: HTMLAudioElement;
		pushToTalkMode: boolean;
		sessionConfig: ProviderSessionConfig;
		resume: ProviderResumeContext | null;
		callbacks: RealtimeProviderCallbacks;
		configurationUpdateId?: string;
	}): Promise<void> {
		if (this.state) {
			throw new Error('OpenAiRealtimeFrontendProvider already connected — call disconnect() first');
		}
		const {
			descriptor,
			mic,
			playbackEl,
			pushToTalkMode,
			sessionConfig,
			resume,
			callbacks,
			configurationUpdateId
		} =
			args;
		if (!descriptor.webrtc_url || !descriptor.upstream_token) {
			throw new Error('OpenAI descriptor missing webrtc_url / upstream_token');
		}
		this.callbacks = callbacks;
		this.voice = descriptor.voice?.trim() || null;

		const peer = new RTCPeerConnection();
		const dataChannel = peer.createDataChannel(OAI_DATA_CHANNEL_LABEL);
		const assistantBuffers = new Map<string, string>();
		const userBuffers = new Map<string, string>();

		dataChannel.addEventListener('message', (event) => {
			if (typeof event.data !== 'string') return;
			try {
				this.handleDataChannelEvent(JSON.parse(event.data));
			} catch (err) {
				console.debug('[voice/openai] non-JSON data-channel frame', err);
			}
		});

		dataChannel.addEventListener('open', () => {
			this.applySessionUpdate(sessionConfig, pushToTalkMode, configurationUpdateId);
			if (resume) {
				this.replayResume(resume);
			}
		});

		for (const track of mic.getTracks()) {
			peer.addTrack(track, mic);
			if (pushToTalkMode) track.enabled = false;
		}

		// Detach any prior incoming stream so the new ontrack handler
		// attaches the fresh remote audio cleanly.
		playbackEl.srcObject = null;
		peer.addEventListener('track', (event) => {
			const [stream] = event.streams;
			if (stream) playbackEl.srcObject = stream;
		});

		peer.addEventListener('connectionstatechange', () => {
			// `disconnected` is a transient WebRTC state — ICE often
			// recovers on its own within seconds (especially on
			// mobile networks switching between WiFi and cellular).
			// Only escalate on `failed`, which is terminal.
			if (peer.connectionState === 'failed') {
				this.callbacks?.onProviderError({
					message: 'peer connection failed',
					recoverable: false
				});
			}
		});

		const offer = await peer.createOffer();
		await peer.setLocalDescription(offer);
		const answerSdp = await negotiateSdp({
			url: descriptor.webrtc_url,
			token: descriptor.upstream_token,
			offerSdp: offer.sdp ?? ''
		});
		await peer.setRemoteDescription({ type: 'answer', sdp: answerSdp });

		this.state = { peer, dataChannel, assistantBuffers, userBuffers };
	}

	disconnect(): void {
		const s = this.state;
		this.state = null;
		this.callbacks = null;
		this.voice = null;
		this.pendingSessionUpdateIds = [];
		this.activeTurnContextItemId = null;
		this.responsePendingOrActive = false;
		this.outputAudioActive = false;
		this.outputAudioResponseId = null;
		if (!s) return;
		try { s.dataChannel.close(); } catch { /* no-op */ }
		try { s.peer.close(); } catch { /* no-op */ }
	}

	setMicEnabled(enabled: boolean): void {
		const s = this.state;
		if (!s) return;
		// Mic tracks live on the senders of the peer connection. We
		// flip the `enabled` flag rather than removing the track so
		// the WebRTC connection stays stable through PTT cycles.
		for (const sender of s.peer.getSenders()) {
			const track = sender.track;
			if (track && track.kind === 'audio') {
				track.enabled = enabled;
			}
		}
	}

	/** Switch turn-taking mode on a LIVE call (no reconnect). PTT disables
	 *  server VAD and drops the mic until the next engage; hands-free restores
	 *  the profile VAD mode and re-arms the mic. Mirrors `applySessionUpdate`'s
	 *  `turn_detection` derivation so connect + live-toggle stay consistent. */
	setTurnDetection(pushToTalkMode: boolean): void {
		// A session STARTED in PTT is minted with `turn_detection_mode:
		// "none"` (the backend maps `turn_boundary: push_to_talk` to "none").
		// Treating that as "null even when the user turns PTT OFF" made the
		// toggle a one-way door: mic unmuted, audio flowing, but no VAD — so
		// no turn ever committed and open mic was dead air. A live PTT-OFF
		// toggle is an explicit request for VAD; fall back to server_vad
		// when the minted mode carries none.
		const turnDetection = pushToTalkMode ? null : this.openMicTurnDetection();
		this.pendingSessionUpdateIds.push(undefined);
		this.safeSend({
			type: 'session.update',
			session: {
				type: 'realtime',
				audio: { input: { turn_detection: turnDetection } }
			}
		});
		// Hands-free keeps the mic live; PTT mutes it until the user engages
		// (a pending input buffer is cleared so stray audio isn't committed).
		if (pushToTalkMode) {
			this.clearInputBuffer();
		}
		this.setMicEnabled(!pushToTalkMode);
	}

	updateSession(args: {
		sessionConfig: ProviderSessionConfig;
		pushToTalkMode: boolean;
		updateId: string;
	}): void {
		this.applySessionUpdate(args.sessionConfig, args.pushToTalkMode, args.updateId);
	}

	clearInputBuffer(): void {
		this.safeSend({ type: 'input_audio_buffer.clear' });
	}

	commitInputAndRespond(): void {
		this.safeSend({ type: 'input_audio_buffer.commit' });
		if (!this.deferResponseUntilContext) this.requestResponse();
	}

	respondWithTurnContext(args: { contextItemId: string; context: string | null }): void {
		// A second turn while the previous response is still open is rejected
		// by OpenAI (`conversation already has an active response`). Cancel
		// first so later turns can speak without reconnecting.
		if (this.responsePendingOrActive) {
			this.safeSend({ type: 'response.cancel' });
			this.responsePendingOrActive = false;
		}
		this.clearActiveTurnContext();
		const context = args.context?.trim() ?? '';
		if (context) {
			this.safeSend({
				type: 'conversation.item.create',
				item: {
					id: args.contextItemId,
					type: 'message',
					role: 'system',
					content: [{ type: 'input_text', text: context }]
				}
			});
			this.activeTurnContextItemId = args.contextItemId;
		}
		this.requestResponse();
	}

	interruptResponse(): void {
		// OpenAI reports `response.cancel` with no active response as a
		// recoverable `error`. Context-gated turns intentionally have no response
		// until retrieval completes, so cancellation must be state-aware.
		if (this.responsePendingOrActive) {
			this.safeSend({ type: 'response.cancel' });
			this.responsePendingOrActive = false;
		}
		if (this.outputAudioActive) {
			this.safeSend({ type: 'output_audio_buffer.clear' });
			this.outputAudioActive = false;
		}
		this.clearActiveTurnContext();
	}

	sendToolResult(args: { callId: string; output: string }): void {
		this.safeSend({
			type: 'conversation.item.create',
			item: {
				type: 'function_call_output',
				call_id: args.callId,
				output: args.output
			}
		});
		this.requestResponse();
	}

	injectSystemMessage(args: { text: string; requestResponse: boolean }): void {
		this.safeSend({
			type: 'conversation.item.create',
			item: {
				type: 'message',
				role: 'system',
				content: [{ type: 'input_text', text: args.text }]
			}
		});
		if (args.requestResponse) {
			this.requestResponse();
		}
	}

	// ─── Internal ─────────────────────────────────────────────────

	private safeSend(payload: Record<string, unknown>): void {
		const ch = this.state?.dataChannel;
		if (!ch || ch.readyState !== 'open') return;
		try {
			ch.send(JSON.stringify(payload));
		} catch (err) {
			console.warn('[voice/openai] data-channel send failed', payload.type, err);
		}
	}

	private requestResponse(): void {
		this.responsePendingOrActive = true;
		this.safeSend({ type: 'response.create' });
	}

	private clearActiveTurnContext(): void {
		if (!this.activeTurnContextItemId) return;
		this.safeSend({
			type: 'conversation.item.delete',
			item_id: this.activeTurnContextItemId
		});
		this.activeTurnContextItemId = null;
	}

	private openMicTurnDetection(): {
		type: string;
		create_response: boolean;
	} {
		// A session minted from Hold-to-talk carries `turn_detection_mode:
		// "none"`. Open mic must still get a VAD, or audio flows and no
		// turn is ever committed — the call stays on "Listening".
		const vadMode = this.turnDetectionMode === 'none' ? 'server_vad' : this.turnDetectionMode;
		return {
			type: vadMode,
			create_response: !this.deferResponseUntilContext
		};
	}

	private applySessionUpdate(
		config: ProviderSessionConfig,
		pushToTalkMode: boolean,
		updateId?: string
	): void {
		// PTT overrides the profile-supplied VAD mode — when the user
		// is driving turn-taking we always disable server VAD.
		// `config.transcriptionModel` / `turnDetectionMode` are always
		// populated by the orchestrator (with `??` fallbacks from the
		// descriptor); no in-provider defaults here.
		this.turnDetectionMode = config.turnDetectionMode;
		this.deferResponseUntilContext = config.deferResponseUntilContext;
		const turnDetection = pushToTalkMode ? null : this.openMicTurnDetection();
		this.pendingSessionUpdateIds.push(updateId);
		const output: Record<string, unknown> = {
			format: { type: 'audio/pcm', rate: 24000 }
		};
		if (this.voice) output.voice = this.voice;
		this.safeSend({
			type: 'session.update',
			...(updateId ? { event_id: updateId } : {}),
			session: {
				// OpenAI Realtime API (2026-05) requires `session.type`
				// and re-nested several params under `audio.input` /
				// `audio.output`. Specifically:
				//   - top-level `input_audio_transcription` →
				//     `audio.input.transcription`
				//   - top-level `turn_detection` →
				//     `audio.input.turn_detection`
				// Without these renames the API rejects the update.
				// `output_modalities: ['audio']` is required for a spoken
				// reply; omitting it can leave a connected call silent.
				type: 'realtime',
				output_modalities: ['audio'],
				instructions: config.instructions,
				audio: {
					input: {
						format: { type: 'audio/pcm', rate: 24000 },
						transcription: { model: config.transcriptionModel },
						turn_detection: turnDetection
					},
					output
				},
				tools: config.tools.map((t) => ({
					type: 'function',
					name: t.name,
					description: t.description,
					parameters: t.parameters
				}))
			}
		});
	}

	private replayResume(resume: ProviderResumeContext): void {
		if (resume.summary && resume.summary.trim()) {
			this.safeSend({
				type: 'conversation.item.create',
				item: {
					type: 'message',
					role: 'system',
					content: [
						{
							type: 'input_text',
							// Reference-only framing: the summary is a record of what already
							// happened. Without this, the model treats tasks mentioned in it as
							// open to-dos and re-fires old multi-step requests on resume/rotation.
							text:
								`Earlier conversation (reference only — do NOT re-run, re-create, or ` +
								`re-dispatch any task, pipeline, or action mentioned here; those already ` +
								`happened. Act only on the user's current request): ${resume.summary.trim()}`
						}
					]
				}
			});
		}
		for (const exchange of resume.tool_exchanges ?? []) {
			const replayCallId = openAiReplayCallId(exchange.call_id);
			this.safeSend({
				type: 'conversation.item.create',
				item: {
					type: 'function_call',
					call_id: replayCallId,
					name: exchange.tool_name,
					arguments: JSON.stringify(exchange.arguments ?? {})
				}
			});
			this.safeSend({
				type: 'conversation.item.create',
				item: {
					type: 'function_call_output',
					call_id: replayCallId,
					output: JSON.stringify(exchange.projected_result ?? null)
				}
			});
		}
		for (const turn of resume.recent_turns ?? []) {
			const role = turn.role === 'assistant' ? 'assistant' : 'user';
			const contentType = role === 'assistant' ? 'output_text' : 'input_text';
			this.safeSend({
				type: 'conversation.item.create',
				item: {
					type: 'message',
					role,
					content: [{ type: contentType, text: turn.text }]
				}
			});
		}
	}

	private handleDataChannelEvent(event: { type?: string } & Record<string, unknown>): void {
		const cb = this.callbacks;
		if (!cb) return;
		const s = this.state;
		if (!s) return;
		const type = String(event.type ?? '');
		switch (type) {
			case 'session.updated':
				cb.onSessionConfigured({ updateId: this.pendingSessionUpdateIds.shift() });
				break;
			case 'input_audio_buffer.speech_started':
				cb.onSpeechStarted();
				break;
			case 'input_audio_buffer.speech_stopped':
				cb.onSpeechStopped();
				break;
			// User audio transcript completed. Old event name was
			// `conversation.item.input_audio_transcription.completed`;
			// 2026-05 API renamed to `conversation.item.input_audio.transcription.completed`.
			// Accept both for forward compatibility.
			case 'conversation.item.input_audio_transcription.delta':
			case 'conversation.item.input_audio.transcription.delta': {
				const itemId = String((event as { item_id?: string }).item_id ?? '').trim();
				const deltaRaw = (event as { delta?: unknown }).delta;
				const delta = typeof deltaRaw === 'string' ? deltaRaw : '';
				if (!itemId || !delta) break;
				const buf = (s.userBuffers.get(itemId) ?? '') + delta;
				s.userBuffers.set(itemId, buf);
				cb.onTranscriptUserPartial?.({ text: buf, itemId });
				break;
			}
			case 'conversation.item.input_audio_transcription.completed':
			case 'conversation.item.input_audio.transcription.completed': {
				const text = String((event as { transcript?: string }).transcript ?? '').trim();
				const itemId = String((event as { item_id?: string }).item_id ?? `user-${Date.now()}`);
				s.userBuffers.delete(itemId);
				cb.onTranscriptUserFinal({ text, itemId });
				break;
			}
			// Assistant audio transcript streaming. Old name
			// `response.audio_transcript.delta`; 2026-05 nested
			// under `output_audio` →
			// `response.output_audio_transcript.delta`. Accept both.
			case 'response.audio_transcript.delta':
			case 'response.output_audio_transcript.delta':
			case 'response.output_audio.transcript.delta': {
				const responseId = String((event as { response_id?: string }).response_id ?? '');
				const deltaRaw = (event as { delta?: unknown }).delta;
				const delta = typeof deltaRaw === 'string' ? deltaRaw : '';
				if (!responseId || !delta) break;
				const buf = (s.assistantBuffers.get(responseId) ?? '') + delta;
				s.assistantBuffers.set(responseId, buf);
				cb.onTranscriptAssistantDelta({ responseId, text: buf });
				break;
			}
			case 'response.audio_transcript.done':
			case 'response.output_audio_transcript.done':
			case 'response.output_audio.transcript.done': {
				const responseId = String((event as { response_id?: string }).response_id ?? '');
				const text = String((event as { transcript?: string }).transcript ?? '').trim();
				s.assistantBuffers.delete(responseId);
				if (text && responseId) cb.onTranscriptAssistantFinal({ responseId, text });
				break;
			}
			case 'output_audio_buffer.started':
				this.outputAudioActive = true;
				this.outputAudioResponseId = typeof event.response_id === 'string' ? event.response_id : null;
				cb.onPlaybackStateChanged?.(true);
				break;
			case 'output_audio_buffer.stopped':
			case 'output_audio_buffer.cleared':
				if (this.outputAudioResponseId && event.response_id !== this.outputAudioResponseId) break;
				this.outputAudioActive = false;
				this.outputAudioResponseId = null;
				cb.onPlaybackStateChanged?.(false);
				break;
			case 'response.created': {
				this.responsePendingOrActive = true;
				const response = (event as { response?: Record<string, unknown> }).response;
				const responseId = typeof response?.id === 'string' ? response.id : undefined;
				cb.onResponseStarted?.({ responseId });
				break;
			}
			case 'response.done': {
				this.responsePendingOrActive = false;
				this.clearActiveTurnContext();
				const response = (event as { response?: Record<string, unknown> }).response;
				const responseId = typeof response?.id === 'string' ? response.id : undefined;
				const status = typeof response?.status === 'string' ? response.status : undefined;
				if (status !== 'completed') {
					const terminalState =
						status === 'cancelled'
							? 'cancelled'
							: status === 'incomplete'
								? 'incomplete'
								: 'failed';
					cb.onResponseFailed?.({ responseId, terminalState });
					break;
				}
				const usage = response?.usage as Record<string, unknown> | undefined;
				const inputTokens =
					typeof usage?.input_tokens === 'number'
						? (usage.input_tokens as number)
						: undefined;
				const outputTokens =
					typeof usage?.output_tokens === 'number'
						? (usage.output_tokens as number)
						: undefined;
				cb.onResponseDone({
					responseId,
					inputTokens,
					outputTokens,
					usage: usage ? parseRealtimeUsage(usage) : undefined
				});
				break;
			}
			case 'response.function_call_arguments.done': {
				const responseId = String((event as { response_id?: string }).response_id ?? '').trim();
				const callId = String((event as { call_id?: string }).call_id ?? '');
				const name = String((event as { name?: string }).name ?? '');
				const argumentsJson = String((event as { arguments?: string }).arguments ?? '{}');
				if (callId && name) cb.onFunctionCall({
					responseId: responseId || undefined,
					callId,
					name,
					argumentsJson
				});
				break;
			}
			case 'error': {
				const err = (event as {
					error?: {
						message?: string;
						type?: string;
						code?: string | null;
						event_id?: string | null;
					};
				}).error;
				// Per the GA Realtime contract, most server `error` events are
				// recoverable and leave the session open. Physical WebRTC failure is
				// reported separately by `connectionstatechange` above.
				cb.onProviderError({
					message: err?.message ?? 'OpenAI Realtime error',
					recoverable: true,
					...(err?.code ? { code: err.code } : {}),
					...(err?.type ? { errorType: err.type } : {}),
					...(err?.event_id ? { clientEventId: err.event_id } : {})
				});
				break;
			}
			default:
				// Bumped to console.warn so the new 2026-05 event
				// names are easy to spot in the browser console
				// when retrieved via Realtime API. Once a new event
				// name appears here in production, add it to the
				// case arms above.
				console.warn('[voice/openai] unhandled data-channel event', type, event);
		}
	}
}

async function negotiateSdp(args: {
	url: string;
	token: string;
	offerSdp: string;
}): Promise<string> {
	const response = await fetch(args.url, {
		method: 'POST',
		headers: {
			Authorization: `Bearer ${args.token}`,
			'Content-Type': 'application/sdp'
		},
		body: args.offerSdp
	});
	if (!response.ok) {
		const text = await response.text().catch(() => '');
		throw new Error(`SDP exchange ${response.status}: ${text.slice(0, 240)}`);
	}
	return await response.text();
}
