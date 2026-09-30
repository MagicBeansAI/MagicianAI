// @vitest-environment jsdom

import { beforeEach, describe, expect, it, vi } from 'vitest';

const tts = vi.hoisted(() => ({
	speak: vi.fn(),
	providerSpeakBlocks: vi.fn(async () => undefined)
}));

vi.mock('$app/environment', () => ({
	browser: true,
	dev: false,
	building: false,
	version: 'test'
}));

vi.mock('$lib/media/tts/browserTts', () => ({
	isBrowserTtsAvailable: () => true,
	speak: tts.speak
}));

vi.mock('$lib/media/tts/providerTts', () => ({
	providerSpeakBlocks: tts.providerSpeakBlocks
}));

import { mediaProvidersStore } from '$lib/media/providers';
import { voiceCallStore } from '$lib/media/voice/realtimeVoiceClient';
import { ttsStore } from './store';
import {
	maybeSpeakTaskCompletion,
	resetTaskCompletionSpeechDedupeForTests
} from './taskCompletionSpeech';

beforeEach(() => {
	vi.clearAllMocks();
	resetTaskCompletionSpeechDedupeForTests();
	mediaProvidersStore.reset();
	ttsStore.reset();
	ttsStore.hydratePrefs({ autoSpeak: true });
	ttsStore.markUserInteracted();
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

describe('task completion speech', () => {
	it('speaks one terminal completion per task execution', () => {
		const content = {
			type: 'task_status_update',
			status: 'completed',
			task_id: 'task-1',
			execution_id: 'exec-1',
			speech_tts: 'The task is complete.'
		};

		maybeSpeakTaskCompletion({ id: 'message-1', content });
		maybeSpeakTaskCompletion({ id: 'message-2', content });

		expect(tts.speak).toHaveBeenCalledOnce();
	});

	it('allows a later execution of the same task to speak', () => {
		const base = {
			type: 'task_status_update',
			status: 'completed',
			task_id: 'task-1',
			speech_tts: 'The task is complete.'
		};

		maybeSpeakTaskCompletion({ id: 'message-1', content: { ...base, execution_id: 'exec-1' } });
		maybeSpeakTaskCompletion({ id: 'message-2', content: { ...base, execution_id: 'exec-2' } });

		expect(tts.speak).toHaveBeenCalledTimes(2);
	});
});
