/**
 * Microphone capture via `MediaRecorder` (Phase 2).
 *
 * Honors the realtime-media-control-rails plan's "raw audio ephemeral
 * by default" rule: this module never touches the artifact ledger. It
 * just streams chunks into memory and yields a single `Blob` when the
 * caller calls `stop()`. The downstream attachment flow is what
 * (optionally) persists.
 *
 * Mime preference order — same logic OpenAI/ElevenLabs SDKs use:
 *   1. audio/webm;codecs=opus     — best quality + size, broad support
 *   2. audio/webm                 — Chromium fallback
 *   3. audio/mp4                  — Safari (iOS 17+, macOS 14+)
 *   4. browser default            — last resort, may be vendor-specific
 */

import { browser } from '$app/environment';

export interface RecordingHandle {
	stop(): Promise<RecordedAudio>;
	cancel(): void;
	/** Live `AnalyserNode` wired to the captured stream. Consumers can
	 *  read frequency data each frame to drive UI visualizations (e.g.
	 *  audio-reactive orbs). Null when WebAudio isn't available (rare). */
	analyser: AnalyserNode | null;
	/** Underlying capture stream. Exposed so callers can attach additional
	 *  AudioContext nodes if needed. The handle's own AnalyserNode is
	 *  attached to this stream already. */
	stream: MediaStream;
}

export interface RecordedAudio {
	blob: Blob;
	mimeType: string;
	durationMs: number;
}

const PREFERRED_MIME_TYPES = ['audio/webm;codecs=opus', 'audio/webm', 'audio/mp4'];

export function isMicCaptureSupported(): boolean {
	if (!browser) return false;
	return (
		typeof navigator.mediaDevices !== 'undefined'
		&& typeof navigator.mediaDevices.getUserMedia === 'function'
		&& typeof window.MediaRecorder !== 'undefined'
	);
}

function pickMimeType(): string | undefined {
	if (!browser || typeof window.MediaRecorder === 'undefined') return undefined;
	const supports = (window.MediaRecorder as unknown as { isTypeSupported?: (mt: string) => boolean })
		.isTypeSupported;
	if (typeof supports !== 'function') return undefined;
	return PREFERRED_MIME_TYPES.find((mt) => supports(mt));
}

export async function startMicRecording(): Promise<RecordingHandle> {
	if (!isMicCaptureSupported()) {
		throw new Error('mic_capture_unsupported');
	}
	const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
	const mimeType = pickMimeType();
	const recorder = new MediaRecorder(
		stream,
		mimeType ? { mimeType } : undefined
	);
	const chunks: Blob[] = [];
	recorder.addEventListener('dataavailable', (event) => {
		if (event.data && event.data.size > 0) {
			chunks.push(event.data);
		}
	});

	// Tap the same stream into a WebAudio analyser so UI components can
	// drive audio-reactive visualizations without grabbing the mic
	// separately. The analyser is owned by the handle; cleanup tears it
	// down alongside the stream tracks below.
	let audioCtx: AudioContext | null = null;
	let analyser: AnalyserNode | null = null;
	try {
		const Ctor =
			window.AudioContext
			|| (window as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
		if (Ctor) {
			audioCtx = new Ctor();
			analyser = audioCtx.createAnalyser();
			analyser.fftSize = 256;
			analyser.smoothingTimeConstant = 0.7;
			const source = audioCtx.createMediaStreamSource(stream);
			source.connect(analyser);
		}
	} catch {
		// WebAudio init can fail on some restricted contexts. Recording
		// itself still works without it; the orb just won't react.
		audioCtx = null;
		analyser = null;
	}

	const startedAt = Date.now();
	recorder.start();
	let stopped = false;

	const cleanup = () => {
		stream.getTracks().forEach((track) => {
			try {
				track.stop();
			} catch {
				// no-op
			}
		});
		if (audioCtx) {
			audioCtx.close().catch(() => {
				/* AudioContext may already be closed if the user revoked mid-recording. */
			});
			audioCtx = null;
			analyser = null;
		}
	};

	return {
		stream,
		analyser,
		async stop(): Promise<RecordedAudio> {
			if (stopped) throw new Error('recording_already_stopped');
			stopped = true;
			const ended = new Promise<RecordedAudio>((resolve) => {
				recorder.addEventListener(
					'stop',
					() => {
						const blob = new Blob(chunks, {
							type: recorder.mimeType || mimeType || 'audio/webm'
						});
						resolve({
							blob,
							mimeType: blob.type,
							durationMs: Date.now() - startedAt
						});
					},
					{ once: true }
				);
			});
			recorder.stop();
			const result = await ended;
			cleanup();
			return result;
		},
		cancel(): void {
			if (stopped) return;
			stopped = true;
			try {
				recorder.stop();
			} catch {
				// no-op
			}
			cleanup();
		}
	};
}

export function makeRecordedAudioFile(audio: RecordedAudio): File {
	const ext = mimeToExt(audio.mimeType);
	const stamp = new Date().toISOString().replace(/[:.]/g, '-');
	return new File([audio.blob], `voice-${stamp}.${ext}`, { type: audio.mimeType });
}

function mimeToExt(mime: string): string {
	const normalized = mime.toLowerCase();
	if (normalized.includes('webm')) return 'webm';
	if (normalized.includes('mp4') || normalized.includes('m4a')) return 'm4a';
	if (normalized.includes('ogg')) return 'ogg';
	if (normalized.includes('wav')) return 'wav';
	return 'webm';
}
