import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Hud from './+page.svelte';

const native = vi.hoisted(() => ({
	expanded: false,
	listeners: new Map<string, (event: { payload: boolean }) => void>(),
	invoke: vi.fn(),
	goto: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('$app/navigation', () => ({ goto: native.goto }));
vi.mock('$app/stores', async () => ({ page: (await import('svelte/store')).readable({ url: new URL('http://localhost/hud?thread=notes&session=selected-session') }) }));
vi.mock('$lib/magician/chat/ChatPanel.svelte', async () => ({ default: (await import('../../test/fixtures/HudChatPanelStub.svelte')).default }));
vi.mock('$lib/media/session', () => ({ ensureMediaSessionStarted: vi.fn().mockResolvedValue(null) }));
vi.mock('$lib/stores/scopeIdentityStore', async (original) => ({
	...(await original<object>()),
	installScopedApiFetch: vi.fn(),
}));
vi.mock('$lib/stores/pendingHitlStore', async (original) => ({ ...(await original<object>()), ensurePendingHitlBridge: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ label: 'overlay', isVisible: async () => false }), currentMonitor: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
	listen: async (name: string, callback: (event: { payload: boolean }) => void) => {
		native.listeners.set(name, callback);
		return () => native.listeners.delete(name);
	}
}));

beforeEach(() => {
	native.expanded = false;
	native.listeners.clear();
	native.goto.mockClear();
	native.invoke.mockReset().mockImplementation(async (command: string, args?: { expanded: boolean }) => {
		if (command === 'get_hud_expanded') return native.expanded;
		if (command === 'set_hud_expanded') native.expanded = args!.expanded;
		return null;
	});
	Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {}, configurable: true });
	vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { headers: { 'Content-Type': 'application/json' } })));
});

afterEach(() => {
	cleanup();
	Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
	vi.unstubAllGlobals();
});

async function mountHud() {
	const mounted = render(Hud);
	await waitFor(() => expect(native.listeners.has('overlay-focus-input')).toBe(true));
	return mounted;
}

describe('HUD native window mode', () => {
	it('places native screen capture beside attachments instead of in the dock', async () => {
		await mountHud();
		const capture = screen.getByRole('button', { name: 'Attach screen context' });
		expect(screen.getByTestId('attachment-actions')).toContainElement(capture);
		expect(screen.getByTestId('composer-dock')).not.toContainElement(capture);
	});
	it('opens the actual chat route with the selected thread and session', async () => {
		await mountHud();
		expect(screen.getByTestId('hud-chat')).toHaveAttribute('data-thread', 'notes');
		await fireEvent.click(screen.getByRole('button', { name: 'Expand HUD' }));
		await waitFor(() => expect(native.invoke).toHaveBeenCalledWith('set_hud_expanded', { expanded: true }));
		expect(native.goto).toHaveBeenCalledWith('/chat?thread=notes&session=selected-session', { keepFocus: true, noScroll: true });
	});

	it('opens the regular chat page for a reply', async () => {
		await mountHud();
		await fireEvent.click(screen.getByRole('button', { name: 'Reply in HUD' }));
		await waitFor(() => expect(native.goto).toHaveBeenCalledWith('/chat?thread=notes&session=selected-session', { keepFocus: true, noScroll: true }));
	});

	it('releases HUD native listeners when it gives way to the app', async () => {
		const mounted = await mountHud();
		mounted.unmount();
		expect(native.listeners.size).toBe(0);
	});

	it('retains compact chrome and reports a native resize failure', async () => {
		await mountHud();
		native.invoke.mockImplementation(async (command) => {
			if (command === 'set_hud_expanded') throw new Error('Resize failed');
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Expand HUD' }));
		expect(await screen.findByRole('alert')).toHaveTextContent('Resize failed');
		expect(screen.getByRole('button', { name: 'Expand HUD' })).toBeInTheDocument();
	});
});
