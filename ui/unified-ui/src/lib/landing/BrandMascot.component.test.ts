import { act, cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import BrandMascot from './BrandMascot.svelte';
import { motionEnabled } from '$lib/motion';

const { loadAnimation, players, state } = vi.hoisted(() => ({
	loadAnimation: vi.fn(),
	players: [] as {
		play: ReturnType<typeof vi.fn>;
		pause: ReturnType<typeof vi.fn>;
		destroy: ReturnType<typeof vi.fn>;
		addEventListener: ReturnType<typeof vi.fn>;
		ready: () => void;
	}[],
	state: { autoReady: true }
}));
vi.mock('lottie-web', () => ({ default: { loadAnimation } }));

const response = () => ({ ok: true, json: async () => ({ layers: [] }) });
function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((done) => { resolve = done; });
	return { promise, resolve };
}

beforeEach(() => {
	vi.clearAllMocks();
	players.length = 0;
	state.autoReady = true;
	motionEnabled.set(true);
	vi.stubGlobal('fetch', vi.fn(async () => response()));
	loadAnimation.mockImplementation(() => {
		const player = {
			play: vi.fn(), pause: vi.fn(), destroy: vi.fn(), ready: () => {},
			addEventListener: vi.fn((_event: string, callback: () => void) => {
				player.ready = callback;
				if (state.autoReady) queueMicrotask(callback);
			})
		};
		players.push(player);
		return player;
	});
});
afterEach(() => {
	cleanup();
	motionEnabled.set(true);
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

async function firstPlayer(): Promise<void> {
	await waitFor(() => expect(players[0]?.play).toHaveBeenCalled());
}

describe('BrandMascot', () => {
	it('uses only the static pose when reduced motion is enabled', async () => {
		motionEnabled.set(false);
		const { container, rerender } = render(BrandMascot);
		await rerender({ mood: 'working', dark: true });
		expect(container.querySelector('img')).toHaveAttribute('src', '/brand/mascot-hi-dark.svg');
		expect(fetch).not.toHaveBeenCalled();
		expect(loadAnimation).not.toHaveBeenCalled();
	});

	it('retains the old loop until the next SVG is ready, then retires it after the fade', async () => {
		const { container, rerender, unmount } = render(BrandMascot, { mood: 'working' });
		await firstPlayer();
		const firstContainer = container.querySelector('.mascot-motion.active');
		state.autoReady = false;
		await rerender({ mood: 'side-quest', dark: true });
		expect(players[0].destroy).not.toHaveBeenCalled();
		expect(container.querySelector('.brand-mascot')).toHaveClass('ready');
		await waitFor(() => expect(loadAnimation).toHaveBeenCalledTimes(2), { timeout: 2000 });
		expect(fetch).toHaveBeenLastCalledWith('/brand/animations/side-quest-dark.json', expect.anything());
		expect(container.querySelector('.mascot-motion.active')).toBe(firstContainer);
		expect(players[1].play).not.toHaveBeenCalled();
		await act(() => players[1].ready());
		expect(container.querySelector('.mascot-motion.active')).not.toBe(firstContainer);
		expect(players[1].play).toHaveBeenCalled();
		expect(players[0].destroy).not.toHaveBeenCalled();
		await waitFor(() => expect(players[0].destroy).toHaveBeenCalledOnce());
		expect(loadAnimation).toHaveBeenLastCalledWith(expect.objectContaining({ loop: true, autoplay: false }));
		unmount();
		expect(players[0].destroy).toHaveBeenCalledOnce();
		expect(players[1].destroy).toHaveBeenCalledOnce();
	});

	it('keeps the current animation visible if the next asset fails', async () => {
		const { container, rerender } = render(BrandMascot);
		await firstPlayer();
		vi.mocked(fetch).mockResolvedValueOnce({ ok: false } as Response);
		await rerender({ mood: 'working' });
		await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
		expect(container.querySelector('.brand-mascot')).toHaveClass('ready');
		expect(loadAnimation).toHaveBeenCalledOnce();
		expect(players[0].destroy).not.toHaveBeenCalled();
	});

	it('ignores a slow stale request when selections change rapidly', async () => {
		const { rerender, unmount } = render(BrandMascot);
		await firstPlayer();
		const slow = deferred<Response>();
		vi.mocked(fetch).mockReturnValueOnce(slow.promise);
		await rerender({ mood: 'working' });
		await waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
		const signal = vi.mocked(fetch).mock.calls[1][1]?.signal;
		await rerender({ mood: 'side-quest' });
		await waitFor(() => expect(loadAnimation).toHaveBeenCalledTimes(2), { timeout: 2000 });
		expect(signal?.aborted).toBe(true);
		await act(async () => { slow.resolve(response() as Response); });
		expect(loadAnimation).toHaveBeenCalledTimes(2);
		unmount();
		for (const player of players) expect(player.destroy).toHaveBeenCalledOnce();
	});

	it('pauses both sides of a handover when the document hides, and releases them for reduced motion', async () => {
		const { container, rerender, unmount } = render(BrandMascot);
		await firstPlayer();
		await rerender({ mood: 'side-quest' });
		await waitFor(() => expect(players[1]?.play).toHaveBeenCalled(), { timeout: 2000 });
		const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
		await fireEvent(document, new Event('visibilitychange'));
		for (const player of players) expect(player.pause).toHaveBeenCalled();
		hidden.mockReturnValue(false);
		await fireEvent(document, new Event('visibilitychange'));
		await act(() => motionEnabled.set(false));
		expect(container.querySelector('.brand-mascot')).not.toHaveClass('ready');
		for (const player of players) expect(player.destroy).toHaveBeenCalledOnce();
		await rerender({ mood: 'working', dark: true });
		expect(fetch).toHaveBeenCalledTimes(2);
		unmount();
		for (const player of players) expect(player.destroy).toHaveBeenCalledOnce();
	});

	it('does not create a player after unmounting during an asset request', async () => {
		const slow = deferred<Response>();
		vi.mocked(fetch).mockReturnValueOnce(slow.promise);
		const { unmount } = render(BrandMascot);
		await waitFor(() => expect(fetch).toHaveBeenCalledOnce());
		const signal = vi.mocked(fetch).mock.calls[0][1]?.signal;
		unmount();
		await act(async () => { slow.resolve(response() as Response); });
		expect(signal?.aborted).toBe(true);
		expect(loadAnimation).not.toHaveBeenCalled();
	});
});
