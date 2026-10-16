/**
 * Provider-neutral contract for the frontend half of a realtime voice
 * session.
 *
 * `realtimeVoiceClient.ts` (the orchestrator-shaped client) owns:
 *  - the single control WebSocket to the backend
 *  - mic capture, analyser node, audio playback element
 *  - PTT state + status / transcript Svelte stores
 *  - resume payload routing
 *
 * It does NOT own:
 *  - the upstream wire format (OpenAI's `oai-events` channel + event
 *    kinds, Gemini's `setup` WS protocol, etc.)
 *  - the SDP exchange (provider-specific Authorization scheme)
 *  - the audio rendering path (peer connection for OpenAI WebRTC,
 *    PCM stream for Gemini WS, etc.)
 *
 * Those concerns live behind this interface. Adding a new provider
 * is one new file in `lib/media/voice/providers/` plus a single
 * pick-the-provider branch in the client.
 */

/**
 * Wire shape of a tool spec advertised to the realtime model.
 *
 * Voice-as-chat-agent (Phase A1): the backend forwards the FULL chat
 * tool surface for the active agent in session.ready — same shape
 * `LLMToolSpec` ships in chat. Each provider adapter translates this
 * into its native function-spec format (OpenAI Realtime maps to
 * `{type:'function', name, description, parameters}`; Gemini maps to
 * `functionDeclarations`).
 */
export interface VoiceToolDefinition {
	name: string;
	description: string;
	parameters: Record<string, unknown>;
}

export interface ProviderSessionConfig {
	instructions: string;
	tools: VoiceToolDefinition[];
	/** Provider-side knob: model the upstream uses for input audio
	 *  transcription. Provider-specific (e.g. `whisper-1` for OpenAI). */
	transcriptionModel: string;
	/** `server_vad` / `none` / future provider-specific modes. PTT
	 *  callers should pass `none` to disable upstream VAD. */
	turnDetectionMode: string;
	/** Hold provider response creation until the backend returns the bounded
	 * current-utterance context outcome. */
	deferResponseUntilContext: boolean;
}

export interface ProviderDescriptor {
	provider: string;
	model: string;
	topology: 'direct_peer_to_peer' | 'backend_proxied' | string;
	voice?: string | null;
	webrtc_url?: string;
	upstream_token?: string;
	native_resume_handle?: string | null;
	half_duplex?: boolean | null;
}

export interface ProviderResumeTurn {
	role: 'user' | 'assistant' | string;
	text: string;
}

export interface ProviderResumeContext {
	summary: string | null;
	recent_turns: ProviderResumeTurn[];
	tool_exchanges: ProviderResumeToolExchange[];
}

export interface ProviderResumeToolExchange {
	call_id: string;
	tool_name: string;
	arguments: Record<string, unknown> | unknown;
	projected_result: unknown;
}

/** Billing-complete token split from a provider `response.done`. Inputs are
 * uncached counts; cached inputs stay separate so the backend never bills the
 * same token twice. The wire keys deliberately match `magicllm::RealtimeUsage`.
 */
export interface RealtimeUsageBreakdown {
	text_input_tokens: number;
	text_cached_input_tokens: number;
	text_output_tokens: number;
	audio_input_tokens: number;
	audio_cached_input_tokens: number;
	audio_output_tokens: number;
}

export interface RealtimeProviderCallbacks {
	/** Physical PCM queue or WebRTC output buffer state, distinct from response.done. */
	onPlaybackStateChanged?(playing: boolean): void;
	onSessionConfigured(args: { updateId?: string }): void;
	onSpeechStarted(): void;
	onSpeechStopped(): void;
	onTranscriptUserFinal(args: { text: string; itemId: string }): void;
	onTranscriptUserPartial?(args: { text: string; itemId: string }): void;
	onTranscriptAssistantDelta(args: { responseId: string; text: string }): void;
	onTranscriptAssistantFinal(args: { responseId: string; text: string }): void;
	/** Received when the provider has created a physical response. Direct
	 * browser transports use it to establish server-side response timing. */
	onResponseStarted?(args?: { responseId?: string }): void;
	onResponseDone(args: {
		responseId?: string;
		inputTokens?: number;
		outputTokens?: number;
		usage?: RealtimeUsageBreakdown;
	}): void;
	/** Received when a response reaches a non-success terminal state. The
	 * provider adapter normalizes vendor status text to this closed vocabulary
	 * so raw error content never crosses the control channel. */
	onResponseFailed?(args: {
		responseId?: string;
		terminalState: 'failed' | 'cancelled' | 'incomplete';
	}): void;
	onFunctionCall(args: {
		responseId?: string;
		callId: string;
		name: string;
		argumentsJson: string;
	}): void;
	/** Provider errors have two materially different lifecycles. Realtime server
	 *  `error` events are normally request-scoped and leave the session open;
	 *  transport failures mean the peer itself is no longer usable. Callers must
	 *  reconnect only for the latter. Optional vendor metadata is for local
	 *  diagnostics and must not cross the backend control channel. */
	onProviderError(args: {
		message: string;
		recoverable: boolean;
		code?: string;
		errorType?: string;
		clientEventId?: string;
	}): void;
	/** `BackendProxied` providers emit captured mic PCM through
	 *  this callback. The orchestrator-shaped client forwards it
	 *  upstream as a binary frame on the control WebSocket; the
	 *  backend pipes it into the provider session.
	 *  `DirectPeerToPeer` providers never call this (audio rides
	 *  WebRTC, never crosses magician). */
	onAudioFrameOut?(bytes: ArrayBuffer): void;
}

export interface RealtimeFrontendProvider {
	/** Connect upstream. Resolves once audio is flowing both ways.
	 *  Rejects on SDP exchange / negotiation failure. For
	 *  `backend_proxied` topology the implementation just attaches
	 *  to the existing control WS rather than opening a peer. */
	connect(args: {
		descriptor: ProviderDescriptor;
		mic: MediaStream;
		playbackEl: HTMLAudioElement;
		pushToTalkMode: boolean;
		sessionConfig: ProviderSessionConfig;
		resume: ProviderResumeContext | null;
		callbacks: RealtimeProviderCallbacks;
		configurationUpdateId?: string;
	}): Promise<void>;

	/** Replace the live provider catalog. Resolves through
	 * `onSessionConfigured` only after the provider acknowledges it. */
	updateSession?(args: {
		sessionConfig: ProviderSessionConfig;
		pushToTalkMode: boolean;
		updateId: string;
	}): void;

	/** Tear down peer + data channel. Idempotent. */
	disconnect(): void;

	/** Enable/disable the outgoing mic track. PTT uses this between
	 *  press + release. */
	setMicEnabled(enabled: boolean): void;

	/** Switch turn-taking mode on a LIVE call without a reconnect —
	 *  rebuilds `turn_detection` (PTT → none, hands-free → profile VAD)
	 *  and re-arms/mutes the mic accordingly. Optional: only the
	 *  DirectPeerToPeer (OpenAI) provider drives turn detection
	 *  client-side; backend-proxied turn-taking is server-owned. */
	setTurnDetection?(pushToTalkMode: boolean): void;

	/** Drop any audio queued in the provider's input buffer. Called
	 *  on PTT engage to discard pre-roll. */
	clearInputBuffer(): void;

	/** Commit the current input audio as a complete turn and trigger
	 *  a response. Called on PTT release. */
	commitInputAndRespond(): void;

	/** Install one replaceable turn-context item and create the response. The
	 * provider removes that item on response completion. */
	respondWithTurnContext(args: { contextItemId: string; context: string | null }): void;

	/** Interrupt the provider's current assistant response when another
	 *  backend-owned rail, such as Personal Tutor, takes audio ownership.
	 *  Each provider adapter must map this semantic handoff to its own
	 *  cancel/stop primitive, or deliberately no-op if the backend owns
	 *  cancellation and there is no local playback buffer to clear. */
	interruptResponse(): void;

	/** Deliver the result of a backend-dispatched tool call to the
	 *  model and trigger a response. */
	sendToolResult(args: { callId: string; output: string }): void;

	/** Inject a system message into the conversation and (optionally)
	 *  trigger a response — used for backend-pushed announcements
	 *  (task completion, future HITL prompts). */
	injectSystemMessage(args: { text: string; requestResponse: boolean }): void;

	/** `BackendProxied` providers receive provider → browser PCM
	 *  here, forwarded by the client from WS binary frames. The
	 *  provider decodes + plays. `DirectPeerToPeer` providers
	 *  ignore (audio is handled by the WebRTC peer). */
	feedIncomingAudio?(bytes: ArrayBuffer): void;
}

/** Factory: pick the right provider impl from the descriptor's
 *  `provider` field. Throws when no match is found so the client can
 *  surface a clear "unsupported provider" error instead of silently
 *  no-oping. */
export type RealtimeFrontendProviderFactory = (
	descriptor: ProviderDescriptor
) => RealtimeFrontendProvider;
