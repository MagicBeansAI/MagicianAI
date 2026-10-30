import { browser } from '$app/environment';
import { goto } from '$app/navigation';
import { page } from '$app/stores';
import { get, writable } from 'svelte/store';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { chatStore } from '$lib/stores/chatStore';

export const hudWindow = writable({ available: false, ready: false, expanded: false });
let changing = false;

export function isHudWindow(): boolean {
	return browser && '__TAURI_INTERNALS__' in window && getCurrentWindow().label === 'overlay';
}

/** The regular chat page accepts the same thread/session deep link as Quick Chat. */
export function hudModeHref(url: URL, expanded: boolean): string {
	const thread = /^\/t\/([^/]+)(?:\/chat)?\/?$/.exec(url.pathname);
	const chatRoute = /^\/(?:hud|chat)\/?$/.test(url.pathname) || !!thread;
	// Other app pages can carry their own session IDs (notably VibeDev).
	// Returning to Quick Chat must resume a chat, never re-open a coding run.
	const params = chatRoute ? new URLSearchParams(url.search) : new URLSearchParams();
	if (url.pathname === '/vibe') {
		for (const key of ['draft', 'q']) {
			if (url.searchParams.has(key)) params.set(key, url.searchParams.get(key)!);
		}
	}
	if (thread) params.set('thread', decodeURIComponent(thread[1]));
	const chat = get(chatStore);
	const sessionId = chat.viewingArchivedId ?? chat.activeSessionId;
	if (!params.has('session') && sessionId) {
		params.set('session', sessionId);
		const session = chat.sessions.find((entry) => entry.id === sessionId);
		if (!params.has('thread') && session?.ui_thread_id) params.set('thread', session.ui_thread_id);
	}
	// Legacy page switches carried q, which would otherwise send on mount.
	if (params.has('q') && !params.has('draft')) params.set('draft', params.get('q')!);
	params.delete('q');
	const search = params.toString();
	return `${expanded ? '/chat' : '/hud'}${search ? `?${search}` : ''}${url.hash}`;
}

export async function setHudWindowExpanded(expanded: boolean, href?: string): Promise<void> {
	if (changing) return;
	changing = true;
	try {
		await invoke('set_hud_expanded', { expanded });
		document.documentElement.classList.toggle('desktop-app-window', expanded);
		hudWindow.set({ available: true, ready: true, expanded });
		await goto(href ?? hudModeHref(get(page).url, expanded), { keepFocus: true, noScroll: true });
	} finally { changing = false; }
}

/** Root-owned: stays mounted while the window switches between HUD and app routes. */
export async function connectHudWindow(): Promise<() => void> {
	if (!isHudWindow()) return () => {};
	let disposed = false;
	let revision = 0;
	const sync = async (expanded: boolean, initial = false) => {
		document.documentElement.classList.toggle('desktop-app-window', expanded);
		hudWindow.set({ available: true, ready: true, expanded });
		if (changing) return;
		const url = get(page).url;
		// Authentication and special overlay routes remain available on boot.
		if (initial && !expanded && !/^\/(?:hud|chat|vibe)(?:\/|$)|^\/t\/[^/]+(?:\/chat)?\/?$/.test(url.pathname)) return;
		if (expanded ? url.pathname === '/hud' : url.pathname !== '/hud') {
			await goto(hudModeHref(url, expanded), { replaceState: true });
		}
	};
	const unlisten = await listen<boolean>('hud-window-mode', (event) => {
		revision += 1;
		if (!disposed) void sync(event.payload).catch((error) => console.warn('[desktop] Window navigation failed', error));
	});
	const unlistenCapture = await listen<{ thread_id: string; session_id: string }>('screen-ask-capture', (event) => {
		if (disposed || get(page).url.pathname === '/hud') return;
		const query = new URLSearchParams({ thread: event.payload.thread_id, session: event.payload.session_id });
		// The capture remains staged natively until the HUD mounts and consumes it.
		void setHudWindowExpanded(false, `/hud?${query}`).catch((error) => console.warn('[desktop] Could not open screen capture', error));
	});
	const initialRevision = revision;
	try {
		const expanded = await invoke<boolean>('get_hud_expanded');
		if (!disposed && revision === initialRevision) await sync(expanded, true);
	} catch (error) { unlisten(); unlistenCapture(); throw error; }
	return () => { disposed = true; unlisten(); unlistenCapture(); document.documentElement.classList.remove('desktop-app-window'); hudWindow.set({ available: false, ready: false, expanded: false }); };
}
