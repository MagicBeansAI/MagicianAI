/**
 * Provider-backed TTS over the magician
 * `POST /media/tts/synthesize_message` endpoint.
 *
 * The frontend never calls the legacy single-shot `/synthesize`
 * endpoint anymore — `providerSpeakBlocks` always uses the streamed
 * segment endpoint, even for a one-block reply, because:
 *
 *  - One round-trip instead of N.
 *  - The server runs the chain rotation contract per segment so we
 *    don't replicate fallback semantics on the client.
 *  - The cache decorator on each provider catches repeat utterances.
 *
 * The legacy `/synthesize` endpoint stays on the backend for ad-hoc
 * API callers (CLI tools, future external integrations).
 */

import { browser } from '$app/environment';

import { publishMediaEvent } from '$lib/media/session';
import {
	MEDIA_TTS_CANCELLED,
	MEDIA_TTS_COMPLETED,
	MEDIA_TTS_ERROR,
	MEDIA_TTS_STARTED
} from '$lib/media/types';

import type { SpeechBlock } from './speechTags';
import { planGaplessAudio } from './gaplessAudio';

/**
 * Monotonic id of the most recently kicked-off block queue. Each
 * `providerSpeakBlocks` call snapshots this value on entry and after
 * every await; if the snapshot no longer matches, the queue exits
 * early because a newer caller (or an explicit cancel) has taken over.
 */
let blockQueueGeneration = 0;

interface ActiveAudio {
	messageId: string;
	startedAt: number;
	/** Stop the current playback source without reporting a second terminal event. */
	stop: () => void;
	/** Idempotently release any browser resources held by the source. */
	cleanup: () => void;
}

let active: ActiveAudio | null = null;
let providerAudioContext: AudioContext | null = null;
/**
 * AbortController for the in-flight `synthesize_message` fetch. Set
 * at the start of `providerSpeakBlocks`, cleared when the NDJSON
 * stream completes or errors. `cancelProviderSpeak` aborts whatever
 * is in-flight so a cancel during synth tears down the HTTP request
 * instead of letting it complete and play a moment later.
 */
let activeAbort: AbortController | null = null;

function clearActive(): void {
	const prior = active;
	if (!prior) return;
	// Clear the shared reference before stopping. Both Web Audio and
	// HTMLAudioElement synchronously/asynchronously report their stop event;
	// those handlers then recognize this as an external cancellation.
	active = null;
	try {
		prior.stop();
	} catch {}
	prior.cleanup();
}

/**
 * Create and resume Web Audio while the caller still holds the Speak button's
 * user activation. TTS synthesis is asynchronous, so calling HTMLAudioElement
 * `play()` only after the response arrives is commonly rejected by Chromium's
 * autoplay policy. A resumed context keeps the later decoded audio playable.
 */
function unlockProviderAudioContext(): AudioContext | null {
	try {
		const AudioContextConstructor =
			window.AudioContext ??
			(window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
		if (!AudioContextConstructor) return null;

		if (!providerAudioContext || providerAudioContext.state === 'closed') {
			providerAudioContext = new AudioContextConstructor();
		}
		if (providerAudioContext.state !== 'running') {
			// This must be invoked before the first await in providerSpeakBlocks.
			// Do not await it: Safari may wait for a later activation, whereas a
			// regular user-initiated click transitions it immediately.
			void providerAudioContext.resume().catch(() => {});
		}
		return providerAudioContext;
	} catch {
		return null;
	}
}

/**
 * Prime provider playback while a caller still owns a real user gesture.
 * Ambient Dictation receives synthesized audio only after capture, STT, and an
 * agent turn, when browser autoplay activation has otherwise expired.
 */
export function primeProviderTtsPlayback(): void {
	if (!browser) return;
	unlockProviderAudioContext();
}

function abortInFlight(): void {
	if (!activeAbort) return;
	try {
		activeAbort.abort();
	} catch {
		// AbortController.abort() throws nowhere in spec, but guard
		// against polyfills / browser quirks.
	}
	activeAbort = null;
}

export function cancelProviderSpeak(reason: 'user' | 'replaced' = 'user'): void {
	// Only user-initiated cancels bump the queue generation. A fresh
	// `providerSpeakBlocks` call invokes this with reason='replaced'
	// to tear down the prior queue's playback before claiming its own
	// generation; bumping on that path would make the new caller
	// invalidate its own generation token on entry.
	if (reason === 'user') {
		blockQueueGeneration += 1;
	}
	// Tear down any in-flight fetch first. We do this BEFORE checking
	// `active` because the cancel can race with the synth phase — the
	// fetch may have started but the audio element doesn't exist yet,
	// so `active` is still null. Without this, a quick cancel would
	// let the download complete and play a moment later.
	abortInFlight();
	if (!active) return;
	const prior = active;
	clearActive();
	void publishMediaEvent(MEDIA_TTS_CANCELLED, {
		message_id: prior.messageId,
		reason,
		duration_ms: Date.now() - prior.startedAt,
		via: 'provider'
	});
}

export interface ProviderSpeakBlocksOptions {
	messageId: string;
	blocks: SpeechBlock[];
	provider?: string | null;
	voice?: string | null;
	model?: string | null;
	rate?: number | null;
	format?: string | null;
	onStart?: () => void;
}

interface SegmentEnvelope {
	type: 'segment' | 'error' | 'done';
	index?: number;
	message_id?: string;
	audio_b64?: string;
	content_type?: string;
	provider?: string;
	model?: string;
	fallback?: boolean;
	attempts?: string;
	code?: string;
	message?: string;
	segment_count?: number;
}

function decodeBase64ToBlob(b64: string, mime: string): Blob {
	const binary = atob(b64);
	const bytes = new Uint8Array(binary.length);
	for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
	return new Blob([bytes], { type: mime });
}

/**
 * Synthesize and play a sequence of `SpeechBlock`s by streaming
 * through the backend's `POST /tts/synthesize_message` endpoint.
 *
 * One HTTP request instead of N per-block round-trips: the server
 * walks the segments, drives the TTS provider chain per segment,
 * and streams each segment's audio back as an NDJSON envelope. The
 * client decodes the base64, queues the audio, and prebuffers the complete
 * message before scheduling every decoded segment on one contiguous WebAudio
 * timeline. That small multi-block latency trade keeps independently generated
 * clips from dropping phonemes at JavaScript `onended` boundaries.
 *
 * Cancellation: a concurrent `cancelProviderSpeak()` (or a fresh
 * `providerSpeakBlocks` call) bumps the queue generation AND
 * aborts the in-flight fetch. The reader exits, queued segments are
 * dropped, and the current audio is paused.
 *
 * The `onEnd` callback fires exactly once with the terminal status.
 * Per-segment `error` envelopes don't fail the whole call — partial
 * audio is better than silence, so we play what we have and report
 * `completed` if at least one segment played end-to-end. If every
 * segment errored, the status is `error`.
 */
export async function providerSpeakBlocks(
	opts: ProviderSpeakBlocksOptions,
	onEnd?: (status: 'completed' | 'cancelled' | 'error') => void
): Promise<void> {
	if (!browser) {
		onEnd?.('error');
		return;
	}
	if (opts.blocks.length === 0) {
		onEnd?.('completed');
		return;
	}
	// Must happen while the Speak click still has browser user activation.
	const playbackContext = unlockProviderAudioContext();

	// Stop any in-flight playback and claim this generation. We bump
	// explicitly so the first iteration sees a stable token.
	cancelProviderSpeak('replaced');
	blockQueueGeneration += 1;
	const myGeneration = blockQueueGeneration;
	const abortController = new AbortController();
	activeAbort = abortController;

	let playedAnySegment = false;
	let startedAnySegment = false;
	let sawError = false;
	let terminalCalled = false;
	const callTerminal = (status: 'completed' | 'cancelled' | 'error') => {
		if (terminalCalled) return;
		terminalCalled = true;
		onEnd?.(status);
	};

	// The server synthesizes each speech block independently. We retain every
	// envelope until the message is complete, decode the clips, then schedule
	// them on one AudioContext timeline. Starting clip N only from clip N-1's
	// `onended` callback crosses the JS event loop and creates audible holes.
	const playbackQueue: Array<{ blob: Blob; provider: string; model: string }> = [];
	let queueRunning = false;
	let queueDone = false;
	const playSegmentsWithWebAudio = async (
		segments: typeof playbackQueue,
		markStarted: (next: (typeof playbackQueue)[number]) => void
	): Promise<'completed' | 'cancelled' | 'error'> => {
		const context = playbackContext;
		if (!context) return 'error';

		const decoded: Array<{
			segment: (typeof playbackQueue)[number];
			buffer: AudioBuffer;
		}> = [];
		for (const segment of segments) {
			try {
				const buffer = await context.decodeAudioData(await segment.blob.arrayBuffer());
				decoded.push({ segment, buffer });
			} catch (error) {
				if (myGeneration !== blockQueueGeneration) return 'cancelled';
				sawError = true;
				void publishMediaEvent(MEDIA_TTS_ERROR, {
					message_id: opts.messageId,
					reason: error instanceof Error ? error.message : 'web_audio_decode_failed',
					via: 'provider'
				});
			}
		}
		if (myGeneration !== blockQueueGeneration) return 'cancelled';
		if (decoded.length === 0) return 'error';

		return new Promise((resolve) => {
			const sources = decoded.map(({ buffer }) => {
				const source = context.createBufferSource();
				source.buffer = buffer;
				source.connect(context.destination);
				return source;
			});
			const startedAt = Date.now();
			let settled = false;
			let cleanedUp = false;
			let completedSources = 0;
			const cleanup = () => {
				if (cleanedUp) return;
				cleanedUp = true;
				for (const source of sources) {
					try {
						source.disconnect();
					} catch {
						// A source may already be disconnected during cancellation.
					}
				}
			};
			let entry: ActiveAudio | null = null;
			const finish = (status: 'completed' | 'cancelled' | 'error') => {
				if (settled) return;
				settled = true;
				if (entry && active === entry) active = null;
				cleanup();
				resolve(status);
			};
			entry = {
				messageId: opts.messageId,
				startedAt,
				stop: () => {
					for (const source of sources) {
						try {
							source.stop();
						} catch {
							// Stopping an already-ended scheduled source is harmless.
						}
					}
				},
				cleanup
			};
			sources.forEach((source, index) => {
				source.onended = () => {
					if (!entry || active !== entry) {
						finish('cancelled');
						return;
					}
					completedSources += 1;
					void publishMediaEvent(MEDIA_TTS_COMPLETED, {
						message_id: opts.messageId,
						duration_ms: Math.round(decoded[index].buffer.duration * 1000),
						via: 'provider'
					});
					if (completedSources === sources.length) finish('completed');
				};
			});

			active = entry;
			try {
				// A small lead lets every source reach the audio render thread before
				// the first sample is due. Each following source starts at the exact
				// previous AudioBuffer end — no timer/onended hand-off and no overlap.
				const slots = planGaplessAudio(
					decoded.map(({ buffer }) => buffer.duration),
					context.currentTime + 0.025
				);
				sources.forEach((source, index) => {
					source.start(slots[index].startAt);
					markStarted(decoded[index].segment);
				});
			} catch (error) {
				if (entry && active === entry) {
					void publishMediaEvent(MEDIA_TTS_ERROR, {
						message_id: opts.messageId,
						reason: error instanceof Error ? error.message : 'web_audio_start_failed',
						via: 'provider'
					});
				}
				entry?.stop();
				finish('error');
			}
		});
	};

	const playSegmentWithAudioElement = (
		next: (typeof playbackQueue)[number],
		markStarted: () => void
	): Promise<'completed' | 'cancelled' | 'error'> => {
		const objectUrl = URL.createObjectURL(next.blob);
		const audio = new Audio(objectUrl);
		const startedAt = Date.now();
		let settled = false;
		let cleanedUp = false;
		const cleanup = () => {
			if (cleanedUp) return;
			cleanedUp = true;
			try {
				URL.revokeObjectURL(objectUrl);
			} catch {
				// no-op
			}
		};

		return new Promise((resolve) => {
			let entry: ActiveAudio | null = null;
			const finish = (status: 'completed' | 'cancelled' | 'error') => {
				if (settled) return;
				settled = true;
				if (entry && active === entry) active = null;
				cleanup();
				resolve(status);
			};
			entry = {
				messageId: opts.messageId,
				startedAt,
				stop: () => audio.pause(),
				cleanup
			};
			audio.addEventListener('play', markStarted, { once: true });
			audio.addEventListener('playing', markStarted, { once: true });
			audio.addEventListener(
				'ended',
				() => {
					if (!entry || active !== entry) {
						finish('cancelled');
						return;
					}
					void publishMediaEvent(MEDIA_TTS_COMPLETED, {
						message_id: opts.messageId,
						duration_ms: Date.now() - startedAt,
						via: 'provider'
					});
					finish('completed');
				},
				{ once: true }
			);
			audio.addEventListener(
				'error',
				() => {
					if (entry && active === entry) {
						void publishMediaEvent(MEDIA_TTS_ERROR, {
							message_id: opts.messageId,
							reason: 'audio_element_error',
							via: 'provider'
						});
						finish('error');
					} else {
						finish('cancelled');
					}
				},
				{ once: true }
			);
			audio.addEventListener('pause', () => {
				if (audio.ended) return;
				if (entry && active === entry) {
					void publishMediaEvent(MEDIA_TTS_CANCELLED, {
						message_id: opts.messageId,
						duration_ms: Date.now() - startedAt,
						reason: 'paused',
						via: 'provider'
					});
				}
				finish('cancelled');
			});

			active = entry;
			audio
				.play()
				.then(markStarted)
				.catch((error) => {
					if (entry && active === entry) {
						void publishMediaEvent(MEDIA_TTS_ERROR, {
							message_id: opts.messageId,
							reason: error instanceof Error ? error.message : 'play_threw',
							via: 'provider'
						});
						finish('error');
					} else {
						finish('cancelled');
					}
				});
		});
	};
	const startedSegments = new Set<(typeof playbackQueue)[number]>();
	const markSegmentStarted = (next: (typeof playbackQueue)[number]) => {
		if (startedSegments.has(next)) return;
		startedSegments.add(next);
		void publishMediaEvent(MEDIA_TTS_STARTED, {
			message_id: opts.messageId,
			voice: opts.voice ?? null,
			text_length: 0,
			via: 'provider',
			provider: next.provider,
			model: next.model
		});
		if (!startedAnySegment) {
			startedAnySegment = true;
			opts.onStart?.();
		}
	};
	const startQueueIfIdle = async () => {
		if (queueRunning || !queueDone) return;
		queueRunning = true;
		if (playbackContext && playbackQueue.length > 0) {
			const scheduled = playbackQueue.splice(0, playbackQueue.length);
			const endStatus = await playSegmentsWithWebAudio(scheduled, markSegmentStarted);
			if (endStatus === 'completed') {
				playedAnySegment = true;
			} else if (endStatus === 'cancelled') {
				queueRunning = false;
				callTerminal('cancelled');
				return;
			} else {
				sawError = true;
			}
		}
		while (playbackQueue.length > 0) {
			if (myGeneration !== blockQueueGeneration) {
				queueRunning = false;
				return;
			}
			const next = playbackQueue.shift()!;
			const endStatus = await playSegmentWithAudioElement(
				next,
				() => markSegmentStarted(next)
			);

			if (endStatus === 'completed') {
				playedAnySegment = true;
			} else if (endStatus === 'cancelled') {
				queueRunning = false;
				callTerminal('cancelled');
				return;
			} else {
				sawError = true;
			}
		}
		queueRunning = false;
		if (queueDone) {
			callTerminal(playedAnySegment ? 'completed' : sawError ? 'error' : 'completed');
		}
	};

	let response: Response;
	try {
		response = await fetch('/api/magician/v2/media/tts/synthesize_message', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			signal: abortController.signal,
			body: JSON.stringify({
				message_id: opts.messageId,
				provider: opts.provider && opts.provider !== 'auto' ? opts.provider : undefined,
				segments: opts.blocks.map((b) => ({
					text: b.text,
					emotion: b.emotion,
					style: b.style,
					pace: b.pace,
					voice_mode: b.voice_mode,
					emphasis: b.emphasis
				})),
				voice: opts.voice ?? undefined,
				model: opts.model ?? undefined,
				rate: opts.rate ?? undefined,
				format: opts.format ?? undefined
			})
		});
	} catch (error) {
		if (activeAbort === abortController) activeAbort = null;
		const aborted = error instanceof DOMException && error.name === 'AbortError';
		callTerminal(aborted ? 'cancelled' : 'error');
		return;
	}

	if (!response.ok || !response.body) {
		if (activeAbort === abortController) activeAbort = null;
		const body = response.body ? await response.text().catch(() => '') : '';
		void publishMediaEvent(MEDIA_TTS_ERROR, {
			message_id: opts.messageId,
			reason: `http_${response.status}`,
			body: body.slice(0, 400),
			via: 'provider'
		});
		callTerminal('error');
		return;
	}

	const reader = response.body.getReader();
	const decoder = new TextDecoder();
	let buffer = '';
	const consumeEnvelopes = () => {
		let nl: number;
		while ((nl = buffer.indexOf('\n')) !== -1) {
			const line = buffer.slice(0, nl).trim();
			buffer = buffer.slice(nl + 1);
			if (!line) continue;
			let env: SegmentEnvelope;
			try {
				env = JSON.parse(line) as SegmentEnvelope;
			} catch {
				continue;
			}
			if (env.type === 'segment' && env.audio_b64 && env.content_type) {
				const blob = decodeBase64ToBlob(env.audio_b64, env.content_type);
				playbackQueue.push({
					blob,
					provider: env.provider ?? 'unknown',
					model: env.model ?? 'unknown'
				});
			} else if (env.type === 'error') {
				sawError = true;
				void publishMediaEvent(MEDIA_TTS_ERROR, {
					message_id: opts.messageId,
					reason: env.code ?? 'tts_error',
					body: env.message?.slice(0, 400),
					via: 'provider'
				});
			} else if (env.type === 'done') {
				queueDone = true;
			}
		}
	};

	try {
		for (;;) {
			const { done, value } = await reader.read();
			if (done) break;
			buffer += decoder.decode(value, { stream: true });
			consumeEnvelopes();
		}
		buffer += decoder.decode();
		consumeEnvelopes();
		queueDone = true;
	} catch (error) {
		const aborted = error instanceof DOMException && error.name === 'AbortError';
		if (aborted) {
			callTerminal('cancelled');
			if (activeAbort === abortController) activeAbort = null;
			return;
		}
		sawError = true;
	} finally {
		if (activeAbort === abortController) activeAbort = null;
	}

	// The complete-message prebuffer is intentional: it lets WebAudio schedule
	// independently synthesized speech blocks on one contiguous sample timeline.
	// Starting earlier would reintroduce an unavoidable underrun whenever the
	// next provider response arrives after the current clip ends.
	await startQueueIfIdle();
	if (!terminalCalled) {
		callTerminal(playedAnySegment ? 'completed' : sawError ? 'error' : 'completed');
	}
}
