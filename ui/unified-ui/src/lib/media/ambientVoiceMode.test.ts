import { describe, expect, it } from 'vitest';

import {
	AMBIENT_DICTATION_MAX_UTTERANCE_MS,
	advanceAmbientDictationGate,
	loadAmbientVoiceMode,
	normalizeAmbientVoiceMode,
	persistAmbientVoiceMode,
	seedAmbientVoiceMode,
	type AmbientVoiceModeStorage
} from './ambientVoiceMode';

function memoryStorage(initial: Record<string, string> = {}): AmbientVoiceModeStorage {
	const values = new Map(Object.entries(initial));
	return {
		getItem: (key) => values.get(key) ?? null,
		setItem: (key, value) => values.set(key, value)
	};
}

describe('ambient voice mode', () => {
	it('normalizes all backend and legacy values into the three local modes', () => {
		expect(normalizeAmbientVoiceMode(' recording ')).toBe('dictation');
		expect(normalizeAmbientVoiceMode('DICTATE')).toBe('dictation');
		expect(normalizeAmbientVoiceMode('hands_free')).toBe('hands_free');
		expect(normalizeAmbientVoiceMode(' LIVE ')).toBe('realtime');
		expect(normalizeAmbientVoiceMode('unknown')).toBe('hands_free');
	});

	it('seeds once from backend and preserves the browser-local selection', () => {
		const storage = memoryStorage();
		expect(seedAmbientVoiceMode(storage, 'recording')).toBe('dictation');
		persistAmbientVoiceMode(storage, 'realtime');
		expect(seedAmbientVoiceMode(storage, 'hands_free')).toBe('realtime');
		expect(loadAmbientVoiceMode(storage)).toBe('realtime');
	});

	it('treats choosing the displayed default as an explicit local choice', () => {
		const storage = memoryStorage();
		persistAmbientVoiceMode(storage, 'hands_free');
		expect(seedAmbientVoiceMode(storage, 'recording')).toBe('hands_free');
	});

	it('ends dictation on trailing silence, no speech, and the utterance cap', () => {
		const trailing = { heardSpeech: false, lastSpeechAtMs: null };
		expect(advanceAmbientDictationGate(trailing, 0.02, 1_000)).toBeNull();
		expect(advanceAmbientDictationGate(trailing, 0, 2_099)).toBeNull();
		expect(advanceAmbientDictationGate(trailing, 0, 2_100)).toBe('speech_complete');

		const quiet = { heardSpeech: false, lastSpeechAtMs: null };
		expect(advanceAmbientDictationGate(quiet, 0, 7_999)).toBeNull();
		expect(advanceAmbientDictationGate(quiet, 0, 8_000)).toBe('no_speech');

		const capped = { heardSpeech: false, lastSpeechAtMs: null };
		expect(advanceAmbientDictationGate(capped, 0.02, 1_000)).toBeNull();
		expect(
			advanceAmbientDictationGate(capped, 0.02, AMBIENT_DICTATION_MAX_UTTERANCE_MS)
		).toBe('speech_complete');
	});
});
