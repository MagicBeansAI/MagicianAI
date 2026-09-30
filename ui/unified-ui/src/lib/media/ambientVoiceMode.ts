export type AmbientVoiceMode = 'dictation' | 'hands_free' | 'realtime';

export interface AmbientVoiceModeStorage {
	getItem(key: string): string | null;
	setItem(key: string, value: string): void;
}

export const AMBIENT_VOICE_MODE_STORAGE_KEY = 'magician.voice.ambientMode';

export const AMBIENT_DICTATION_POLL_MS = 100;
export const AMBIENT_DICTATION_TRAILING_SILENCE_MS = 1_100;
export const AMBIENT_DICTATION_NO_SPEECH_MS = 8_000;
export const AMBIENT_DICTATION_MAX_UTTERANCE_MS = 45_000;
export const AMBIENT_DICTATION_SPEECH_LEVEL = 0.012;

export type AmbientDictationBoundary = 'speech_complete' | 'no_speech' | null;

export interface AmbientDictationGateState {
	heardSpeech: boolean;
	lastSpeechAtMs: number | null;
}

export function normalizeAmbientVoiceMode(value: unknown): AmbientVoiceMode {
	if (typeof value !== 'string') return 'hands_free';
	switch (value.trim().toLowerCase()) {
		case 'realtime':
		case 'live':
			return 'realtime';
		case 'recording':
		case 'dictation':
		case 'dictate':
			return 'dictation';
		default:
			return 'hands_free';
	}
}

export function loadAmbientVoiceMode(
	storage: AmbientVoiceModeStorage | null
): AmbientVoiceMode {
	if (!storage) return 'hands_free';
	try {
		return normalizeAmbientVoiceMode(storage.getItem(AMBIENT_VOICE_MODE_STORAGE_KEY));
	} catch {
		return 'hands_free';
	}
}

/**
 * Adopt the backend's shared voice mode only when this browser has never made
 * an Ambient Orb choice. The key's presence is the seed marker, matching the
 * iOS UserDefaults contract: later backend updates cannot replace a local
 * Dictation, Hands-free, or Live selection.
 */
export function seedAmbientVoiceMode(
	storage: AmbientVoiceModeStorage | null,
	backendVoiceMode: unknown
): AmbientVoiceMode {
	if (!storage) return normalizeAmbientVoiceMode(backendVoiceMode);
	try {
		const stored = storage.getItem(AMBIENT_VOICE_MODE_STORAGE_KEY);
		if (stored !== null) return normalizeAmbientVoiceMode(stored);
		const seeded = normalizeAmbientVoiceMode(backendVoiceMode);
		storage.setItem(AMBIENT_VOICE_MODE_STORAGE_KEY, seeded);
		return seeded;
	} catch {
		return normalizeAmbientVoiceMode(backendVoiceMode);
	}
}

export function persistAmbientVoiceMode(
	storage: AmbientVoiceModeStorage | null,
	mode: AmbientVoiceMode
): void {
	if (!storage) return;
	try {
		storage.setItem(AMBIENT_VOICE_MODE_STORAGE_KEY, mode);
	} catch {
		// Storage can be disabled or full. The in-memory selection still works.
	}
}

export function advanceAmbientDictationGate(
	state: AmbientDictationGateState,
	level: number,
	elapsedMs: number,
	noSpeechAfterMs = AMBIENT_DICTATION_NO_SPEECH_MS
): AmbientDictationBoundary {
	if (Number.isFinite(level) && level >= AMBIENT_DICTATION_SPEECH_LEVEL) {
		state.heardSpeech = true;
		state.lastSpeechAtMs = elapsedMs;
	}
	if (!state.heardSpeech) {
		return elapsedMs >= noSpeechAfterMs ? 'no_speech' : null;
	}
	if (elapsedMs >= AMBIENT_DICTATION_MAX_UTTERANCE_MS) return 'speech_complete';
	if (
		state.lastSpeechAtMs !== null
		&& elapsedMs - state.lastSpeechAtMs >= AMBIENT_DICTATION_TRAILING_SILENCE_MS
	) {
		return 'speech_complete';
	}
	return null;
}
