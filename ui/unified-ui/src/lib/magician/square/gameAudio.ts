export type GameCue = 'select' | 'command' | 'success' | 'warning';

const STORAGE_KEY = 'fleet-game-audio-muted';
let muted: boolean | null = null;
let context: AudioContext | null = null;

function readMuted(): boolean {
	if (muted != null) return muted;
	try {
		muted = window.localStorage.getItem(STORAGE_KEY) === 'true';
	} catch {
		muted = false;
	}
	return muted;
}

export function isGameAudioMuted(): boolean {
	return typeof window === 'undefined' ? true : readMuted();
}

export function setGameAudioMuted(value: boolean): void {
	muted = value;
	try {
		window.localStorage.setItem(STORAGE_KEY, String(value));
	} catch {
		// Storage is optional; the in-memory setting still applies.
	}
}

export function playGameCue(cue: GameCue): void {
	if (typeof window === 'undefined' || readMuted()) return;
	const AudioContextClass = window.AudioContext;
	if (!AudioContextClass) return;
	context ??= new AudioContextClass();
	void context.resume();
	const now = context.currentTime;
	const oscillator = context.createOscillator();
	const gain = context.createGain();
	const tones: Record<GameCue, { start: number; end: number; duration: number; volume: number }> = {
		select: { start: 330, end: 390, duration: 0.055, volume: 0.012 },
		command: { start: 280, end: 440, duration: 0.09, volume: 0.016 },
		success: { start: 430, end: 660, duration: 0.12, volume: 0.016 },
		warning: { start: 240, end: 190, duration: 0.11, volume: 0.014 }
	};
	const tone = tones[cue];
	oscillator.type = cue === 'warning' ? 'square' : 'sine';
	oscillator.frequency.setValueAtTime(tone.start, now);
	oscillator.frequency.exponentialRampToValueAtTime(tone.end, now + tone.duration);
	gain.gain.setValueAtTime(0.0001, now);
	gain.gain.exponentialRampToValueAtTime(tone.volume, now + 0.008);
	gain.gain.exponentialRampToValueAtTime(0.0001, now + tone.duration);
	oscillator.connect(gain);
	gain.connect(context.destination);
	oscillator.start(now);
	oscillator.stop(now + tone.duration + 0.01);
}
