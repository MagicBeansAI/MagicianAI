import { browser } from '$app/environment';
import { writable } from 'svelte/store';

/** Meeting attendee default. The same id the server uses when a join omits one. */
export const MEET_REALTIME_PROFILE_DEFAULT = 'voice_realtime_openai_backend';

const STORAGE_KEY = 'magician.meet_realtime_profile';

function storedProfile(): string {
	if (!browser) return MEET_REALTIME_PROFILE_DEFAULT;
	const stored = window.localStorage.getItem(STORAGE_KEY)?.trim();
	return stored || MEET_REALTIME_PROFILE_DEFAULT;
}

function createMeetVoiceProfileStore() {
	const { subscribe, set } = writable(storedProfile());
	return {
		subscribe,
		set(value: string): void {
			const normalized = value.trim() || MEET_REALTIME_PROFILE_DEFAULT;
			if (browser) window.localStorage.setItem(STORAGE_KEY, normalized);
			set(normalized);
		}
	};
}

/** Engine for the next Google Meet join. Shared by Observe and Settings. */
export const meetVoiceProfileStore = createMeetVoiceProfileStore();
