// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
vi.mock('$app/environment', () => ({ browser: true }));
import { get } from 'svelte/store';
import { createVisiblePoll } from './visiblePoll';

let visibility: DocumentVisibilityState = 'visible';
function setVisibility(state: DocumentVisibilityState): void {
	visibility = state;
	document.dispatchEvent(new Event('visibilitychange'));
}

beforeEach(() => {
	vi.useFakeTimers();
	visibility = 'visible';
	vi.spyOn(document, 'visibilityState', 'get').mockImplementation(() => visibility);
});
afterEach(() => {
	vi.useRealTimers();
	vi.restoreAllMocks();
});

it('refreshes at once, then every interval, and exposes when the next refresh is due', async () => {
	const run = vi.fn().mockResolvedValue(undefined);
	const poll = createVisiblePoll({ run, intervalMs: 60_000 });
	poll.start();
	await vi.advanceTimersByTimeAsync(0);
	expect(run).toHaveBeenCalledTimes(1);
	expect(get(poll.nextAt)).toBe(Date.now() + 60_000);
	await vi.advanceTimersByTimeAsync(60_000);
	expect(run).toHaveBeenCalledTimes(2);
	poll.stop();
});

it('does not poll while the tab is hidden and catches up when it is shown again', async () => {
	const run = vi.fn().mockResolvedValue(undefined);
	const poll = createVisiblePoll({ run, intervalMs: 60_000 });
	poll.start();
	await vi.advanceTimersByTimeAsync(0);
	setVisibility('hidden');
	expect(get(poll.nextAt)).toBeNull();
	await vi.advanceTimersByTimeAsync(5 * 60_000);
	expect(run).toHaveBeenCalledTimes(1);
	setVisibility('visible');
	await vi.advanceTimersByTimeAsync(0);
	expect(run).toHaveBeenCalledTimes(2);
	poll.stop();
});

it('a shorter interval counts the time already waited', async () => {
	const run = vi.fn().mockResolvedValue(undefined);
	const poll = createVisiblePoll({ run, intervalMs: 300_000 });
	poll.start();
	await vi.advanceTimersByTimeAsync(45_000);
	poll.setIntervalMs(30_000);
	await vi.advanceTimersByTimeAsync(0);
	expect(run).toHaveBeenCalledTimes(2);
	poll.stop();
});

it('a failed refresh never retries sooner than the interval', async () => {
	const run = vi.fn().mockRejectedValue(new Error('down'));
	const poll = createVisiblePoll({ run, intervalMs: 60_000 });
	poll.start();
	await vi.advanceTimersByTimeAsync(59_000);
	expect(run).toHaveBeenCalledTimes(1);
	poll.stop();
});

it('starting for a new target does not wait for the previous request', async () => {
	let finishOld!: () => void;
	const run = vi
		.fn()
		.mockImplementationOnce(() => new Promise<void>((resolve) => (finishOld = resolve)))
		.mockResolvedValue(undefined);
	const poll = createVisiblePoll({ run, intervalMs: 60_000 });
	poll.start();
	poll.start();
	await vi.advanceTimersByTimeAsync(0);
	expect(run).toHaveBeenCalledTimes(2);
	finishOld();
	await vi.advanceTimersByTimeAsync(0);
	// The stale request finishing does not schedule a second timer.
	await vi.advanceTimersByTimeAsync(60_000);
	expect(run).toHaveBeenCalledTimes(3);
	poll.stop();
});
