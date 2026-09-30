/**
 * Thin wrapper around `window.speechSynthesis` for browser-native TTS.
 *
 * Phase 1 only — provider TTS comes online in Phase 3 via the same
 * `TtsProvider` shape the design doc sketches. This module owns the
 * actual `SpeechSynthesisUtterance` lifecycle and emits the canonical
 * `media.tts.*` events through the realtime session so backend
 * subscribers see the same lifecycle whatever the surface.
 *
 * Browser quirks worth knowing:
 *
 * 1. Voices populate asynchronously on Chromium. `getVoices()` returns
 *    `[]` until `voiceschanged` fires. We re-read on every play and
 *    cache the last good list as a fallback.
 * 2. iOS Safari requires a user gesture for the first `speak()` call.
 *    The SpeakButton handler always runs inside a click, so this is
 *    fine — but auto-speak only works AFTER the user has clicked
 *    speak at least once. The TTS store gates auto-speak on a "user
 *    has interacted" flag for that reason.
 * 3. `speechSynthesis.speak()` is fire-and-forget. We track the active
 *    utterance ourselves so `cancel()` knows what it's killing.
 */

import { browser } from '$app/environment';

import { publishMediaEvent } from '$lib/media/session';
import {
	MEDIA_TTS_CANCELLED,
	MEDIA_TTS_COMPLETED,
	MEDIA_TTS_ERROR,
	MEDIA_TTS_STARTED
} from '$lib/media/types';

export interface SpeakOptions {
	messageId: string;
	text: string;
	voiceName?: string | null;
	rate?: number;
	pitch?: number;
	volume?: number;
	onStart?: () => void;
}

export interface ActiveUtterance {
	messageId: string;
	startedAt: number;
	utterance: SpeechSynthesisUtterance;
}

export interface VoiceDescriptor {
	name: string;
	lang: string;
	default: boolean;
	localService: boolean;
}

let activeUtterance: ActiveUtterance | null = null;
let cachedVoices: SpeechSynthesisVoice[] = [];
let voicesChangedHooked = false;

export function isBrowserTtsAvailable(): boolean {
	return browser && typeof window.speechSynthesis !== 'undefined';
}

function ensureVoicesHook(): void {
	if (!isBrowserTtsAvailable() || voicesChangedHooked) return;
	const refresh = () => {
		const list = window.speechSynthesis.getVoices();
		if (list.length > 0) cachedVoices = list;
	};
	refresh();
	window.speechSynthesis.addEventListener('voiceschanged', refresh);
	voicesChangedHooked = true;
}

export function listVoices(): VoiceDescriptor[] {
	if (!isBrowserTtsAvailable()) return [];
	ensureVoicesHook();
	const live = window.speechSynthesis.getVoices();
	const source = live.length > 0 ? live : cachedVoices;
	return source.map((v) => ({
		name: v.name,
		lang: v.lang,
		default: v.default,
		localService: v.localService
	}));
}

export function getActiveMessageId(): string | null {
	return activeUtterance?.messageId ?? null;
}

export function cancelCurrent(reason: 'user' | 'replaced' = 'user'): void {
	if (!isBrowserTtsAvailable()) return;
	const prior = activeUtterance;
	try {
		window.speechSynthesis.cancel();
	} catch {
		// no-op
	}
	if (prior) {
		void publishMediaEvent(MEDIA_TTS_CANCELLED, {
			message_id: prior.messageId,
			reason,
			duration_ms: Date.now() - prior.startedAt
		});
		activeUtterance = null;
	}
}

export function speak(opts: SpeakOptions, onEnd?: (status: 'completed' | 'cancelled' | 'error') => void): void {
	if (!isBrowserTtsAvailable()) {
		void publishMediaEvent(MEDIA_TTS_ERROR, {
			message_id: opts.messageId,
			reason: 'speech_synthesis_unavailable'
		});
		onEnd?.('error');
		return;
	}
	ensureVoicesHook();
	cancelCurrent('replaced');

	const utterance = new SpeechSynthesisUtterance(opts.text);
	utterance.rate = opts.rate ?? 1;
	utterance.pitch = opts.pitch ?? 1;
	utterance.volume = opts.volume ?? 1;
	if (opts.voiceName) {
		const match = listVoices().find((v) => v.name === opts.voiceName);
		if (match) {
			// `SpeechSynthesisUtterance.voice` expects the original
			// `SpeechSynthesisVoice` object, not our serialisable view.
			const live = window.speechSynthesis
				.getVoices()
				.find((v) => v.name === opts.voiceName);
			if (live) utterance.voice = live;
			utterance.lang = match.lang;
		}
	}

	const startedAt = Date.now();
	activeUtterance = { messageId: opts.messageId, startedAt, utterance };

	utterance.onstart = () => {
		void publishMediaEvent(MEDIA_TTS_STARTED, {
			message_id: opts.messageId,
			voice: opts.voiceName ?? null,
			text_length: opts.text.length,
			rate: utterance.rate,
			pitch: utterance.pitch
		});
		opts.onStart?.();
	};
	utterance.onend = () => {
		// `onend` fires for both natural completion AND `cancel()`.
		// We disambiguate by checking whether `activeUtterance` still
		// points at this one — `cancel()` clears it first.
		const isStillActive = activeUtterance?.utterance === utterance;
		if (!isStillActive) {
			onEnd?.('cancelled');
			return;
		}
		void publishMediaEvent(MEDIA_TTS_COMPLETED, {
			message_id: opts.messageId,
			duration_ms: Date.now() - startedAt
		});
		activeUtterance = null;
		onEnd?.('completed');
	};
	utterance.onerror = (event: SpeechSynthesisErrorEvent) => {
		const stillActive = activeUtterance?.utterance === utterance;
		// Browsers fire `onerror` with `error === 'interrupted'`
		// whenever we cancel mid-speak. Surface real errors only.
		if (event.error === 'interrupted' || event.error === 'canceled') {
			activeUtterance = null;
			onEnd?.('cancelled');
			return;
		}
		void publishMediaEvent(MEDIA_TTS_ERROR, {
			message_id: opts.messageId,
			reason: event.error || 'unknown'
		});
		if (stillActive) activeUtterance = null;
		onEnd?.('error');
	};
	try {
		window.speechSynthesis.speak(utterance);
	} catch (error) {
		void publishMediaEvent(MEDIA_TTS_ERROR, {
			message_id: opts.messageId,
			reason: error instanceof Error ? error.message : 'speak_threw'
		});
		activeUtterance = null;
		onEnd?.('error');
	}
}
