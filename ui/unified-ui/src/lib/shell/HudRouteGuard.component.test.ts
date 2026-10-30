import { cleanup, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { page } from '$app/stores';
import { get } from 'svelte/store';
import HudRouteGuard from './HudRouteGuard.svelte';
import { hudWindow, setHudWindowExpanded } from './hudWindow';

const navigation = vi.hoisted(() => ({
	label: 'overlay', expanded: false,
	before: null as null | ((event: { to: { url: URL } | null; cancel: () => void }) => void),
	listeners: new Map<string, (event: { payload: boolean | { thread_id: string; session_id: string } }) => void>(),
	goto: vi.fn().mockResolvedValue(undefined),
	invoke: vi.fn()
}));
vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).writable({ url: new URL('http://localhost/hud') }) }));
vi.mock('$lib/stores/chatStore', async () => ({ chatStore: (await import('svelte/store')).readable({ sessions: [], activeSessionId: null, viewingArchivedId: null }) }));
vi.mock('$app/navigation', () => ({
	beforeNavigate: (callback: NonNullable<typeof navigation.before>) => { navigation.before = callback; },
	goto: navigation.goto
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ label: navigation.label }) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: navigation.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: async (name: string, callback: (event: { payload: boolean | { thread_id: string; session_id: string } }) => void) => {
	navigation.listeners.set(name, callback);
	return () => navigation.listeners.delete(name);
} }));
function route(path: string) {
	window.history.replaceState({}, '', path);
	(page as unknown as { set: (value: { url: URL }) => void }).set({ url: new URL(path, 'http://localhost') });
}
async function mount() {
	const result = render(HudRouteGuard);
	await waitFor(() => expect(get(hudWindow).ready).toBe(true));
	return result;
}
beforeEach(() => {
	navigation.label = 'overlay'; navigation.expanded = false; navigation.before = null;
	navigation.goto.mockClear(); navigation.listeners.clear();
	navigation.invoke.mockReset().mockImplementation(async (command, args) => {
		if (command === 'get_hud_expanded') return navigation.expanded;
		if (command === 'set_hud_expanded') navigation.expanded = args.expanded;
	});
	hudWindow.set({ available: false, ready: false, expanded: false });
	Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {}, configurable: true });
	route('/hud');
});
afterEach(() => { cleanup(); Reflect.deleteProperty(window, '__TAURI_INTERNALS__'); route('/'); });

describe('desktop chat window routing', () => {
	it.each(['/vibe', '/chat/'])('recovers compact mode left on %s without auto-sending a draft', async (path) => {
		route(`${path}?q=fix%20layout&session=chat-1#draft`);
		await mount();
		expect(navigation.goto).toHaveBeenCalledExactlyOnceWith(path === '/vibe' ? '/hud?draft=fix+layout#draft' : '/hud?session=chat-1&draft=fix+layout#draft', { replaceState: true });
	});
	it.each(['/chat', '/vibe'])('keeps compact navigation to %s inside the launcher', async (path) => {
		await mount();
		const cancel = vi.fn();
		navigation.before!({ to: { url: new URL(`${path}?q=draft`, window.location.href) }, cancel });
		expect(cancel).toHaveBeenCalledOnce();
		expect(navigation.goto).toHaveBeenCalledWith('/hud?draft=draft', { replaceState: true });
	});
	it('allows all normal app navigation when expanded, including Talk/Code and history', async () => {
		navigation.expanded = true; route('/chat?session=chosen');
		await mount();
		const cancel = vi.fn();
		for (const path of ['/chat', '/vibe', '/t/notes/chat?session=older', '/work']) {
			navigation.before!({ to: { url: new URL(path, window.location.href) }, cancel });
		}
		expect(cancel).not.toHaveBeenCalled(); expect(navigation.goto).not.toHaveBeenCalled();
		expect(document.documentElement).toHaveClass('desktop-app-window');
	});
	it('opens the real chat page when the native Window menu expands the HUD', async () => {
		route('/hud?thread=notes&session=chosen'); await mount();
		navigation.listeners.get('hud-window-mode')!({ payload: true });
		await waitFor(() => expect(navigation.goto).toHaveBeenCalledWith('/chat?thread=notes&session=chosen', { replaceState: true }));
	});
	it('uses the real chat page after a frontend reload in expanded mode', async () => {
		navigation.expanded = true; route('/hud?session=chosen'); await mount();
		expect(navigation.goto).toHaveBeenCalledWith('/chat?session=chosen', { replaceState: true });
	});
	it('returns from thread chat to compact with the exact session and thread', async () => {
		navigation.expanded = true; route('/t/project%20notes/chat?session=older'); await mount();
		await setHudWindowExpanded(false);
		expect(navigation.invoke).toHaveBeenCalledWith('set_hud_expanded', { expanded: false });
		expect(navigation.goto).toHaveBeenCalledWith('/hud?session=older&thread=project+notes', { keepFocus: true, noScroll: true });
		expect(document.documentElement).not.toHaveClass('desktop-app-window');
	});
	it('resets a closed app window to the compact route for the next summon', async () => {
		navigation.expanded = true; route('/work'); const result = await mount();
		navigation.listeners.get('hud-window-mode')!({ payload: false });
		await waitFor(() => expect(navigation.goto).toHaveBeenCalledWith('/hud', { replaceState: true }));
		result.unmount(); expect(navigation.listeners.size).toBe(0);
	});
	it('returns from Code without passing a coding session into the HUD', async () => {
		navigation.expanded = true; route('/vibe?session=coding-run'); await mount();
		await setHudWindowExpanded(false);
		expect(navigation.goto).toHaveBeenCalledWith('/hud', { keepFocus: true, noScroll: true });
	});
	it('opens a captured screen on its exact HUD session from the expanded app', async () => {
		navigation.expanded = true; route('/chat'); await mount();
		navigation.listeners.get('screen-ask-capture')!({ payload: { thread_id: 'screens', session_id: 'screen-session' } });
		await waitFor(() => expect(navigation.goto).toHaveBeenCalledWith('/hud?thread=screens&session=screen-session', { keepFocus: true, noScroll: true }));
		expect(navigation.invoke).toHaveBeenCalledWith('set_hud_expanded', { expanded: false });
	});
	it('permits sign-in while the native window is compact', async () => {
		route('/login'); await mount();
		expect(navigation.goto).not.toHaveBeenCalled();
	});
	it.each(['magician-attention', 'main', 'browser'])('leaves %s windows alone', (label) => {
		navigation.label = label;
		if (label === 'browser') Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
		route('/chat'); render(HudRouteGuard);
		const cancel = vi.fn();
		navigation.before!({ to: { url: new URL('/vibe', window.location.href) }, cancel });
		expect(cancel).not.toHaveBeenCalled(); expect(navigation.goto).not.toHaveBeenCalled();
	});
});
