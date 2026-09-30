/**
 * Backend-proxied realtime voice provider — for providers whose
 * audio rides through magician instead of straight to the browser
 * (Gemini Live etc.). Captures mic PCM and emits it via the
 * orchestrator-shaped client (which forwards as WS binary frames);
 * accepts provider → browser PCM through `feedIncomingAudio` and
 * plays it out.
 *
 * Wire format here is plain 16-bit little-endian PCM mono at the
 * `OUTPUT_SAMPLE_RATE_HZ` rate below. Per-provider negotiation
 * (sample rate, channels, format) can extend the descriptor in
 * `magicllm::realtime::types` and surface here.
 *
 * Gemini Live and host-native OpenAI realtime both use this path. It still uses
 * the deprecated `ScriptProcessorNode` for capture so it works without a
 * separate AudioWorklet bundle; upgrading to `AudioWorkletNode` remains the
 * lower-latency follow-up.
 */

import type {
	ProviderDescriptor,
	ProviderResumeContext,
	ProviderSessionConfig,
	RealtimeFrontendProvider,
	RealtimeProviderCallbacks
} from './types';

/** Sample rate for both upstream + downstream PCM. Most realtime
 *  providers accept 16 kHz or 24 kHz; we pick 24 kHz as a balance
 *  of bandwidth + speech intelligibility. Browser AudioContext is
 *  forced to this rate so we don't have to resample. */
const PCM_SAMPLE_RATE_HZ = 24_000;
/** Capture window for `ScriptProcessorNode`. 1024 frames ≈ 42 ms
 *  at 24 kHz — small enough for low latency, large enough to stay
 *  well below the main-thread budget. Must be a power of two from
 *  256..16384 per the spec. */
const CAPTURE_BUFFER_SIZE = 1024;

interface InternalState {
	audioCtx: AudioContext;
	micNode: MediaStreamAudioSourceNode;
	captureNode: ScriptProcessorNode;
	playbackSources: Set<AudioBufferSourceNode>;
	playbackTimeSec: number;
	muted: boolean;
}

export class BackendProxiedFrontendProvider implements RealtimeFrontendProvider {
	private state: InternalState | null = null;
	private callbacks: RealtimeProviderCallbacks | null = null;
	private sessionConfig: ProviderSessionConfig | null = null;

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
			throw new Error(
				'BackendProxiedFrontendProvider already connected — call disconnect() first'
			);
		}
		this.callbacks = args.callbacks;
		this.sessionConfig = args.sessionConfig;

		// Detach any prior playback source so we own the audio
		// element exclusively while the call is active.
		args.playbackEl.srcObject = null;
		args.playbackEl.src = '';

		// Force the AudioContext to our wire-format rate so we don't
		// have to resample browser-side. Most realtime providers
		// accept 24 kHz directly.
		const audioCtx = new AudioContext({ sampleRate: PCM_SAMPLE_RATE_HZ });
		const micNode = audioCtx.createMediaStreamSource(args.mic);
		const captureNode = audioCtx.createScriptProcessor(CAPTURE_BUFFER_SIZE, 1, 1);

		const muted = args.pushToTalkMode; // PTT starts muted until user holds
		captureNode.onaudioprocess = (ev) => {
			if (!this.state || this.state.muted) return;
			const channel = ev.inputBuffer.getChannelData(0);
			const pcm16 = floatToPcm16Le(channel);
			const frame = new ArrayBuffer(pcm16.byteLength);
			new Int16Array(frame).set(pcm16);
			this.callbacks?.onAudioFrameOut?.(frame);
		};
		// ScriptProcessorNode only emits audioprocess events when
		// connected; tie it to a silent destination so it runs
		// without making noise on the user's speakers.
		micNode.connect(captureNode);
		captureNode.connect(audioCtx.destination);

		this.state = {
			audioCtx,
			micNode,
			captureNode,
			playbackSources: new Set(),
			playbackTimeSec: audioCtx.currentTime,
			muted
		};
		// Resume context replay isn't a frontend concern for
		// BackendProxied — the orchestrator hands the resume payload
		// to the provider backend-side, which feeds it into the
		// upstream `setup` (Gemini) or equivalent. Frontend just
		// streams audio. Acknowledge configuration immediately so a
		// `tool.catalog.update` does not rotate the call waiting for
		// a WebRTC `session.updated` that this transport never sees.
		args.callbacks.onSessionConfigured({
			updateId: args.configurationUpdateId
		});
		void args.resume;
		void args.descriptor;
	}

	disconnect(): void {
		const s = this.state;
		this.state = null;
		this.callbacks = null;
		this.sessionConfig = null;
		if (!s) return;
		try {
			s.captureNode.disconnect();
		} catch {
			/* no-op */
		}
		try {
			s.micNode.disconnect();
		} catch {
			/* no-op */
		}
		s.audioCtx.close().catch(() => {});
	}

	setMicEnabled(enabled: boolean): void {
		if (!this.state) return;
		this.state.muted = !enabled;
	}

	updateSession(args: {
		sessionConfig: ProviderSessionConfig;
		pushToTalkMode: boolean;
		updateId: string;
	}): void {
		this.sessionConfig = args.sessionConfig;
		this.setMicEnabled(!args.pushToTalkMode);
		this.callbacks?.onSessionConfigured({ updateId: args.updateId });
	}

	clearInputBuffer(): void {
		// BackendProxied providers buffer upstream-side; the browser
		// just stops sending frames when muted. Nothing local to
		// clear unless a provider needs an explicit signal — extend
		// here when that lands.
	}

	commitInputAndRespond(): void {
		// Same as above — turn boundaries are signalled via the
		// upstream protocol (server VAD or explicit
		// `clientContent.turnComplete: true` for Gemini). The PTT
		// release path would send a `user.text {text:""}` or a
		// dedicated control frame; deferred until a real provider
		// lands and we know the shape.
	}

	respondWithTurnContext(_args: { contextItemId: string; context: string | null }): void {
		// Backend-proxied providers receive this control directly from the service.
	}

	interruptResponse(): void {
		const s = this.state;
		if (!s) return;
		for (const src of Array.from(s.playbackSources)) {
			try {
				src.stop();
			} catch {
				/* already stopped */
			}
		}
		s.playbackSources.clear();
		s.playbackTimeSec = s.audioCtx.currentTime;
		this.callbacks?.onPlaybackStateChanged?.(false);
	}

	sendToolResult(_args: { callId: string; output: string }): void {
		// Tool result replay for BackendProxied: provider backend
		// inserts into the upstream session directly. Frontend
		// doesn't need to ship it through; the orchestrator handles
		// this on the backend side when it receives `tool.result`
		// from the control WS (today the OpenAI path forwards via
		// data channel; the Gemini path will inject upstream).
	}

	injectSystemMessage(_args: { text: string; requestResponse: boolean }): void {
		// Same as above — backend-side injection for BackendProxied.
		// Frontend receives the `task.completed` envelope with the
		// announcement but the actual model-side injection happens
		// upstream. No-op here.
	}

	feedIncomingAudio(bytes: ArrayBuffer): void {
		const s = this.state;
		if (!s) return;
		const pcm16 = new Int16Array(bytes);
		const floatBuffer = pcm16ToFloat(pcm16);
		const audioBuffer = s.audioCtx.createBuffer(1, floatBuffer.length, PCM_SAMPLE_RATE_HZ);
		audioBuffer.copyToChannel(floatBuffer, 0);
		const src = s.audioCtx.createBufferSource();
		src.buffer = audioBuffer;
		src.connect(s.audioCtx.destination);
		if (s.playbackSources.size === 0) this.callbacks?.onPlaybackStateChanged?.(true);
		s.playbackSources.add(src);
		src.onended = () => {
			s.playbackSources.delete(src);
			if (this.state === s && s.playbackSources.size === 0) this.callbacks?.onPlaybackStateChanged?.(false);
		};
		// Schedule contiguously — the next buffer starts when this
		// one ends. Falls back to `currentTime` if we drifted behind
		// (network hiccup) so we don't keep stacking late frames.
		const startAt = Math.max(s.playbackTimeSec, s.audioCtx.currentTime);
		src.start(startAt);
		s.playbackTimeSec = startAt + audioBuffer.duration;
	}
}

function floatToPcm16Le(input: Float32Array): Int16Array<ArrayBuffer> {
	const output = new Int16Array(input.length);
	for (let i = 0; i < input.length; i += 1) {
		const clamped = Math.max(-1, Math.min(1, input[i]));
		output[i] = clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff;
	}
	return output;
}

function pcm16ToFloat(input: Int16Array): Float32Array<ArrayBuffer> {
	const output = new Float32Array(input.length);
	for (let i = 0; i < input.length; i += 1) {
		const v = input[i];
		output[i] = v < 0 ? v / 0x8000 : v / 0x7fff;
	}
	return output;
}
