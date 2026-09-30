import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

const tts = vi.hoisted(() => ({
	speak: vi.fn(),
	providerSpeakBlocks: vi.fn(async () => undefined)
}));

vi.mock('$lib/media/tts/browserTts', () => ({
	isBrowserTtsAvailable: () => true,
	speak: tts.speak,
	cancelCurrent: vi.fn()
}));

vi.mock('$lib/media/tts/providerTts', () => ({
	providerSpeakBlocks: tts.providerSpeakBlocks,
	cancelProviderSpeak: vi.fn()
}));

import { voiceCallStore } from '$lib/media/voice/realtimeVoiceClient';
import { ttsStore } from './store';
import { maybeAutoSpeakTail } from './autoSpeak';

const assistantMessage = {
	id: 'assistant-1',
	direction: 'assistant' as const,
	content: 'The answer is ready.',
	voice_origin: true
};

function evaluate() {
	return maybeAutoSpeakTail({
		tailId: assistantMessage.id,
		lastAutoSpokenMessageId: null,
		resolveMessage: () => assistantMessage,
		getMessageText: (content) => String(content)
	});
}

function evaluateMessage(message: typeof assistantMessage & { source_surface?: string }) {
	return maybeAutoSpeakTail({
		tailId: message.id,
		lastAutoSpokenMessageId: null,
		resolveMessage: () => message,
		getMessageText: (content) => String(content)
	});
}

beforeEach(() => {
	vi.clearAllMocks();
	ttsStore.reset();
	voiceCallStore.set({
		state: 'idle',
		error: null,
		model: null,
		voice: null,
		connectedAt: null,
		activationPhrase: null,
		activationPhrases: [],
		addressingRequired: false,
		boundary: null
	});
});

describe('chat auto-speak audio focus', () => {
	it('does not replay a voice-origin reply while live voice owns playback', () => {
		ttsStore.hydratePrefs({ autoSpeak: true });
		voiceCallStore.set({
			state: 'connected',
			error: null,
			model: 'hands-free-local-fluid-v1',
			voice: null,
			connectedAt: Date.now(),
			activationPhrase: null,
			activationPhrases: [],
			addressingRequired: false,
			boundary: null
		});

		expect(evaluate()).toEqual({ nextLastAutoSpokenMessageId: null });
		expect(tts.speak).not.toHaveBeenCalled();
		expect(tts.providerSpeakBlocks).not.toHaveBeenCalled();
		// Voice focus is transient. It must never overwrite the preference
		// that becomes effective again when the call reaches a terminal state.
		expect(get(ttsStore).prefs.autoSpeak).toBe(true);
	});

	it('still speaks a voice-origin reply when no live call owns audio', () => {
		expect(evaluate()).toEqual({ nextLastAutoSpokenMessageId: assistantMessage.id });
		expect(tts.speak).toHaveBeenCalledOnce();
	});

	it.each(['tutor', 'personal_tutor', 'personal_tutor_background', 'ios_tutor_overlay', 'app_copilot'])(
		'does not duplicate %s narration through the mirrored chat reply',
		(sourceSurface) => {
			const message = { ...assistantMessage, source_surface: sourceSurface };

			expect(evaluateMessage(message)).toEqual({ nextLastAutoSpokenMessageId: message.id });
			expect(tts.speak).not.toHaveBeenCalled();
			expect(tts.providerSpeakBlocks).not.toHaveBeenCalled();
		}
	);
});
