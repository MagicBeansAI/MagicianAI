import { afterEach, describe, expect, it, vi } from 'vitest';

import { createSharedPoll } from './sharedPoll';

afterEach(() => {
	vi.useRealTimers();
});

describe('createSharedPoll browser lifecycle', () => {
	it('polls only while subscribed and follows the idle cadence', async () => {
		vi.useFakeTimers();
		const fetcher = vi.fn(async () => ({ sequence: fetcher.mock.calls.length }));
		const poll = createSharedPoll({ fetcher, idleMs: 1_000, fastMs: 100 });
		const values: Array<{ sequence: number } | null> = [];

		const unsubscribe = poll.value.subscribe((value) => values.push(value));
		await vi.advanceTimersByTimeAsync(0);
		expect(fetcher).toHaveBeenCalledTimes(1);
		expect(values.at(-1)).toEqual({ sequence: 1 });

		await vi.advanceTimersByTimeAsync(999);
		expect(fetcher).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(1);
		expect(fetcher).toHaveBeenCalledTimes(2);

		unsubscribe();
		await vi.advanceTimersByTimeAsync(5_000);
		expect(fetcher).toHaveBeenCalledTimes(2);
	});

	it('coalesces a mutation refresh with an in-flight read and then runs once more', async () => {
		vi.useFakeTimers();
		let resolveFirst!: (value: number) => void;
		const fetcher = vi
			.fn<() => Promise<number>>()
			.mockImplementationOnce(
				() => new Promise<number>((resolve) => (resolveFirst = resolve))
			)
			.mockResolvedValue(2);
		const poll = createSharedPoll({ fetcher, idleMs: 1_000, fastMs: 100 });
		const unsubscribe = poll.value.subscribe(() => undefined);
		await vi.advanceTimersByTimeAsync(0);

		poll.pollNow();
		await vi.advanceTimersByTimeAsync(0);
		expect(fetcher).toHaveBeenCalledTimes(1);
		resolveFirst(1);
		await Promise.resolve();
		await vi.advanceTimersByTimeAsync(0);
		expect(fetcher).toHaveBeenCalledTimes(2);
		unsubscribe();
	});

	it('tightens cadence while any fast lease is held and releases idempotently', async () => {
		vi.useFakeTimers();
		const fetcher = vi.fn(async () => fetcher.mock.calls.length);
		const poll = createSharedPoll({ fetcher, idleMs: 1_000, fastMs: 100 });
		const unsubscribe = poll.value.subscribe(() => undefined);
		await vi.advanceTimersByTimeAsync(0);

		const release = poll.requestFast();
		await vi.advanceTimersByTimeAsync(0);
		expect(fetcher).toHaveBeenCalledTimes(2);
		await vi.advanceTimersByTimeAsync(100);
		expect(fetcher).toHaveBeenCalledTimes(3);

		release();
		release();
		await vi.advanceTimersByTimeAsync(100);
		expect(fetcher).toHaveBeenCalledTimes(4);
		await vi.advanceTimersByTimeAsync(999);
		expect(fetcher).toHaveBeenCalledTimes(4);
		await vi.advanceTimersByTimeAsync(1);
		expect(fetcher).toHaveBeenCalledTimes(5);
		unsubscribe();
	});

	it('retains the last good snapshot when a later poll fails', async () => {
		vi.useFakeTimers();
		const fetcher = vi
			.fn<() => Promise<string>>()
			.mockResolvedValueOnce('healthy')
			.mockRejectedValueOnce(new Error('offline'));
		const poll = createSharedPoll({ fetcher, idleMs: 1_000, fastMs: 100 });
		const values: Array<string | null> = [];
		const unsubscribe = poll.value.subscribe((value) => values.push(value));

		await vi.advanceTimersByTimeAsync(0);
		await vi.advanceTimersByTimeAsync(1_000);

		expect(fetcher).toHaveBeenCalledTimes(2);
		expect(values.at(-1)).toBe('healthy');
		unsubscribe();
	});
});
