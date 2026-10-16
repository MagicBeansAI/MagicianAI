/**
 * Svelte store wrapper around the active realtime media session.
 *
 * Consumers (TTS button, capture buttons, settings UI) subscribe here
 * to know whether a session is registered, what capabilities it claims,
 * and which permissions are currently granted. Mutations always flow
 * through `session.ts` — never write to the store directly from a
 * component.
 */

import { writable } from 'svelte/store';
import type { RealtimeSession } from './types';

export interface MediaSessionState {
	session: RealtimeSession | null;
	error: string | null;
}

function createStore() {
	const { subscribe, set, update } = writable<MediaSessionState>({
		session: null,
		error: null
	});

	return {
		subscribe,
		set(session: RealtimeSession): void {
			set({ session, error: null });
		},
		setError(error: string): void {
			update((state) => ({ ...state, error }));
		},
		clear(): void {
			set({ session: null, error: null });
		}
	};
}

export const mediaSessionStore = createStore();
