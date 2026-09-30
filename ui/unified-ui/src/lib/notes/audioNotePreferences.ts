import { browser } from '$app/environment';
import { writable } from 'svelte/store';

export const ARCHIVE_CHAT_DICTATION_KEY = 'magician.voice.archiveChatDictation';

function readInitialPreference(): boolean {
	if (!browser) return false;
	try {
		return localStorage.getItem(ARCHIVE_CHAT_DICTATION_KEY) === 'true';
	} catch {
		return false;
	}
}

export const archiveChatDictationStore = writable(readInitialPreference());

export function setArchiveChatDictation(enabled: boolean): void {
	archiveChatDictationStore.set(enabled);
	if (!browser) return;
	try {
		localStorage.setItem(ARCHIVE_CHAT_DICTATION_KEY, enabled ? 'true' : 'false');
	} catch {
		// The in-memory choice remains valid when private storage is unavailable.
	}
}
