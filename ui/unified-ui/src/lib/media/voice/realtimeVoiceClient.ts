/**
 * Realtime voice client — orchestrator-shaped front end.
 *
 * Owns a single bidirectional WebSocket per call to
 * `/api/magician/v2/media/voice/{voice_session_id}/control` and
 * delegates every provider-specific concern (WebRTC peer, data
 * channel wire format, SDP exchange, session.update shape) to the
 * `RealtimeFrontendProvider` picked from the descriptor.
 *
 * What lives here:
 *   - control WS protocol (envelope `{kind, payload}` both ways)
 *   - mic capture, audio analyser, hidden playback element
 *   - PTT mechanics (calls into provider for buffer ops)
 *   - status + transcript Svelte stores
 *   - resume payload routing (we hand the resume object to provider
 *     .connect; provider replays it provider-shaped on data-channel
 *     open)
 *
 * What lives in `providers/<name>.ts`:
 *   - WebRTC peer connection / WebSocket upstream
 *   - data-channel name + wire format
 *   - session.update / conversation.item.create / response.create
 *     payload shapes
 *   - SDP exchange (Authorization scheme, Content-Type, etc.)
 *
 * Direct OpenAI WebRTC and backend-proxied OpenAI/Gemini sessions implement
 * this shared lifecycle; provider selection does not change the chat contract.
 */

import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

import { scopedWebSocketProtocols, scopedMagicianWebSocketUrl } from '$lib/stores/scopeIdentityStore';
import { codingChoiceFromSelection, codingProfileStore } from '$lib/stores/codingProfileStore';
import { composerChatChoice } from '$lib/stores/chatHarnessPreferenceStore';
import { chatProfileStore } from '$lib/stores/chatProfileStore';
import { mediaSessionStore } from '$lib/media/store';
import { cancelCurrent as cancelBrowserSpeak } from '$lib/media/tts/browserTts';
import { cancelProviderSpeak } from '$lib/media/tts/providerTts';
import { ttsStore } from '$lib/media/tts/store';
import { showError } from '$lib/shared/stores/notifications';
import { screenLockStateStore } from './screenLock';
import { concurrentVoiceStore, setConcurrentVoiceLiveState, settleConcurrentVoiceInput } from './concurrentVoice';

import { pickRealtimeProvider } from './providers';
import {
	decideAddressedTranscript,
	DEFAULT_VOICE_ADDRESSING,
	type VoiceAddressingConfig
} from './voiceAddressing';
import type {
	ProviderDescriptor,
	ProviderResumeContext,
	RealtimeFrontendProvider,
	RealtimeProviderCallbacks,
	VoiceToolDefinition
} from './providers';

export const MAGICIAN_VOICE_CONTROL_WEBSOCKET_PROTOCOL = 'magician-voice-control-v1';

export type VoiceCallState =
	| 'idle'
	| 'connecting'
	| 'connected'
	| 'reconnecting'
	| 'rotating'
	| 'closing'
	| 'error';

export interface VoiceTurn {
	id: string;
	speaker: 'user' | 'assistant';
	text: string;
	timestamp: number;
	done: boolean;
}

export interface VoiceTranscriptState {
	turns: VoiceTurn[];
	userSpeaking: boolean;
	assistantSpeaking: boolean;
	/** The assistant finished an utterance but is still working on the
	 *  request — Gemini 3.8 Live says "let me check…", runs a tool without
	 *  blocking, then answers. Driven by `interaction.status`; providers
	 *  without the signal never set it. */
	assistantWorking: boolean;
	lastIgnoredTurn: { reason: string; at: number } | null;
}

const DEFAULT_TRANSCRIPT: VoiceTranscriptState = {
	turns: [],
	userSpeaking: false,
	assistantSpeaking: false,
	assistantWorking: false,
	lastIgnoredTurn: null
};

const SILENT_IGNORED_TRANSCRIPT_REASONS = new Set([
	'self_echo',
	// Backward compatibility with backend versions that sent lifecycle cleanup
	// through `transcript.user.ignored` instead of `transcript.user.cleared`.
	'no_final_transcript',
	'input_cleared',
	'session_ended',
	'local_transcript_disabled',
	'local_transcript_failed',
	'provider_commit_unavailable'
]);

export const voiceTranscriptStore = writable<VoiceTranscriptState>(DEFAULT_TRANSCRIPT);

export interface VoiceCallStatus {
	state: VoiceCallState;
	error: string | null;
	model: string | null;
	voice: string | null;
	connectedAt: number | null;
	/** First backend-advertised phrase required to admit an open-mic turn. */
	activationPhrase: string | null;
	/**
	 * EVERY phrase the backend will admit (one per assistant name/alias, e.g.
	 * `["Hey magical", "Hey magician"]`). `activationPhrase` is just the first —
	 * surfaces with room should list them all so an alias isn't hidden.
	 */
	activationPhrases: string[];
	/**
	 * Whether the backend actually GATED this call. Distinct from the user's
	 * `require_voice_prefix` preference: the backend leaves a call ungated when
	 * it has no assistant name to listen for, so asking for the gate does not
	 * guarantee getting it. Only this says whether ambient speech is ignored.
	 */
	addressingRequired: boolean;
	/**
	 * The boundary the backend resolved for this call, for display only.
	 * `null` until `session.ready` lands, and on any backend that does not
	 * send it. Nothing in the client may branch on this to request different
	 * treatment — it exists so whoever is in the room can SEE what the call
	 * is, not so the call can ask to be something else.
	 */
	boundary: VoiceCallBoundary | null;
}

/** Backend-resolved call boundary. Mirrors `session.ready.boundary`. */
export interface VoiceCallBoundary {
	/** e.g. `realtime_voice`, `meeting`, `public_envoy`. */
	surface: string;
	/** `owner` or `untrusted` — the half that decides what a call may reach. */
	audience: string;
	/** The agent this call is bound to. A room's is the ambassador. */
	agent_id: string;
	/** Always false. Present so a reader of this type sees the boundary is
	 *  fixed, rather than wondering whether some other flow can raise it. */
	elevatable: boolean;
}

/** Per-call upper bound on the current upstream session lifetime
 *  in milliseconds. Sourced from the backend's
 *  `session.ready.descriptor.max_session_duration_secs` so the
 *  overlay's timer denominator matches the actual model the call
 *  is on. `null` when the provider doesn't cap sessions. */
export const sessionCapMsStore = writable<number | null>(null);

const DEFAULT_STATUS: VoiceCallStatus = {
	state: 'idle',
	error: null,
	model: null,
	voice: null,
	connectedAt: null,
	activationPhrase: null,
	activationPhrases: [],
	addressingRequired: false,
	boundary: null
};

export const voiceCallStore = writable<VoiceCallStatus>(DEFAULT_STATUS);
export const voiceMicAnalyser = writable<AnalyserNode | null>(null);
export const pushToTalkMode = writable<boolean>(false);
export const pushToTalkActive = writable<boolean>(false);

// ─── Wire envelopes (backend → frontend) ────────────────────────────

interface SessionDescriptorWire extends ProviderDescriptor {
	max_session_duration_secs?: number | null;
	transcription_model?: string | null;
	transcription_fallback_model?: string | null;
	turn_detection_mode?: string | null;
	context_window_tokens?: number | null;
}

interface SessionReadyPayload {
	voice_session_id: string;
	descriptor: SessionDescriptorWire;
	rotation_count: number;
	instructions?: string;
	tools?: VoiceToolDefinition[];
	resume?: ResumeContextWire | null;
	addressing?: VoiceAddressingConfig;
	per_turn_context?: { enabled?: boolean; budget_ms?: number };
	/** What the BACKEND resolved this call to be. Read-only: there is
	 *  deliberately no counterpart the client can send, because the boundary
	 *  is not negotiable. Absent on an older backend, which is treated as an
	 *  unknown boundary — never as "owner". Not repeated on `audio.rebind`:
	 *  a rotation preserves it, so the client keeps what it was told once. */
	boundary?: VoiceCallBoundary | null;
}

interface ToolResultPayload {
	call_id: string;
	output: string;
	voice_summary?: string;
	status: 'ok' | 'error' | string;
}

interface ToolCatalogUpdatePayload {
	update_id: string;
	previous_policy_snapshot_id: string;
	policy_snapshot_id: string;
	working_set_generation: number;
	tools: VoiceToolDefinition[];
}

interface TaskCompletedPayload {
	task_id: string;
	title: string;
	status: 'completed' | 'failed' | 'cancelled' | string;
	summary: string;
	announcement?: string;
}

// One streamed `<speech>` segment from a background `delegate_to_chat`
// run. Each chunk is already a complete speech-tag body — the
// extractor on the backend emits one when the closing `</speech>` tag
// arrives. Inject it as a system message + response.create so the
// realtime model speaks the segment.
interface DelegateToChatChunkPayload {
	call_id: string;
	sequence: number;
	text: string;
}

// Sentinel marking the end of a background `delegate_to_chat` run.
// `chunk_count` lets the frontend reason about whether anything was
// spoken at all (chunk_count > 0 && success). On failure mid-stream
// the user may have heard partial content — `success: false` is the
// signal to surface a brief failure cue if nothing was spoken yet.
interface DelegateToChatDonePayload {
	call_id: string;
	chunk_count: number;
	success: boolean;
	error?: string | null;
}

interface ResumeTurnWire {
	role: 'user' | 'assistant' | string;
	text: string;
	message_id: string;
	created_at_ms: number;
}

interface ResumeContextWire {
	summary: string | null;
	recent_turns: ResumeTurnWire[];
	tool_exchanges?: Array<{
		call_id: string;
		tool_name: string;
		arguments: unknown;
		projected_result: unknown;
	}>;
	total_turns: number;
}

interface AudioRebindPayload {
	descriptor: SessionDescriptorWire;
	resume: ResumeContextWire;
	rotation_count: number;
	tools?: VoiceToolDefinition[];
	catalog_update?: ToolCatalogUpdatePayload | null;
	per_turn_context?: { enabled?: boolean; budget_ms?: number };
}

interface TurnContextReadyPayload {
	context_item_id: string;
	context?: string | null;
}

// ─── Per-call state ─────────────────────────────────────────────────

interface ActiveCall {
	ws: WebSocket;
	voiceSessionId: string;
	uiThreadId: string;
	threadId: string | null;
	mic: MediaStream;
	audioCtx: AudioContext | null;
	analyser: AnalyserNode | null;
	playbackEl: HTMLAudioElement;
	provider: RealtimeFrontendProvider | null;
	ended: boolean;
	/** System instructions + tool catalog from `session.ready`.
	 *  Reused on every rotation (handed to provider.connect). */
	sessionConfig: { instructions: string; tools: VoiceToolDefinition[] } | null;
	/** Resume payload to feed into the next provider.connect for a
	 *  rotation. Cleared after each handoff. */
	pendingResume: ProviderResumeContext | null;
	/** Provider-side knobs the backend picked from the realtime voice
	 *  profile. The provider reads them at connect time when building
	 *  session.update / equivalent. */
	transcriptionModel: string;
	turnDetectionMode: string;
	deferResponseUntilContext: boolean;
	turnContextBudgetMs: number;
	contextWindowTokens: number | null;
	voiceMode: 'realtime' | 'hands_free';
	realtimeProfile: string | null;
	audioTopology: SessionDescriptorWire['topology'] | null;
	echoCancellation: boolean;
	halfDuplex: boolean;
	activeAudioResponseId: string | null;
	/** Provider generation observed before audio necessarily starts. This lets
	 *  Tutor/App Copilot fence the exact response even when takeover ack races
	 *  a transcript delta or `response.created`. */
	currentProviderResponseId: string | null;
	tutorTakeoverActive: boolean;
	tutorResponseFenceActive: boolean;
	tutorSuppressedResponseId: string | null;
	addressing: VoiceAddressingConfig;
	suppressAmbientResponse: boolean;
	addressingArmedUntilMs: number;
	lastLocalTranscriptItemId: string | null;
	/** Coalesces terminal provider failures until the backend completes one
	 *  audio rebind. Without this, an error burst can request overlapping
	 *  rotations and repeatedly reset an otherwise recoverable call. */
	reconnectPending: boolean;
	controlReconnectAttempt: number;
	controlReconnectTimer: number | null;
}

let active: ActiveCall | null = null;
let providerOutputPlaying = false;
let lastConcurrentContext: string | null = null;

function syncConcurrentVoiceState(): void {
    if (!browser) return;
    const transcript = get(voiceTranscriptStore);
    const status = get(voiceCallStore).state;
    setConcurrentVoiceLiveState({
        active: status === 'connected' || status === 'reconnecting' || status === 'rotating',
        userSpeaking: transcript.userSpeaking,
        assistantSpeaking: transcript.assistantSpeaking || transcript.assistantWorking || providerOutputPlaying
    });
}
if (browser) {
    voiceTranscriptStore.subscribe(syncConcurrentVoiceState);
    voiceCallStore.subscribe(syncConcurrentVoiceState);
    concurrentVoiceStore.subscribe(state => {
        if (!active || active.ended) return;
        const contextId = state.focus?.branch_session_id ?? null;
        if (contextId !== lastConcurrentContext) {
            lastConcurrentContext = contextId;
            sendControl(active.ws, 'voice.context', { context_session_id: contextId });
            if (contextId) active.provider?.injectSystemMessage({ text: 'The listener selected or heard an application-managed background answer. For follow-ups, delegate_to_chat with their exact words; the server resolves the addressed context. Do not guess content you have not received.', requestResponse: false });
        }
        if (active.halfDuplex) active.provider?.setMicEnabled(!state.speaking && !get(voiceTranscriptStore).assistantSpeaking && !providerOutputPlaying);
    });
}
let starting = false;
let startGeneration = 0;
let pendingStartAbort: AbortController | null = null;
let pttEngagedAt = 0;
let pendingPttEngageOnProviderReady = false;
const PTT_MIN_ENGAGE_MS = 220;
const CONTROL_START_RETRY_DELAYS_MS = [250, 750] as const;

screenLockStateStore.subscribe((state) => {
	if (!active || (state !== 'locked' && state !== 'unlocked')) return;
	sendControl(active.ws, 'screen.state', { locked: state === 'locked' });
});

function setStatus(patch: Partial<VoiceCallStatus>): void {
	voiceCallStore.update((s) => ({ ...s, ...patch }));
}

function voiceErrorMessage(error: unknown, fallback: string): string {
	if (error instanceof Error && error.message.trim()) return error.message;
	if (typeof error === 'object' && error !== null && 'message' in error) {
		const message = (error as { message?: unknown }).message;
		if (typeof message === 'string' && message.trim()) return message;
	}
	return fallback;
}

export function isVoiceCallCaptureState(state: VoiceCallState): boolean {
	return state === 'connecting'
		|| state === 'connected'
		|| state === 'reconnecting'
		|| state === 'rotating'
		|| state === 'closing';
}

function cancelNonCallPlayback(): void {
	cancelBrowserSpeak('replaced');
	// A live call is a distinct audio-focus owner. Invalidate the whole
	// provider queue, including already-buffered follow-on segments.
	cancelProviderSpeak('user');
	ttsStore.setActive(null);
}

// ─── Public API ─────────────────────────────────────────────────────

export async function startVoiceCall(
	options: {
		threadId?: string | null;
		mode?: 'realtime' | 'hands_free';
		realtimeProfile?: string | null;
	} = {}
): Promise<void> {
	if (!browser) return;
	if (active || starting) return;

	const mediaSession = get(mediaSessionStore).session;
	if (!mediaSession) {
		setStatus({ state: 'error', error: 'No realtime media session is registered.' });
		return;
	}
	const generation = ++startGeneration;
	const startupAbort = new AbortController();
	pendingStartAbort = startupAbort;
	starting = true;
	setStatus({ state: 'connecting', error: null, activationPhrase: null,
	activationPhrases: [],
	addressingRequired: false });
	cancelNonCallPlayback();

	let pendingMic: MediaStream | null = null;
	let pendingAudioCtx: AudioContext | null = null;
	let pendingPlaybackEl: HTMLAudioElement | null = null;
	let pendingWs: WebSocket | null = null;

	try {
		const { stream: mic, audioCtx, analyser, echoCancellation } = await openMicWithAnalyser();
		pendingMic = mic;
		pendingAudioCtx = audioCtx;
		if (generation !== startGeneration) {
			await cleanupPendingStart(pendingMic, pendingAudioCtx, null, null);
			return;
		}
		const playbackEl = createHiddenAudioElement();
		pendingPlaybackEl = playbackEl;
		const uiThreadId = options.threadId ?? mediaSession.thread_id ?? 'general';

		const ws = await openControlSocketWithRetry(mediaSession.session_id, startupAbort.signal);
		pendingWs = ws;
		if (generation !== startGeneration || ws.readyState !== WebSocket.OPEN) {
			await cleanupPendingStart(pendingMic, pendingAudioCtx, pendingPlaybackEl, pendingWs);
			return;
		}
		const call: ActiveCall = {
			ws,
			voiceSessionId: mediaSession.session_id,
			uiThreadId,
			threadId: options.threadId ?? mediaSession.thread_id ?? null,
			mic,
			audioCtx,
			analyser,
			playbackEl,
			provider: null,
			ended: false,
			sessionConfig: null,
			pendingResume: null,
			// Empty placeholders. `onSessionReady` overwrites with the
			// descriptor's knobs (or its own `?? 'whisper-1'`/'server_vad'
			// fallbacks) before the provider connects.
			transcriptionModel: '',
			turnDetectionMode: '',
			deferResponseUntilContext: false,
			turnContextBudgetMs: 0,
			contextWindowTokens: null,
			voiceMode: options.mode ?? 'realtime',
			realtimeProfile: options.realtimeProfile?.trim() || null,
			audioTopology: null,
			echoCancellation,
			halfDuplex: false,
			activeAudioResponseId: null,
			currentProviderResponseId: null,
			tutorTakeoverActive: false,
			tutorResponseFenceActive: false,
			tutorSuppressedResponseId: null,
			addressing: DEFAULT_VOICE_ADDRESSING,
			suppressAmbientResponse: false,
			addressingArmedUntilMs: 0,
			lastLocalTranscriptItemId: null,
			reconnectPending: false,
			controlReconnectAttempt: 0,
			controlReconnectTimer: null
		};
		active = call;
		pendingMic = null;
		pendingAudioCtx = null;
		pendingPlaybackEl = null;
		pendingWs = null;
		if (pendingStartAbort === startupAbort) pendingStartAbort = null;
		voiceMicAnalyser.set(analyser);

		bindControlSocket(call, ws);
		sendSessionStart(call);
	} catch (err) {
		await cleanupPendingStart(pendingMic, pendingAudioCtx, pendingPlaybackEl, pendingWs);
		if (pendingStartAbort === startupAbort) pendingStartAbort = null;
		// Release the start guard BEFORE the generation check. A failed start
		// whose generation moved on used to return here with `starting` still
		// true, and `startVoiceCall` bails on `if (active || starting)` — so every
		// later click became a silent no-op that never reached the backend and
		// logged nothing, until the page was reloaded. A failed start must never
		// latch the guard, superseded or not.
		starting = false;
		if (generation !== startGeneration) return;
		const message = voiceErrorMessage(err, 'voice start failed');
		setStatus({ state: 'error', error: message });
		showError('Voice unavailable', message);
		await teardownActive('error');
		return;
	}
}

export async function rotateVoiceUpstream(
	reason: 'proactive' | 'reconnect' | 'watermark' | 'manual' = 'manual'
): Promise<boolean> {
	if (!active || active.ended) return false;
	sendControl(active.ws, 'session.rotate', { reason });
	return true;
}

export function stopVoiceCall(): void {
	stopVoiceCallForReason('clean');
}

function stopVoiceCallForReason(reason: 'clean' | 'error'): void {
	startGeneration += 1;
	pendingStartAbort?.abort();
	pendingStartAbort = null;
	starting = false;
	if (!active) {
		if (reason === 'clean') {
			setStatus({
				state: 'idle',
				error: null,
				connectedAt: null,
				activationPhrase: null,
				activationPhrases: [],
				addressingRequired: false
			});
		}
		return;
	}
	if (reason === 'clean') setStatus({ state: 'closing' });
	if (!active.ended) {
		try {
			sendControl(active.ws, 'session.end', {});
		} catch {
			/* socket already dead; nothing to send */
		}
	}
	void teardownActive(reason);
}

export function sendVoiceControl(payload: Record<string, unknown>): void {
	// Back-compat shim. Provider-specific raw events should not leak
	// through here — every interaction goes through provider methods
	// or the control WS. Logged so any stale caller is visible.
	console.warn('[voice] sendVoiceControl is deprecated', payload);
}

export function engagePushToTalk(): void {
	if (!active) return;
	if (active.voiceMode === 'hands_free') return;
	if (!active.provider) {
		pendingPttEngageOnProviderReady = true;
		pushToTalkActive.set(true);
		return;
	}
	pendingPttEngageOnProviderReady = false;
	active.provider.clearInputBuffer();
	active.provider.setMicEnabled(true);
	pttEngagedAt = Date.now();
	pushToTalkActive.set(true);
	voiceTranscriptStore.update(s => ({ ...s, userSpeaking: true }));
	sendControl(active.ws, 'ptt.engage', {});
	// PTT disables VAD, so freeze the addressed context explicitly.
	sendControl(active.ws, 'speech.started', {});
}

export function releasePushToTalk(): void {
	pendingPttEngageOnProviderReady = false;
	if (!active) {
		pushToTalkActive.set(false);
		pttEngagedAt = 0;
		return;
	}
	if (!active.provider) {
		pushToTalkActive.set(false);
		pttEngagedAt = 0;
		return;
	}
	if (active.voiceMode === 'hands_free') {
		pushToTalkActive.set(false);
		pttEngagedAt = 0;
		return;
	}
	const wasActive = get(pushToTalkActive);
	active.provider?.setMicEnabled(false);
	pushToTalkActive.set(false);
	if (!wasActive) return;
	voiceTranscriptStore.update(s => ({ ...s, userSpeaking: false }));
	const heldFor = Date.now() - pttEngagedAt;
	pttEngagedAt = 0;
	if (heldFor < PTT_MIN_ENGAGE_MS) {
		active.provider.clearInputBuffer();
		sendControl(active.ws, 'input.clear', {});
		settleConcurrentVoiceInput();
		return;
	}
	sendControl(active.ws, 'ptt.release', {});
	active.provider?.commitInputAndRespond();
}

export function setPushToTalkMode(enabled: boolean): void {
	pushToTalkMode.set(enabled);
	if (!active || active.voiceMode !== 'realtime') return;
	if (enabled && get(voiceTranscriptStore).userSpeaking) {
		// PTT discards the uncommitted open-mic buffer. It cannot produce a
		// final transcript, so release that capture's delivery gate.
		voiceTranscriptStore.update(s => ({ ...s, userSpeaking: false }));
		settleConcurrentVoiceInput();
	}
	if (active.audioTopology === 'backend_proxied') {
		// Gemini fixes activity detection in the setup message. Rotate the
		// upstream session so its turn authority cannot drift from this control.
		if (enabled) active.provider?.setMicEnabled(false);
		pendingPttEngageOnProviderReady = false;
		pushToTalkActive.set(false);
		pttEngagedAt = 0;
		sendControl(active.ws, 'session.turn_boundary', {
			turn_boundary: enabled ? 'push_to_talk' : 'server_vad'
		});
		setStatus({ state: 'rotating', error: null });
		return;
	}
	// Direct WebRTC providers can update turn detection in-place.
	active.provider?.setTurnDetection?.(enabled);
}

export function clearVoiceTranscript(): void {
	voiceTranscriptStore.set(DEFAULT_TRANSCRIPT);
}

// ─── Control WebSocket ──────────────────────────────────────────────

function buildControlUrl(voiceSessionId: string): string {
	const proto = window.location.protocol === 'https:' ? 'wss' : 'ws';
	const host = window.location.host;
	return scopedMagicianWebSocketUrl(`/api/magician/v2/media/voice/${encodeURIComponent(
		voiceSessionId
	)}/control`, `${proto}://${host}`);
}

function openControlSocket(voiceSessionId: string, signal: AbortSignal): Promise<WebSocket> {
	return new Promise((resolve, reject) => {
		const url = buildControlUrl(voiceSessionId);
		const ws = new WebSocket(
			url,
			scopedWebSocketProtocols([MAGICIAN_VOICE_CONTROL_WEBSOCKET_PROTOCOL])
		);
		let settled = false;
		const timeout = window.setTimeout(() => {
			finish(new Error('voice control WebSocket timed out while opening'));
			try { ws.close(); } catch { /* no-op */ }
		}, 15_000);
		const cleanup = () => {
			window.clearTimeout(timeout);
			ws.removeEventListener('open', onOpen);
			ws.removeEventListener('error', onError);
			ws.removeEventListener('close', onClose);
			signal.removeEventListener('abort', onAbort);
		};
		const finish = (error?: Error) => {
			if (settled) return;
			settled = true;
			cleanup();
			if (error) reject(error);
			else resolve(ws);
		};
		const onOpen = () => finish();
		const onError = () => {
			finish(new Error('voice control WebSocket failed to open'));
			try { ws.close(); } catch { /* no-op */ }
		};
		const onClose = () => finish(new Error('voice control WebSocket closed while opening'));
		const onAbort = () => {
			finish(new DOMException('voice start cancelled', 'AbortError'));
			try { ws.close(); } catch { /* no-op */ }
		};
		ws.addEventListener('open', onOpen);
		ws.addEventListener('error', onError);
		ws.addEventListener('close', onClose);
		signal.addEventListener('abort', onAbort, { once: true });
		if (signal.aborted) onAbort();
	});
}

async function openControlSocketWithRetry(
	voiceSessionId: string,
	signal: AbortSignal
): Promise<WebSocket> {
	let lastError: unknown = new Error('voice control WebSocket failed to open');
	for (let attempt = 0; attempt <= CONTROL_START_RETRY_DELAYS_MS.length; attempt += 1) {
		if (signal.aborted) throw new DOMException('voice start cancelled', 'AbortError');
		if (attempt > 0) {
			await waitForControlRetry(CONTROL_START_RETRY_DELAYS_MS[attempt - 1], signal);
		}
		try {
			return await openControlSocket(voiceSessionId, signal);
		} catch (error) {
			if (signal.aborted || (error instanceof DOMException && error.name === 'AbortError')) {
				throw error;
			}
			lastError = error;
		}
	}
	throw lastError;
}

function waitForControlRetry(delayMs: number, signal: AbortSignal): Promise<void> {
	return new Promise((resolve, reject) => {
		let settled = false;
		const finish = (error?: DOMException) => {
			if (settled) return;
			settled = true;
			window.clearTimeout(timeout);
			signal.removeEventListener('abort', onAbort);
			if (error) reject(error);
			else resolve();
		};
		const onAbort = () => finish(new DOMException('voice start cancelled', 'AbortError'));
		const timeout = window.setTimeout(() => finish(), delayMs);
		signal.addEventListener('abort', onAbort, { once: true });
		if (signal.aborted) onAbort();
	});
}

function sendControl(ws: WebSocket, kind: string, payload: unknown): void {
	if (ws.readyState !== WebSocket.OPEN) return;
	try {
		ws.send(JSON.stringify({ kind, payload }));
	} catch (err) {
		console.warn('[voice] control send failed', kind, err);
	}
}

function sessionStartPayload(call: ActiveCall): Record<string, unknown> {
	const payload: Record<string, unknown> = {
		ui_thread_id: call.uiThreadId,
		thread_id: call.threadId,
		voice_mode: call.voiceMode,
		concurrent_requests: true,
		turn_boundary: get(pushToTalkMode) ? 'push_to_talk' : 'server_vad',
		echo_cancellation: call.echoCancellation,
		// In-app web matches iOS: the overlay says Listening and must
		// admit ordinary speech. The stored prefix preference is for
		// ambient/wake surfaces that actually show the phrase.
		require_voice_prefix: false
	};
	const screenLockState = get(screenLockStateStore);
	if (screenLockState === 'locked' || screenLockState === 'unlocked') {
		payload.screen_locked = screenLockState === 'locked';
	}
	if (call.voiceMode === 'realtime' && call.realtimeProfile) {
		payload.realtime_profile = call.realtimeProfile;
	}
	const codingChoice = codingChoiceFromSelection(get(codingProfileStore).selected);
	if (codingChoice) {
		payload.coding_choice = codingChoice;
	}
	// The composer's engine: the call's chat turns (delegate_to_chat, hands-free
	// turns) think with it, as a typed turn from this client would. A browser
	// with no choice of its own sends none, and the server's default applies.
	const chatChoice = composerChatChoice(get(chatProfileStore).selected);
	if (chatChoice) {
		payload.chat_choice = chatChoice;
	}
	return payload;
}

function sendSessionStart(call: ActiveCall): void {
	sendControl(call.ws, 'session.start', sessionStartPayload(call));
}

function bindControlSocket(call: ActiveCall, ws: WebSocket): void {
	ws.addEventListener('message', (event) => {
		if (active !== call || call.ended || call.ws !== ws) return;
		if (typeof event.data === 'string') {
			void handleControlMessage(event.data);
			return;
		}
		const data = event.data as ArrayBuffer | Blob;
		if (data instanceof ArrayBuffer) {
			call.provider?.feedIncomingAudio?.(data);
		} else if (data instanceof Blob) {
			void data.arrayBuffer().then((buffer) => {
				if (active === call && !call.ended && call.ws === ws) {
					call.provider?.feedIncomingAudio?.(buffer);
				}
			});
		}
	});
	ws.addEventListener('close', () => {
		if (active === call && !call.ended && call.ws === ws) {
			scheduleControlReconnect(call);
		}
	});
}

function scheduleControlReconnect(call: ActiveCall): void {
	if (active !== call || call.ended || call.controlReconnectTimer !== null) return;
	const delays = [250, 750, 1_500];
	if (call.controlReconnectAttempt >= delays.length) {
		setStatus({ state: 'error', error: 'Voice control connection could not be restored.' });
		showError('Voice unavailable', 'Voice control connection could not be restored.');
		stopVoiceCallForReason('error');
		return;
	}
	call.provider?.disconnect();
	call.provider = null;
	setStatus({ state: 'reconnecting', error: 'Connection interrupted. Reconnecting…' });
	const delay = delays[call.controlReconnectAttempt];
	call.controlReconnectAttempt += 1;
	call.controlReconnectTimer = window.setTimeout(async () => {
		call.controlReconnectTimer = null;
		if (active !== call || call.ended) return;
		const controller = new AbortController();
		try {
			const ws = await openControlSocket(call.voiceSessionId, controller.signal);
			if (active !== call || call.ended) {
				ws.close();
				return;
			}
			call.ws = ws;
			bindControlSocket(call, ws);
			sendSessionStart(call);
		} catch {
			scheduleControlReconnect(call);
		}
	}, delay);
}

async function handleControlMessage(raw: string): Promise<void> {
	let envelope: { kind?: string; payload?: unknown };
	try {
		envelope = JSON.parse(raw);
	} catch {
		return;
	}
	const kind = String(envelope.kind ?? '');
	const payload = (envelope.payload ?? {}) as Record<string, unknown>;
	switch (kind) {
		case 'session.ready':
			// A fresh upstream session starts idle; a "working" flag carried
			// over from a rotated session would never be cleared by it.
			voiceTranscriptStore.update((state) => ({
				...state,
				assistantWorking: false,
				lastIgnoredTurn: null
			}));
			await onSessionReady(payload as unknown as SessionReadyPayload);
			break;
		case 'session.rotating':
			setStatus({ state: 'rotating', error: null });
			break;
		case 'audio.rebind':
			await onAudioRebind(payload as unknown as AudioRebindPayload);
			break;
		case 'task.completed':
			onTaskCompleted(payload as unknown as TaskCompletedPayload);
			break;
		case 'delegate_to_chat.chunk':
			onDelegateToChatChunk(payload as unknown as DelegateToChatChunkPayload);
			break;
		case 'delegate_to_chat.done':
			onDelegateToChatDone(payload as unknown as DelegateToChatDonePayload);
			break;
		case 'tool.result':
			onToolResult(payload as unknown as ToolResultPayload);
			break;
		case 'tool.catalog.update':
			onToolCatalogUpdate(payload as unknown as ToolCatalogUpdatePayload);
			break;
		case 'turn.context.ready':
			onTurnContextReady(payload as unknown as TurnContextReadyPayload);
			break;
		case 'speech.started':
			voiceTranscriptStore.update((state) => ({
				...state,
				userSpeaking: true,
				lastIgnoredTurn: null
			}));
			break;
		case 'speech.stopped':
			voiceTranscriptStore.update((state) => ({ ...state, userSpeaking: false }));
			break;
		case 'transcript.user.partial': {
			const text = String(payload.text ?? '').trim();
			if (text) {
				upsertTurn({
					id: String(payload.item_id ?? 'local-user-current'),
					speaker: 'user',
					text,
					timestamp: Date.now(),
					done: false
				});
			}
			break;
		}
		case 'voice.control.reply':
			active?.provider?.injectSystemMessage({ text: String(payload.text ?? ''), requestResponse: true });
			break;
		case 'voice.request.cancelled':
		case 'voice.request.accepted':
			// The transcript settles capture. An older admission acknowledgement
			// must not release a newer utterance still awaiting transcription.
			break;
		case 'voice.request.failed':
			showError('Voice request was not accepted', String(payload.message ?? 'Please try again.'));
			break;
		case 'transcript.user': {
			settleConcurrentVoiceInput();
			const text = String(payload.text ?? '').trim();
			if (text) {
				if (active) {
					active.suppressAmbientResponse = false;
					active.addressingArmedUntilMs = 0;
				}
				upsertTurn({
					id: String(payload.item_id ?? `user-${Date.now()}`),
					speaker: 'user',
					text,
					timestamp: Date.now(),
					done: true
				});
			}
			break;
		}
		case 'transcript.user.ignored': {
			settleConcurrentVoiceInput();
			const itemId = String(payload.item_id ?? '').trim();
			const reason = String(payload.reason ?? '').trim() || 'unspecified';
			if (itemId) removeTurn(itemId);
			if (active) {
				if (
					itemId &&
					active.lastLocalTranscriptItemId &&
					active.lastLocalTranscriptItemId !== itemId
				) {
					break;
				}
				active.suppressAmbientResponse = true;
				active.addressingArmedUntilMs =
					reason === 'address_prefix_armed'
						? Date.now() + (active.addressing.follow_up_window_ms ?? 8_000)
						: 0;
				active.activeAudioResponseId = null;
				active.currentProviderResponseId = null;
				active.provider?.interruptResponse();
				if (active.halfDuplex) active.provider?.setMicEnabled(true);
			}
			voiceTranscriptStore.update((state) => ({
				...state,
				assistantSpeaking: false,
				lastIgnoredTurn: SILENT_IGNORED_TRANSCRIPT_REASONS.has(reason)
					? state.lastIgnoredTurn
					: { reason, at: Date.now() }
			}));
			break;
		}
		case 'transcript.user.cleared': {
			settleConcurrentVoiceInput();
			const itemId = String(payload.item_id ?? '').trim();
			if (itemId) removeTurn(itemId);
			break;
		}
		case 'transcript.assistant.delta': {
			const responseId = String(payload.response_id ?? 'current');
			upsertAssistantDelta(responseId, String(payload.text ?? ''));
			break;
		}
		case 'transcript.assistant': {
			const responseId = String(payload.response_id ?? 'current');
			upsertTurn({
				id: `assistant-${responseId}`,
				speaker: 'assistant',
				text: String(payload.text ?? ''),
				timestamp: Date.now(),
				done: true
			});
			break;
		}
		case 'audio.output.started': {
			if (active) {
				const responseId = String(payload.response_id ?? 'current');
				active.activeAudioResponseId = responseId;
				active.currentProviderResponseId = responseId;
			}
			voiceTranscriptStore.update((state) => ({ ...state, assistantSpeaking: true }));
			if (active?.halfDuplex) active.provider?.setMicEnabled(false);
			break;
		}
		case 'audio.output.ended': {
			const responseId = String(payload.response_id ?? 'current');
			if (active && active.activeAudioResponseId !== responseId) break;
			if (active) active.activeAudioResponseId = null;
			voiceTranscriptStore.update((state) => ({ ...state, assistantSpeaking: false }));
			if (active?.halfDuplex) active.provider?.setMicEnabled(true);
			break;
		}
		case 'interaction.status': {
			const working = String(payload.status ?? '') === 'in_progress';
			voiceTranscriptStore.update((state) =>
				state.assistantWorking === working ? state : { ...state, assistantWorking: working }
			);
			break;
		}
		case 'response.interrupted': {
			const responseId = payload.response_id ? String(payload.response_id) : null;
			if (
				responseId
				&& active
				&& (
					(active.activeAudioResponseId && active.activeAudioResponseId !== responseId)
					|| (active.currentProviderResponseId && active.currentProviderResponseId !== responseId)
				)
			) break;
			active?.provider?.interruptResponse();
			if (active) {
				active.activeAudioResponseId = null;
				if (!responseId || active.currentProviderResponseId === responseId) {
					active.currentProviderResponseId = null;
				}
			}
			voiceTranscriptStore.update((state) => ({
				...state,
				assistantSpeaking: false,
				assistantWorking: false
			}));
			if (active?.halfDuplex) active.provider?.setMicEnabled(true);
			break;
		}
		case 'tutor.takeover.started':
			onTutorTakeoverStarted();
			break;
		case 'tutor.takeover.completed':
			onTutorTakeoverCompleted();
			break;
		case 'tutor.takeover.failed':
			onTutorTakeoverFailed(payload);
			break;
		case 'session.error':
			if (payload.recoverable) {
				// Keep the live lifecycle state: the backend explicitly says the
				// transport can continue. Moving to `error` hides the call UI while
				// its microphone remains active.
				setStatus({ error: String(payload.message ?? 'voice error') });
			} else {
				removeUnfinishedUserTurns();
				setStatus({
					state: 'error',
					error: String(payload.message ?? 'voice error')
				});
				showError(
					'Voice unavailable',
					String(payload.message ?? 'Voice connection lost.')
				);
				stopVoiceCallForReason('error');
			}
			break;
		case 'session.ended':
			removeUnfinishedUserTurns();
			void teardownActive('clean');
			break;
		default:
			console.debug('[voice] unknown control frame', kind, payload);
	}
}

// ─── Session lifecycle ──────────────────────────────────────────────

/** Narrow the wire boundary without inventing one. An absent or malformed
 *  block yields `null` — "we do not know" — never a fabricated owner
 *  boundary, which is the one wrong answer a display could give. */
function readCallBoundary(wire: VoiceCallBoundary | null | undefined): VoiceCallBoundary | null {
	if (!wire || typeof wire !== 'object') return null;
	const surface = typeof wire.surface === 'string' ? wire.surface.trim() : '';
	const audience = typeof wire.audience === 'string' ? wire.audience.trim() : '';
	if (!surface || !audience) return null;
	return {
		surface,
		audience,
		agent_id: typeof wire.agent_id === 'string' ? wire.agent_id : '',
		elevatable: false
	};
}

async function onSessionReady(payload: SessionReadyPayload): Promise<void> {
	if (!active) return;
	active.controlReconnectAttempt = 0;
	active.sessionConfig = {
		instructions: payload.instructions ?? '',
		tools: payload.tools ?? []
	};
	active.addressing = {
		required: payload.addressing?.required === true,
		activation_phrases: Array.isArray(payload.addressing?.activation_phrases)
			? payload.addressing.activation_phrases.filter(
					(phrase): phrase is string => typeof phrase === 'string' && phrase.trim().length > 0
				)
			: [],
		follow_up_window_ms:
			typeof payload.addressing?.follow_up_window_ms === 'number'
				? payload.addressing.follow_up_window_ms
				: 8_000
	};
	active.addressingArmedUntilMs = 0;
	setStatus({ boundary: readCallBoundary(payload.boundary) });
	const descriptor = payload.descriptor;
	const transcriptionModel = descriptor.transcription_model?.trim() ?? '';
	if (descriptor.topology === 'direct_peer_to_peer' && !transcriptionModel) {
		const message = 'Realtime transcription model is not configured.';
		setStatus({ state: 'error', error: message });
		showError('Voice unavailable', message);
		stopVoiceCallForReason('error');
		return;
	}
	active.transcriptionModel = transcriptionModel;
	active.turnDetectionMode = descriptor.turn_detection_mode ?? 'server_vad';
	active.audioTopology = descriptor.topology;
	active.deferResponseUntilContext = payload.per_turn_context?.enabled === true;
	active.turnContextBudgetMs = Number(payload.per_turn_context?.budget_ms ?? 0);
	active.contextWindowTokens = descriptor.context_window_tokens ?? null;
	active.halfDuplex = descriptor.half_duplex === true;
	sessionCapMsStore.set(
		descriptor.max_session_duration_secs != null
			? descriptor.max_session_duration_secs * 1000
			: null
	);
	try {
		await connectProvider(descriptor, toProviderResume(payload.resume));
		if (active) active.reconnectPending = false;
	} catch (err) {
		const message = voiceErrorMessage(err, 'provider connect failed');
		setStatus({ state: 'error', error: message });
		showError('Voice unavailable', message);
		stopVoiceCallForReason('error');
		return;
	}
	starting = false;
	setStatus({
		state: 'connected',
		error: null,
		model: descriptor.model,
		voice: descriptor.voice ?? null,
		connectedAt: get(voiceCallStore).connectedAt ?? Date.now(),
		// `addressingRequired` is the BACKEND's answer, not the user's preference:
		// it leaves a call ungated when it has no assistant name to listen for, so
		// asking for the gate doesn't guarantee getting it. Surfacing both lets the
		// UI say "not gated" instead of silently implying ambient speech is ignored.
		activationPhrase: active.addressing.required
			? active.addressing.activation_phrases[0] ?? null
			: null,
		activationPhrases: active.addressing.required
			? [...active.addressing.activation_phrases]
			: [],
		addressingRequired: active.addressing.required
	});
}

async function onAudioRebind(payload: AudioRebindPayload | undefined): Promise<void> {
	if (!active || !payload) return;
	const descriptor = payload.descriptor;
	if (!descriptor) return;
	active.audioTopology = descriptor.topology;
	active.turnDetectionMode = descriptor.turn_detection_mode ?? 'server_vad';
	active.deferResponseUntilContext = payload.per_turn_context?.enabled === true;
	active.turnContextBudgetMs = Number(payload.per_turn_context?.budget_ms ?? 0);
	// Tear down only the provider — mic, audio context, playback
	// element survive so the orb stays continuous.
	active.provider?.disconnect();
	active.provider = null;
	active.pendingResume = toProviderResume(payload.resume);
	if (payload.tools && active.sessionConfig) {
		active.sessionConfig = {
			...active.sessionConfig,
			tools: payload.tools
		};
	}
	try {
		await connectProvider(
			descriptor,
			active.pendingResume,
			payload.catalog_update?.update_id
		);
		active.pendingResume = null;
		active.reconnectPending = false;
		// A provider token/peer rebind is still the same user-visible call. Keep
		// the original `connectedAt` so the duration never jumps back to zero.
		setStatus({ state: 'connected', error: null });
	} catch (err) {
		const message = voiceErrorMessage(err, 'rebind failed');
		setStatus({ state: 'error', error: message });
		showError('Voice unavailable', message);
		stopVoiceCallForReason('error');
	}
}

async function connectProvider(
	descriptor: SessionDescriptorWire,
	resume: ProviderResumeContext | null,
	configurationUpdateId?: string
): Promise<void> {
	const call = active;
	if (!call) throw new Error('no active call');
	if (!call.sessionConfig) {
		throw new Error('session config missing — `session.ready` must precede connect');
	}
	const provider = pickRealtimeProvider(descriptor);
	const providerPushToTalkMode = call.voiceMode === 'realtime' && get(pushToTalkMode);
	const callbacks: RealtimeProviderCallbacks = {
        onPlaybackStateChanged(playing) {
            if (active !== call || call.provider !== provider) return;
            providerOutputPlaying = playing;
            syncConcurrentVoiceState();
        },
		onSessionConfigured({ updateId }) {
			if (active !== call || !updateId) return;
			sendControl(call.ws, 'tool.catalog.ack', { update_id: updateId });
		},
		onSpeechStarted() {
			if (active !== call || call.provider !== provider) return;
			setStatus({ error: null });
			sendControl(call.ws, 'speech.started', {});
			voiceTranscriptStore.update((s) => ({ ...s, userSpeaking: true }));
		},
		onSpeechStopped() {
			if (descriptor.topology === 'direct_peer_to_peer') {
				sendControl(call.ws, 'speech.stopped', {});
			}
			voiceTranscriptStore.update((s) => ({ ...s, userSpeaking: false }));
		},
		onTranscriptUserPartial({ text, itemId }) {
			if (active !== call || call.ended) return;
			const trimmed = text.trim();
			if (!trimmed) return;
			upsertTurn({
				id: itemId,
				speaker: 'user',
				text: trimmed,
				timestamp: Date.now(),
				done: false
			});
		},
		onTranscriptUserFinal({ text, itemId }) {
			if (active !== call || call.ended) return;
			// Silence/noise still settles input. Never release a newer capture.
			if (!text.trim() && !get(voiceTranscriptStore).userSpeaking) settleConcurrentVoiceInput();
			call.lastLocalTranscriptItemId = itemId;
			const decision = decideAddressedTranscript(
				text,
				call.addressing,
				call.addressingArmedUntilMs
			);
			call.addressingArmedUntilMs = decision.armedUntilMs;
			sendControl(call.ws, 'transcript.user', { text, item_id: itemId });
			if (decision.kind !== 'admitted' || !decision.text) {
				removeTurn(itemId);
				call.suppressAmbientResponse = true;
				call.activeAudioResponseId = null;
				call.currentProviderResponseId = null;
				call.provider?.interruptResponse();
				voiceTranscriptStore.update((state) => ({
					...state,
					assistantSpeaking: false
				}));
				if (call.halfDuplex) call.provider?.setMicEnabled(true);
				return;
			}
			call.suppressAmbientResponse = false;
			if (!call.tutorTakeoverActive && call.tutorSuppressedResponseId === null) {
				call.tutorResponseFenceActive = false;
			}
			upsertTurn({
				id: itemId,
				speaker: 'user',
				text: decision.text,
				timestamp: Date.now(),
				done: true
			});
		},
		onTranscriptAssistantDelta({ responseId, text }) {
			if (active !== call) return;
			if (shouldSuppressTutorProviderResponse(call, responseId, false)) return;
			call.currentProviderResponseId = responseId;
			if (call.suppressAmbientResponse) return;
			upsertAssistantDelta(responseId, text);
			voiceTranscriptStore.update((s) => ({ ...s, assistantSpeaking: true }));
		},
		onTranscriptAssistantFinal({ responseId, text }) {
			if (active !== call) return;
			if (shouldSuppressTutorProviderResponse(call, responseId, true)) return;
			if (call.currentProviderResponseId === null) {
				call.currentProviderResponseId = responseId;
			}
			if (call.suppressAmbientResponse) return;
			upsertTurn({
				id: `assistant-${responseId}`,
				speaker: 'assistant',
				text,
				timestamp: Date.now(),
				done: true
			});
			if (active) sendControl(active.ws, 'transcript.assistant', {
				text,
				response_id: responseId
			});
		},
		onResponseStarted({ responseId } = {}) {
			if (active !== call || descriptor.topology !== 'direct_peer_to_peer') return;
			if (responseId && shouldSuppressTutorProviderResponse(call, responseId, false)) return;
			if (responseId) call.currentProviderResponseId = responseId;
			voiceTranscriptStore.update(s => ({ ...s, assistantSpeaking: true }));
			sendControl(call.ws, 'response.started', {
				response_id: responseId
			});
		},
		onResponseDone({ responseId, inputTokens, outputTokens, usage }) {
			if (active !== call) return;
			const distinctCurrentResponse = Boolean(
				responseId
				&& call.currentProviderResponseId
				&& call.currentProviderResponseId !== responseId
			);
			if (responseId) {
				// Provider response completion can race ahead of the final caption.
				// Claim/retain the exact fence here; only the matching assistant
				// transcript final releases it.
				shouldSuppressTutorProviderResponse(call, responseId, false);
			}
			const suppressed = !distinctCurrentResponse && call.suppressAmbientResponse;
			if (!distinctCurrentResponse) {
				if (responseId && call.currentProviderResponseId === responseId) {
					call.currentProviderResponseId = null;
				}
				call.suppressAmbientResponse = false;
				if (call.tutorSuppressedResponseId === null) {
					call.tutorResponseFenceActive = false;
				}
				voiceTranscriptStore.update((s) => ({ ...s, assistantSpeaking: false }));
			}
			if (active) {
				sendControl(active.ws, 'token.usage', {
					response_id: responseId,
					input_tokens: inputTokens,
					output_tokens: outputTokens,
					usage,
					context_window_tokens: active.contextWindowTokens ?? undefined
				});
			}
			if (suppressed && call.halfDuplex) call.provider?.setMicEnabled(true);
		},
		onResponseFailed({ responseId, terminalState }) {
			if (active !== call) return;
			const distinctCurrentResponse = Boolean(
				responseId
				&& call.currentProviderResponseId
				&& call.currentProviderResponseId !== responseId
			);
			if (responseId) {
				shouldSuppressTutorProviderResponse(call, responseId, false);
			}
			if (!distinctCurrentResponse) {
				if (responseId && call.currentProviderResponseId === responseId) {
					call.currentProviderResponseId = null;
				}
				call.suppressAmbientResponse = false;
				if (call.tutorSuppressedResponseId === null) {
					call.tutorResponseFenceActive = false;
				}
				voiceTranscriptStore.update((state) => ({ ...state, assistantSpeaking: false }));
			}
			sendControl(call.ws, 'response.failed', {
				response_id: responseId,
				terminal_state: terminalState
			});
			if (!distinctCurrentResponse && call.halfDuplex) call.provider?.setMicEnabled(true);
		},
		onFunctionCall({ responseId, callId, name, argumentsJson }) {
			if (active !== call) return;
			if (call.tutorResponseFenceActive && (
				!responseId || shouldSuppressTutorProviderResponse(call, responseId, false)
			)) {
				call.provider?.sendToolResult({
					callId,
					output: JSON.stringify({ error: 'guided voice flow owns this turn' })
				});
				call.provider?.interruptResponse();
				return;
			}
			if (responseId) call.currentProviderResponseId = responseId;
			if (call.suppressAmbientResponse) {
				call.provider?.sendToolResult({
					callId,
					output: JSON.stringify({ error: 'voice address phrase required' })
				});
				call.provider?.interruptResponse();
				return;
			}
			sendControl(call.ws, 'tool.dispatch', {
				response_id: responseId,
				tool_name: name,
				arguments_json: argumentsJson,
				call_id: callId
			});
		},
		onProviderError({ message, recoverable, code, errorType, clientEventId }) {
			console.warn('[voice] provider error', {
				message,
				recoverable,
				code,
				errorType,
				clientEventId
			});
			if (active !== call || call.ended || call.provider !== provider) return;
			if (recoverable) {
				// Keep the peer and call alive. The provider contract explicitly says
				// request-scoped errors normally leave the Realtime session open.
				setStatus({
					state: 'connected',
					error: 'That voice turn could not be completed. Please try again.'
				});
				return;
			}
			if (call.reconnectPending) return;
			call.reconnectPending = true;
			setStatus({ state: 'reconnecting', error: null });
			sendControl(call.ws, 'response.failed', { terminal_state: 'failed' });
			sendControl(call.ws, 'session.rotate', { reason: 'reconnect' });
		},
		onAudioFrameOut(bytes) {
			// `BackendProxied` providers emit captured mic PCM
			// here. Forward upstream as a binary WS frame; the
			// control-WS actor pushes it into the provider session
			// via the audio channel. Silently dropped when the
			// socket is closing (last-chunk race).
			if (active?.ws.readyState === WebSocket.OPEN) {
				try {
					active.ws.send(bytes);
				} catch (err) {
					console.debug('[voice] outgoing audio send failed', err);
				}
			}
		}
	};
	// Publish identity before connecting so provider callbacks that arrive during
	// WebRTC setup can prove they belong to the current generation. A failed
	// connect is rolled back before the error leaves this function.
	call.provider = provider;
	try {
		await provider.connect({
			descriptor,
			mic: call.mic,
			playbackEl: call.playbackEl,
			// The shared store configures only vendor Realtime. Named Hands-free is
			// always continuous and owns turn boundaries through its surface profile.
			pushToTalkMode: providerPushToTalkMode,
			sessionConfig: {
				instructions: call.sessionConfig.instructions,
				tools: call.sessionConfig.tools,
				transcriptionModel: call.transcriptionModel,
				turnDetectionMode: call.turnDetectionMode || 'server_vad',
				deferResponseUntilContext: call.deferResponseUntilContext
			},
			resume,
			callbacks,
			configurationUpdateId
		});
	} catch (error) {
		if (call.provider === provider) {
			provider.disconnect();
			call.provider = null;
		}
		throw error;
	}
	if (pendingPttEngageOnProviderReady && active === call && providerPushToTalkMode) {
		engagePushToTalk();
	}
}

function toProviderResume(wire: ResumeContextWire | undefined | null): ProviderResumeContext | null {
	if (!wire) return null;
	return {
		summary: wire.summary ?? null,
		recent_turns: (wire.recent_turns ?? []).map((t) => ({ role: t.role, text: t.text })),
		tool_exchanges: (wire.tool_exchanges ?? []).map((exchange) => ({
			call_id: exchange.call_id,
			tool_name: exchange.tool_name,
			arguments: exchange.arguments,
			projected_result: exchange.projected_result
		}))
	};
}

// ─── Backend-pushed events ──────────────────────────────────────────

function onTaskCompleted(payload: TaskCompletedPayload): void {
	if (!active || !active.provider) return;
	// Backend renders the announcement via PromptManager — single
	// source of truth for the spoken copy. Skip on missing render
	// rather than fabricating English here.
	const announcement = payload.announcement?.trim();
	if (!announcement) {
		console.debug(
			'[voice] task.completed without rendered announcement; skipping speak',
			payload.task_id
		);
		return;
	}
	active.provider.injectSystemMessage({ text: announcement, requestResponse: true });
}

function onToolResult(payload: ToolResultPayload): void {
	if (!active || !active.provider) return;
	active.provider.sendToolResult({ callId: payload.call_id, output: payload.output });
	if (active.suppressAmbientResponse) active.provider.interruptResponse();
}

function onToolCatalogUpdate(payload: ToolCatalogUpdatePayload): void {
	if (!active || !active.provider || !active.sessionConfig) return;
	active.sessionConfig = {
		...active.sessionConfig,
		tools: payload.tools
	};
	const updateSession = active.provider.updateSession;
	if (!updateSession) {
		sendControl(active.ws, 'session.rotate', { reason: 'reconnect' });
		return;
	}
	updateSession.call(active.provider, {
		sessionConfig: {
			instructions: active.sessionConfig.instructions,
			tools: active.sessionConfig.tools,
			transcriptionModel: active.transcriptionModel,
			turnDetectionMode: active.turnDetectionMode || 'server_vad',
			deferResponseUntilContext: active.deferResponseUntilContext
		},
		pushToTalkMode: active.voiceMode === 'realtime' && get(pushToTalkMode),
		updateId: payload.update_id
	});
}

function onTurnContextReady(payload: TurnContextReadyPayload): void {
	if (!active || !active.provider || !active.deferResponseUntilContext) return;
	const contextItemId = String(payload.context_item_id ?? '').trim();
	if (!contextItemId) return;
	active.provider.respondWithTurnContext({
		contextItemId,
		context: typeof payload.context === 'string' ? payload.context : null
	});
}

function shouldSuppressTutorProviderResponse(
	call: ActiveCall,
	responseId: string,
	terminal: boolean
): boolean {
	if (!call.tutorResponseFenceActive) return false;
	if (call.tutorSuppressedResponseId === null) {
		call.tutorSuppressedResponseId = responseId;
	}
	if (call.tutorSuppressedResponseId !== responseId) return false;
	if (terminal) {
		if (call.currentProviderResponseId === responseId) {
			call.currentProviderResponseId = null;
		}
		call.tutorResponseFenceActive = false;
		call.tutorSuppressedResponseId = null;
		call.suppressAmbientResponse = false;
	}
	return true;
}

function onTutorTakeoverStarted(): void {
	if (!active || !active.provider) return;
	active.tutorTakeoverActive = true;
	active.tutorResponseFenceActive = true;
	active.tutorSuppressedResponseId =
		active.currentProviderResponseId ?? active.activeAudioResponseId;
	active.suppressAmbientResponse = true;
	active.currentProviderResponseId = null;
	active.activeAudioResponseId = null;
	active.provider.interruptResponse();
	removeAssistantTurnsAfterLatestUser();
	voiceTranscriptStore.update((s) => ({ ...s, assistantSpeaking: false }));
}

function onTutorTakeoverCompleted(): void {
	if (active) active.tutorTakeoverActive = false;
	voiceTranscriptStore.update((s) => ({ ...s, assistantSpeaking: false }));
}

function onTutorTakeoverFailed(payload: Record<string, unknown>): void {
	if (active) {
		active.tutorTakeoverActive = false;
		active.suppressAmbientResponse = false;
		if (active.tutorSuppressedResponseId === null) {
			active.tutorResponseFenceActive = false;
		}
	}
	voiceTranscriptStore.update((s) => ({ ...s, assistantSpeaking: false }));
	const error = String(payload.error ?? '').trim();
	if (error) {
		console.warn('[voice] tutor takeover failed', error);
	}
	const message = String(payload.message ?? '').trim() || (
		error.toLowerCase().includes('screen capture')
			? "I couldn't capture the screen, so I didn't start that guided flow. Please check Screen Recording permission and try again."
			: "I couldn't start that guided session. Nothing was changed; please try again."
	);
	showError('Guided voice flow could not start', message);
	if (active?.provider && payload.backend_announced !== true) {
		active.provider.injectSystemMessage({ text: message, requestResponse: true });
	}
}

// Streaming delegate_to_chat: each chunk is one finalized `<speech>`
// segment from the background chat-LLM run. We inject it as a system
// message + `response.create` so the realtime model speaks it. The
// realtime model treats sequential `response.create` frames as a
// turn-by-turn flow — each chunk plays after the previous one
// finishes, no manual barge-in handling needed.
//
// We use `injectSystemMessage` (the same path `task.completed` uses)
// because that's the established protocol for backend-pushed
// utterances; the realtime model speaks the injected text verbatim
// rather than re-summarising. The text already went through the chat-
// LLM's `<speech>` tag protocol so it's pre-shaped for audio (≤30
// words per segment, conversational tone) — no additional
// post-processing here.
function onDelegateToChatChunk(payload: DelegateToChatChunkPayload): void {
	if (!active || !active.provider) return;
	const text = payload.text?.trim();
	if (!text) {
		console.debug('[voice] delegate_to_chat.chunk with empty text', payload.call_id);
		return;
	}
	active.provider.injectSystemMessage({ text, requestResponse: true });
}

// Stream finished — usually a no-op since each chunk already spoke
// itself. Two cases warrant attention:
//  1. Failure with zero chunks: nothing was ever spoken. Inject a
//     brief honest fallback so the user isn't left hanging.
//  2. Failure mid-stream: the user heard partial content; the
//     orchestrator already persisted the error to the ledger. We log
//     for observability but don't speak again — interrupting an
//     in-flight chunk would feel jarring.
function onDelegateToChatDone(payload: DelegateToChatDonePayload): void {
	if (!active || !active.provider) return;
	if (!payload.success && payload.chunk_count === 0) {
		const message = payload.error
			? `I couldn't get a clear answer from the deeper reasoner: ${payload.error}`
			: "I couldn't get a clear answer from the deeper reasoner. Want to try a more specific question?";
		active.provider.injectSystemMessage({ text: message, requestResponse: true });
		return;
	}
	if (!payload.success) {
		console.warn(
			'[voice] delegate_to_chat failed mid-stream after',
			payload.chunk_count,
			'chunks:',
			payload.error
		);
	}
}

// ─── Mic + audio plumbing ───────────────────────────────────────────

async function openMicWithAnalyser(): Promise<{
	stream: MediaStream;
	audioCtx: AudioContext | null;
	analyser: AnalyserNode | null;
	echoCancellation: boolean;
}> {
	const stream = await navigator.mediaDevices.getUserMedia({
		audio: {
			channelCount: 1,
			echoCancellation: true,
			noiseSuppression: true,
			autoGainControl: true
		}
	});
	const echoCancellation = stream.getAudioTracks()[0]?.getSettings().echoCancellation === true;
	let audioCtx: AudioContext | null = null;
	let analyser: AnalyserNode | null = null;
	try {
		audioCtx = new AudioContext();
		const source = audioCtx.createMediaStreamSource(stream);
		analyser = audioCtx.createAnalyser();
		analyser.fftSize = 1024;
		source.connect(analyser);
	} catch (err) {
		console.warn('[voice] audio analyser unavailable', err);
	}
	return { stream, audioCtx, analyser, echoCancellation };
}

function createHiddenAudioElement(): HTMLAudioElement {
	const el = document.createElement('audio');
	el.autoplay = true;
	el.setAttribute('playsinline', 'true');
	el.style.display = 'none';
	document.body.appendChild(el);
	return el;
}

async function cleanupPendingStart(
	mic: MediaStream | null,
	audioCtx: AudioContext | null,
	playbackEl: HTMLAudioElement | null,
	ws: WebSocket | null
): Promise<void> {
	for (const track of mic?.getTracks() ?? []) {
		try { track.stop(); } catch { /* no-op */ }
	}
	if (audioCtx) await audioCtx.close().catch(() => {});
	if (playbackEl) {
		playbackEl.srcObject = null;
		playbackEl.remove();
	}
	if (ws) {
		try { ws.close(); } catch { /* no-op */ }
	}
}

// ─── Turn store helper ──────────────────────────────────────────────

function upsertTurn(turn: VoiceTurn): void {
	voiceTranscriptStore.update((s) => {
		const idx = s.turns.findIndex((t) => t.id === turn.id);
		if (idx === -1) return { ...s, turns: [...s.turns, turn] };
		const copy = s.turns.slice();
		copy[idx] = turn;
		return { ...s, turns: copy };
	});
}

/** GPT P2P sends a growing snapshot; Gemini/OpenAI backend may send a fragment. */
function mergeStreamingCaption(existing: string, incoming: string): string {
	if (!incoming) return existing;
	if (!existing) return incoming;
	if (incoming.startsWith(existing)) return incoming;
	if (existing.startsWith(incoming)) return existing;
	return existing + incoming;
}

function upsertAssistantDelta(responseId: string, incoming: string): void {
	if (!incoming) return;
	const id = `assistant-${responseId}`;
	voiceTranscriptStore.update((s) => {
		const idx = s.turns.findIndex((t) => t.id === id);
		const existing = idx >= 0 ? s.turns[idx].text : '';
		const turn: VoiceTurn = {
			id,
			speaker: 'assistant',
			text: mergeStreamingCaption(existing, incoming),
			timestamp: Date.now(),
			done: false
		};
		if (idx === -1) return { ...s, turns: [...s.turns, turn] };
		const copy = s.turns.slice();
		copy[idx] = turn;
		return { ...s, turns: copy };
	});
}

function removeTurn(id: string): void {
	voiceTranscriptStore.update((state) => ({
		...state,
		turns: state.turns.filter((turn) => turn.id !== id)
	}));
}

function removeUnfinishedUserTurns(): void {
	voiceTranscriptStore.update((state) => ({
		...state,
		turns: state.turns.filter((turn) => turn.speaker !== 'user' || turn.done)
	}));
}

function removeAssistantTurnsAfterLatestUser(): void {
	voiceTranscriptStore.update((state) => {
		let latestUserIndex = -1;
		for (let index = state.turns.length - 1; index >= 0; index -= 1) {
			if (state.turns[index]?.speaker === 'user') {
				latestUserIndex = index;
				break;
			}
		}
		if (latestUserIndex < 0) return state;
		return {
			...state,
			turns: state.turns.filter((turn, index) => (
				index <= latestUserIndex || turn.speaker !== 'assistant'
			))
		};
	});
}

// ─── Teardown ───────────────────────────────────────────────────────

async function teardownActive(reason: 'clean' | 'error'): Promise<void> {
	providerOutputPlaying = false;
	lastConcurrentContext = null;
	const call = active;
	active = null;
	pendingPttEngageOnProviderReady = false;
	pushToTalkActive.set(false);
	pttEngagedAt = 0;
	voiceMicAnalyser.set(null);
	sessionCapMsStore.set(null);
	if (!call) {
		if (reason === 'clean') {
			setStatus({
				state: 'idle',
				error: null,
				connectedAt: null,
				activationPhrase: null,
				activationPhrases: [],
				addressingRequired: false
			});
		} else {
			setStatus({ state: 'error', connectedAt: null, activationPhrase: null,
			activationPhrases: [],
			addressingRequired: false });
		}
		return;
	}
	call.ended = true;
	if (call.controlReconnectTimer !== null) {
		window.clearTimeout(call.controlReconnectTimer);
		call.controlReconnectTimer = null;
	}
	call.provider?.disconnect();
	call.provider = null;
	for (const t of call.mic.getTracks()) {
		try {
			t.stop();
		} catch {
			/* no-op */
		}
	}
	if (call.audioCtx) {
		call.audioCtx.close().catch(() => {});
	}
	if (call.playbackEl.srcObject instanceof MediaStream) {
		for (const t of call.playbackEl.srcObject.getTracks()) {
			try {
				t.stop();
			} catch {
				/* no-op */
			}
		}
	}
	call.playbackEl.srcObject = null;
	call.playbackEl.remove();
	try {
		call.ws.close();
	} catch {
		/* no-op */
	}
	voiceTranscriptStore.update((s) => ({
		...s,
		userSpeaking: false,
		assistantSpeaking: false,
		assistantWorking: false
	}));
	if (reason === 'clean') {
		setStatus({
			state: 'idle',
			error: null,
			connectedAt: null,
			activationPhrase: null,
			activationPhrases: [],
			addressingRequired: false
		});
	} else {
		setStatus({ state: 'error', connectedAt: null, activationPhrase: null,
		activationPhrases: [],
		addressingRequired: false });
	}
}
