/**
 * User-facing TTS preferences + "currently speaking" state.
 *
 * Two concerns in one store so the SpeakButton can read both without
 * subscribing twice:
 *
 *   * `prefs` — backend-hydrated auto-speak plus device-local browser voice,
 *     rate, and pitch.
 *   * `activeMessageId` — non-persisted; the message id being spoken
 *     right now. Drives the button's play/stop affordance.
 *
 * Auto-speak only kicks in after the user has interacted with the page
 * once (clicking the speak button counts) because iOS Safari blocks
 * `speechSynthesis.speak()` without a gesture. We track that flag here
 * so the chat page's auto-speak hook can no-op until the first click.
 */

import { writable } from 'svelte/store';

export interface TtsPrefs {
	autoSpeak: boolean;
	voiceName: string | null;
	rate: number;
	pitch: number;
}

export interface TtsState {
	prefs: TtsPrefs;
	activeMessageId: string | null;
	userInteracted: boolean;
}

const DEFAULT_PREFS: TtsPrefs = {
	autoSpeak: false,
	voiceName: null,
	rate: 1,
	pitch: 1
};

function normalizePrefs(prefs: Partial<TtsPrefs>): TtsPrefs {
	return {
		autoSpeak: typeof prefs.autoSpeak === 'boolean' ? prefs.autoSpeak : DEFAULT_PREFS.autoSpeak,
		voiceName: typeof prefs.voiceName === 'string' ? prefs.voiceName : null,
		rate: typeof prefs.rate === 'number' && prefs.rate > 0 ? prefs.rate : DEFAULT_PREFS.rate,
		pitch: typeof prefs.pitch === 'number' && prefs.pitch > 0 ? prefs.pitch : DEFAULT_PREFS.pitch
	};
}

// Browser voice / rate / pitch are device-specific — the OS voices available
// differ per device — so they persist to localStorage rather than the
// backend-synced media preferences. autoSpeak stays backend-hydrated; provider
// and model resolution belong to the configured Dictation profile.
const VOICE_PREFS_KEY = 'magican-tts-voice-prefs';

function loadLocalVoicePrefs(): Partial<TtsPrefs> {
	if (typeof localStorage === 'undefined') return {};
	try {
		const raw = localStorage.getItem(VOICE_PREFS_KEY);
		if (!raw) return {};
		const parsed = JSON.parse(raw) as Partial<TtsPrefs>;
		const out: Partial<TtsPrefs> = {};
		if (typeof parsed.voiceName === 'string') out.voiceName = parsed.voiceName;
		if (typeof parsed.rate === 'number') out.rate = parsed.rate;
		if (typeof parsed.pitch === 'number') out.pitch = parsed.pitch;
		return out;
	} catch {
		return {};
	}
}

function persistLocalVoicePrefs(prefs: TtsPrefs): void {
	if (typeof localStorage === 'undefined') return;
	try {
		localStorage.setItem(
			VOICE_PREFS_KEY,
			JSON.stringify({ voiceName: prefs.voiceName, rate: prefs.rate, pitch: prefs.pitch })
		);
	} catch {
		/* private mode / quota exceeded — non-fatal, prefs stay in-memory */
	}
}

function createStore() {
	const { subscribe, update, set } = writable<TtsState>({
		prefs: normalizePrefs({ ...DEFAULT_PREFS, ...loadLocalVoicePrefs() }),
		activeMessageId: null,
		userInteracted: false
	});

	return {
		subscribe,
		setActive(messageId: string | null): void {
			update((state) => ({ ...state, activeMessageId: messageId }));
		},
		markUserInteracted(): void {
			update((state) => (state.userInteracted ? state : { ...state, userInteracted: true }));
		},
		setAutoSpeak(autoSpeak: boolean): void {
			update((state) => {
				const next = { ...state.prefs, autoSpeak };
				return { ...state, prefs: next };
			});
		},
		setVoice(voiceName: string | null): void {
			update((state) => {
				const next = { ...state.prefs, voiceName };
				persistLocalVoicePrefs(next);
				return { ...state, prefs: next };
			});
		},
		setRate(rate: number): void {
			update((state) => {
				const next = { ...state.prefs, rate };
				persistLocalVoicePrefs(next);
				return { ...state, prefs: next };
			});
		},
		setPitch(pitch: number): void {
			update((state) => {
				const next = { ...state.prefs, pitch };
				persistLocalVoicePrefs(next);
				return { ...state, prefs: next };
			});
		},
		hydratePrefs(prefs: Partial<TtsPrefs>): void {
			update((state) => ({
				...state,
				prefs: normalizePrefs({
					...state.prefs,
					...prefs
				})
			}));
		},
		reset(): void {
			set({
				prefs: { ...DEFAULT_PREFS },
				activeMessageId: null,
				userInteracted: false
			});
		}
	};
}

export const ttsStore = createStore();
