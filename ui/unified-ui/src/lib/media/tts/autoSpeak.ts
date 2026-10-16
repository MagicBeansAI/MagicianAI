/**
 * Shared TTS auto-speak orchestration for chat-like surfaces.
 *
 * Both `/chat` and `/t/[name]` need the same behaviour:
 *
 *  1. Watch the tail of the visible message list.
 *  2. When a new assistant message lands, decide whether to speak
 *     it aloud (always for voice-originated turns; opt-in via
 *     `autoSpeak` for typed turns).
 *  3. Prefer backend-parsed `speech_segments` over re-parsing the
 *     raw body.
 *  4. Pick provider TTS when a backend provider is registered;
 *     fall back to browser TTS otherwise.
 *
 * This module owns the decision logic; the page only supplies a
 * way to read the current tail and the current message list.
 *
 * Why a function and not a Svelte store: the decision depends on
 * page-specific state (`displayMessages`, `visibleMessages`) and on
 * the global `ttsStore` / `mediaProvidersStore` stores. Wrapping
 * everything in another store would entangle them. A pure function
 * `evaluateAutoSpeak(...)` keeps Svelte reactivity at the page
 * level — the page calls this once per tail change, and the
 * function returns the decision the page acts on.
 */

import { get } from 'svelte/store';
import { isBrowserTtsAvailable, speak as speakBrowserTts } from './browserTts';
import { providerSpeakBlocks } from './providerTts';
import { resolveTtsProviderChoice } from './providerChoices';
import { resolveSpeechBlocks, type SpeechBlock } from './speechTags';
import { mediaProvidersStore } from '../providers';
import { isVoiceCallCaptureState, voiceCallStore } from '../voice/realtimeVoiceClient';
import { ttsStore } from './store';
import { isTutorAudioFocusActive } from './tutorAudioFocus';

export interface AutoSpeakMessageLike {
	id: string;
	direction: 'user' | 'assistant' | 'system';
	content: unknown;
	source_surface?: string;
	voice_origin?: boolean;
	speech_segments?: SpeechBlock[];
}

export interface AutoSpeakOptions {
	/** The freshly-arrived tail message id. */
	tailId: string | null;
	/** Last id we already auto-spoke — guards against double-fire. */
	lastAutoSpokenMessageId: string | null;
	/**
	 * Lookup the message by id. Pages typically pass
	 * `(id) => visibleMessages.find(v => v.id === id)?.message`. The
	 * returned shape must include `direction`, `content`, optional
	 * `source_surface`, optional `voice_origin`, and optional
	 * `speech_segments`.
	 */
	resolveMessage: (id: string) => AutoSpeakMessageLike | null | undefined;
	/**
	 * Extract spoken-aloud text from `content`. Pages pass their
	 * existing `getMessageText` helper — the type of `content` is
	 * page-specific so we don't unify it here.
	 */
	getMessageText: (content: unknown) => string | null | undefined;
}

export interface AutoSpeakDecision {
	/** New value to assign to the page's `lastAutoSpokenMessageId` */
	nextLastAutoSpokenMessageId: string | null;
}

/**
 * Tutor/App Copilot own narration for their visual timeline. Their assistant
 * reply is still mirrored into the chat ledger, but reading that mirror through
 * normal chat auto-speak would produce a second voice beside the overlay.
 */
function isTutorNarratedSurface(sourceSurface: string | undefined): boolean {
	switch (sourceSurface?.trim().toLowerCase()) {
		case 'tutor':
		case 'personal_tutor':
		case 'personal_tutor_background':
		case 'ios_tutor_overlay':
		case 'app_copilot':
			return true;
		default:
			return false;
	}
}

/**
 * Inspect the freshly-arrived tail message and kick off auto-speak
 * if the rules say to. Returns the new `lastAutoSpokenMessageId`
 * the page should assign — the page owns the variable, this
 * function only computes the next value.
 *
 * Safe to call on every tail change; it no-ops when:
 *  - no browser (SSR),
 *  - `tailId` is null or unchanged,
 *  - tail is a streaming placeholder,
 *  - tail message isn't an assistant message,
 *  - no TTS providers (backend or browser) are available,
 *  - the user hasn't opted into autoSpeak AND the turn isn't
 *    voice-originated.
 */
export function maybeAutoSpeakTail(opts: AutoSpeakOptions): AutoSpeakDecision {
	const unchanged: AutoSpeakDecision = {
		nextLastAutoSpokenMessageId: opts.lastAutoSpokenMessageId
	};
	if (typeof window === 'undefined') return unchanged;
	if (!opts.tailId) return unchanged;
	if (opts.tailId === opts.lastAutoSpokenMessageId) return unchanged;
	if (opts.tailId.startsWith('streaming-')) return unchanged;
	const message = opts.resolveMessage(opts.tailId);
	if (!message || message.direction !== 'assistant') return unchanged;
	if (isTutorNarratedSurface(message.source_surface)) {
		// Consume the mirrored reply before checking transient focus/provider
		// state. It must never become eligible for delayed chat narration.
		return { nextLastAutoSpokenMessageId: opts.tailId };
	}
	if (isTutorAudioFocusActive()) return unchanged;
	// Realtime/hands-free owns playback while its session is live. The
	// same assistant turn is also persisted into chat with voice_origin;
	// speaking that tail again here would produce a second, overlapping voice.
	if (isVoiceCallCaptureState(get(voiceCallStore).state)) return unchanged;
	const state = get(ttsStore);
	const providers = get(mediaProvidersStore);
	const browserOk = isBrowserTtsAvailable();
	const ttsSelection = resolveTtsProviderChoice(providers, browserOk);
	if (ttsSelection.mode === 'none') return unchanged;

	// Voice-origin is server-stamped on the message envelope.
	const isVoiceTurn = message.voice_origin === true;
	if (!isVoiceTurn && (!state.prefs.autoSpeak || !state.userInteracted)) {
		return unchanged;
	}

	const raw = opts.getMessageText(message.content) ?? '';
	const blocks = resolveSpeechBlocks(message.speech_segments, raw);
	const fallbackText = blocks.map((b) => b.text).join(' ');
	if (!fallbackText) return unchanged;

	ttsStore.setActive(message.id);
	const onEnd = (status: 'completed' | 'cancelled' | 'error') => {
		if (status !== 'cancelled') {
			ttsStore.setActive(null);
		}
	};
	if (ttsSelection.mode === 'backend') {
		void providerSpeakBlocks(
			{
				messageId: message.id,
				blocks,
				provider: null,
				voice: null,
				model: null,
				rate: state.prefs.rate
			},
			onEnd
		);
	} else {
		speakBrowserTts(
			{
				messageId: message.id,
				text: fallbackText,
				voiceName: state.prefs.voiceName,
				rate: state.prefs.rate,
				pitch: state.prefs.pitch
			},
			onEnd
		);
	}

	return { nextLastAutoSpokenMessageId: opts.tailId };
}
