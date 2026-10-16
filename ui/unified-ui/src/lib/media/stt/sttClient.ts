/**
 * Provider-backed speech-to-text client (Phase 3).
 *
 * Uploads a recorded audio blob to `/media/stt/transcribe` and returns
 * the transcript. Used by MicCaptureButton's "Transcribe" action when
 * the backend has a STT provider registered.
 *
 * Browser-native `SpeechRecognition` is a separate path (mostly
 * desktop Chrome only). Phase 3 prioritises the provider route since
 * Whisper / gpt-transcribe work consistently across surfaces.
 */

import { browser } from '$app/environment';

export interface TranscribeOptions {
	audio: Blob;
	filename?: string;
	mimeType?: string;
	provider?: string;
	language?: string;
	model?: string;
	prompt?: string;
	messageId?: string;
	/** Optional abort signal so callers can cancel a long-running
	 *  upload (e.g. user clicked Discard mid-transcribe). The fetch
	 *  rejects with an `AbortError` when this fires. */
	signal?: AbortSignal;
}

export interface TranscribeResult {
	transcript: string;
	model: string;
	language?: string | null;
	message_id?: string | null;
	extras?: unknown;
}

/** Backend `SttStreamEvent` mirror. The `text` on a `delta` is the
 *  cumulative transcript so far — callers can assign it directly to
 *  the composer without bookkeeping. `fragment` carries the new
 *  piece for typewriter-style effects. */
export type SttStreamEvent =
	| { type: 'delta'; text: string; fragment?: string }
	| { type: 'final'; transcript: string; model: string; language?: string | null }
	| { type: 'error'; reason: string };

export interface StreamingTranscribeCallbacks {
	onDelta?(text: string, fragment: string | undefined): void;
	onFinal?(result: TranscribeResult): void;
	onError?(reason: string): void;
}

export async function transcribeAudio(opts: TranscribeOptions): Promise<TranscribeResult> {
	if (!browser) throw new Error('transcribeAudio called outside the browser');
	const formData = new FormData();
	const upload = await prepareAudioUpload(opts);
	const file = new File([upload.audio], upload.filename, {
		type: upload.mimeType
	});
	formData.append('file', file);

	const params = new URLSearchParams();
	if (opts.provider && opts.provider !== 'auto') params.set('provider', opts.provider);
	if (opts.language) params.set('language', opts.language);
	if (opts.model) params.set('model', opts.model);
	if (opts.prompt) params.set('prompt', opts.prompt);
	if (opts.messageId) params.set('message_id', opts.messageId);
	const query = params.toString();
	const url = `/api/magician/v2/media/stt/transcribe${query ? `?${query}` : ''}`;

	const response = await fetch(url, {
		method: 'POST',
		body: formData,
		signal: opts.signal
	});
	if (!response.ok) {
		const body = await response.text().catch(() => '');
		throw new Error(`stt ${response.status}: ${body.slice(0, 300)}`);
	}
	return (await response.json()) as TranscribeResult;
}

/**
 * Streaming counterpart to `transcribeAudio`. Posts the same multipart
 * upload to `/media/stt/transcribe/stream` and parses the SSE response
 * — `delta` events fire as OpenAI emits incremental transcripts, the
 * `final` event resolves the returned promise with the canonical
 * result, and `error` events reject. Pass `signal` to abort mid-stream.
 *
 * The first audible win this enables: the composer can show text as
 * it's being transcribed instead of staring at a "Transcribing…"
 * label for 1–3 seconds.
 */
export async function transcribeAudioStreaming(
	opts: TranscribeOptions,
	callbacks: StreamingTranscribeCallbacks
): Promise<TranscribeResult> {
	if (!browser) throw new Error('transcribeAudioStreaming called outside the browser');
	const formData = new FormData();
	const upload = await prepareAudioUpload(opts);
	const file = new File([upload.audio], upload.filename, {
		type: upload.mimeType
	});
	formData.append('file', file);

	const params = new URLSearchParams();
	if (opts.provider && opts.provider !== 'auto') params.set('provider', opts.provider);
	if (opts.language) params.set('language', opts.language);
	if (opts.model) params.set('model', opts.model);
	if (opts.prompt) params.set('prompt', opts.prompt);
	if (opts.messageId) params.set('message_id', opts.messageId);
	const query = params.toString();
	const url = `/api/magician/v2/media/stt/transcribe/stream${query ? `?${query}` : ''}`;

	const response = await fetch(url, {
		method: 'POST',
		body: formData,
		signal: opts.signal
	});
	if (!response.ok) {
		const body = await response.text().catch(() => '');
		throw new Error(`stt-stream ${response.status}: ${body.slice(0, 300)}`);
	}
	if (!response.body) {
		throw new Error('stt-stream returned no body');
	}

	const reader = response.body.getReader();
	const decoder = new TextDecoder();
	let buffer = '';
	let finalResult: TranscribeResult | null = null;
	let aborted = false;

	const consumeEventBlock = (block: string) => {
		// Each SSE event block is `event: stt\ndata: {…}\n` (we
		// strip the trailing blank line by split). We only care
		// about the `data:` line.
		const dataLine = block
			.split('\n')
			.map((line) => line.trim())
			.find((line) => line.startsWith('data:'));
		if (!dataLine) return;
		const payload = dataLine.slice('data:'.length).trim();
		if (!payload || payload === '[DONE]') return;
		let event: SttStreamEvent;
		try {
			event = JSON.parse(payload) as SttStreamEvent;
		} catch {
			// Malformed event — skip rather than tearing the stream down.
			return;
		}
		switch (event.type) {
			case 'delta':
				callbacks.onDelta?.(event.text, event.fragment);
				break;
			case 'final':
				finalResult = {
					transcript: event.transcript,
					model: event.model,
					language: event.language ?? null
				};
				callbacks.onFinal?.(finalResult);
				break;
			case 'error':
				callbacks.onError?.(event.reason);
				throw new Error(event.reason);
		}
	};

	try {
		while (true) {
			const { value, done } = await reader.read();
			if (done) break;
			buffer += decoder.decode(value, { stream: true });
			let separatorIdx = buffer.indexOf('\n\n');
			while (separatorIdx !== -1) {
				const block = buffer.slice(0, separatorIdx);
				buffer = buffer.slice(separatorIdx + 2);
				consumeEventBlock(block);
				separatorIdx = buffer.indexOf('\n\n');
			}
		}
		// Flush any tail content (rare — SSE always ends with `\n\n`).
		if (buffer.trim().length > 0) consumeEventBlock(buffer);
	} catch (error) {
		if (
			error instanceof DOMException
			&& (error.name === 'AbortError' || opts.signal?.aborted)
		) {
			aborted = true;
		} else {
			throw error;
		}
	} finally {
		try {
			reader.releaseLock();
		} catch {
			// no-op
		}
	}

	if (aborted) {
		throw new DOMException('STT stream aborted', 'AbortError');
	}
	if (!finalResult) {
		throw new Error('stt-stream ended without a final event');
	}
	return finalResult;
}

interface PreparedAudioUpload {
	audio: Blob;
	filename: string;
	mimeType: string;
}

export function providerRequiresWavUpload(provider: string | undefined): boolean {
	const normalized = (provider ?? 'auto').trim() || 'auto';
	return normalized === 'auto'
		|| normalized === 'default'
		|| normalized === 'macos_speech'
		|| normalized.startsWith('fluid-');
}

async function prepareAudioUpload(opts: TranscribeOptions): Promise<PreparedAudioUpload> {
	const provider = (opts.provider ?? '').trim();
	const mimeType = opts.mimeType ?? opts.audio.type ?? 'audio/webm';
	const filename = opts.filename ?? 'audio.webm';
	const requiresWav = providerRequiresWavUpload(provider);
	if (!requiresWav || mimeType.includes('wav')) {
		return { audio: opts.audio, filename, mimeType };
	}
	const wav = await convertAudioBlobToWav(opts.audio);
	return {
		audio: wav,
		filename: filename.replace(/\.[^.]+$/, '') + '.wav',
		mimeType: 'audio/wav'
	};
}

async function convertAudioBlobToWav(blob: Blob): Promise<Blob> {
	const Ctor =
		window.AudioContext
		|| (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
	if (!Ctor) {
		throw new Error('AudioContext unavailable; cannot prepare WAV for local speech recognition');
	}
	const context = new Ctor();
	try {
		const buffer = await context.decodeAudioData(await blob.arrayBuffer());
		return new Blob([encodeWav(buffer)], { type: 'audio/wav' });
	} finally {
		try {
			await context.close();
		} catch {
			// no-op
		}
	}
}

function encodeWav(buffer: AudioBuffer): ArrayBuffer {
	const channels = Math.min(buffer.numberOfChannels, 2);
	const sampleRate = buffer.sampleRate;
	const samples = buffer.length;
	const dataBytes = samples * channels * 2;
	const out = new ArrayBuffer(44 + dataBytes);
	const view = new DataView(out);
	writeAscii(view, 0, 'RIFF');
	view.setUint32(4, 36 + dataBytes, true);
	writeAscii(view, 8, 'WAVE');
	writeAscii(view, 12, 'fmt ');
	view.setUint32(16, 16, true);
	view.setUint16(20, 1, true);
	view.setUint16(22, channels, true);
	view.setUint32(24, sampleRate, true);
	view.setUint32(28, sampleRate * channels * 2, true);
	view.setUint16(32, channels * 2, true);
	view.setUint16(34, 16, true);
	writeAscii(view, 36, 'data');
	view.setUint32(40, dataBytes, true);

	const channelData = Array.from({ length: channels }, (_, idx) => buffer.getChannelData(idx));
	let offset = 44;
	for (let i = 0; i < samples; i += 1) {
		for (let ch = 0; ch < channels; ch += 1) {
			const sample = Math.max(-1, Math.min(1, channelData[ch][i] ?? 0));
			view.setInt16(offset, sample < 0 ? sample * 0x8000 : sample * 0x7fff, true);
			offset += 2;
		}
	}
	return out;
}

function writeAscii(view: DataView, offset: number, value: string): void {
	for (let i = 0; i < value.length; i += 1) {
		view.setUint8(offset + i, value.charCodeAt(i));
	}
}
