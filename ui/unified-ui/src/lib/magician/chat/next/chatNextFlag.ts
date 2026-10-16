/**
 * `chatNext` — the flag that switches chat to the "lively" presentation layer
 * in `chat/next/`.
 *
 * Resolution order (first match wins):
 * 1. URL `?chatNext=1|0` — read once at module init and persisted, so a shared
 *    link flips the flag for this browser from then on.
 * 2. `localStorage['magican-chat-next']` = `'on' | 'off'` — the viewer's choice.
 * 3. Default — on, in every theme. Every theme inherits the liveliness
 *    tokens' :root fallbacks, so the design renders everywhere; the flag
 *    stays as an escape hatch (`?chatNext=0`).
 *
 * Unrecognised URL or stored values are ignored and fall through.
 */
import { derived, writable } from 'svelte/store';
import { browser } from '$app/environment';


const CHAT_NEXT_STORAGE_KEY = 'magican-chat-next';
const CHAT_NEXT_URL_PARAM = 'chatNext';

export function resolveChatNext(i: { stored?: string | null; url?: string | null }): boolean {
	if (i.url === '1') return true;
	if (i.url === '0') return false;
	if (i.stored === 'on') return true;
	if (i.stored === 'off') return false;
	return true;
}

function readStored(): string | null {
	if (!browser) return null;
	try {
		return window.localStorage.getItem(CHAT_NEXT_STORAGE_KEY);
	} catch {
		/* localStorage unavailable — the default still answers */
		return null;
	}
}

const stored = writable<string | null>(readStored());

/** Remember an explicit on/off choice for this browser. */
export function setChatNext(on: boolean): void {
	const value = on ? 'on' : 'off';
	stored.set(value);
	if (!browser) return;
	try {
		window.localStorage.setItem(CHAT_NEXT_STORAGE_KEY, value);
	} catch {
		/* localStorage unavailable — the choice holds for this visit only */
	}
}

// The URL override is settled once, before anything derives from `stored`,
// so persisting it never re-enters the derived store below.
if (browser) {
	const url = new URLSearchParams(window.location.search).get(CHAT_NEXT_URL_PARAM);
	if (url === '1') setChatNext(true);
	else if (url === '0') setChatNext(false);
}

/** Whether the lively chat presentation layer is active. */
export const chatNext = derived(stored, ($stored) => resolveChatNext({ stored: $stored }));
