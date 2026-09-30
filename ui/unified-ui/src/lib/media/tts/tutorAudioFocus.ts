/**
 * Temporary audio focus for Personal Tutor narration.
 *
 * This is not the user's persistent mute/auto-speak setting. While active it
 * suppresses normal chat/task TTS and asks the Tauri host to mute live/PTT
 * assistant playback only until the tutor narration releases focus.
 */

import { get, writable } from 'svelte/store';

const active = writable(false);
let depth = 0;

export const tutorAudioFocusStore = {
	subscribe: active.subscribe
};

export function isTutorAudioFocusActive(): boolean {
	return get(active);
}

export async function acquireTutorAudioFocus(): Promise<() => Promise<void>> {
	depth += 1;
	if (depth === 1) {
		active.set(true);
		await setDesktopTutorAudioFocus(true);
	}
	let released = false;
	return async () => {
		if (released) return;
		released = true;
		depth = Math.max(0, depth - 1);
		if (depth === 0) {
			active.set(false);
			await setDesktopTutorAudioFocus(false);
		}
	};
}

async function setDesktopTutorAudioFocus(enabled: boolean): Promise<void> {
	if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
	try {
		const { invoke } = await import('@tauri-apps/api/core');
		await invoke('set_tutor_audio_focus', { active: enabled });
	} catch (error) {
		console.warn('[tutor-audio-focus] set_tutor_audio_focus failed:', error);
	}
}
