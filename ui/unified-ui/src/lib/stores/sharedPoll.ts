// One poller per backend resource, shared by every consumer.
//
// Before this existed, the TopBar store AND the Observe page each ran
// their own interval against the same endpoint (`/meetings/active`,
// `/screen/observe/status`), so the busiest page doubled the request
// rate. A shared poll gives every consumer the same readable; pages
// that need snappier data hold a `requestFast()` lease instead of
// fetching themselves, and mutations call `pollNow()` instead of a
// page-local refresh.
//
// Failure behavior: consecutive failed fetches back off exponentially
// with jitter (see realtime/backoff.ts) up to `maxBackoffMs`, so a dead
// backend costs a few requests per minute, not a steady hammer. The
// last good value is kept.

import { readable, type Readable } from 'svelte/store';
import { browser } from '$app/environment';
import { createBackoff } from '$lib/realtime/backoff';

export interface SharedPoll<T> {
	/** Poll-while-subscribed readable; `null` until the first successful fetch. */
	value: Readable<T | null>;
	/** Refresh immediately (after a mutation). Coalesces with an in-flight poll. */
	pollNow(): void;
	/** Hold a fast-cadence lease (e.g. while a live-controls page is open).
	 *  Returns the release function; cadence drops back when no leases remain. */
	requestFast(): () => void;
}

export function createSharedPoll<T>(opts: {
	/** Fetch + parse one snapshot. Throw on any failure (drives the backoff). */
	fetcher: () => Promise<T>;
	idleMs: number;
	fastMs: number;
	maxBackoffMs?: number;
}): SharedPoll<T> {
	const backoff = createBackoff({
		initialMs: opts.idleMs,
		maxMs: opts.maxBackoffMs ?? 120_000
	});

	let setValue: ((v: T | null) => void) | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;
	let inFlight = false;
	/** A pollNow() arrived while a fetch was running — run again right after,
	 *  so a mutation's refresh can't be swallowed by a stale in-flight read. */
	let runAgain = false;
	let fastLeases = 0;

	function intervalMs(): number {
		return fastLeases > 0 ? opts.fastMs : opts.idleMs;
	}

	function clearTimer(): void {
		if (timer) {
			clearTimeout(timer);
			timer = null;
		}
	}

	function schedule(delayMs: number): void {
		if (!setValue) return; // no subscribers — poller is parked
		clearTimer();
		timer = setTimeout(() => void pollOnce(), delayMs);
	}

	async function pollOnce(): Promise<void> {
		if (!setValue) return;
		if (inFlight) {
			runAgain = true;
			return;
		}
		inFlight = true;
		let nextDelay: number;
		try {
			const snapshot = await opts.fetcher();
			setValue?.(snapshot);
			backoff.reset();
			nextDelay = intervalMs();
		} catch {
			// Backend unreachable / non-OK — keep the last value, slow down.
			nextDelay = Math.max(intervalMs(), backoff.nextMs());
		} finally {
			inFlight = false;
		}
		if (runAgain) {
			runAgain = false;
			schedule(0);
		} else {
			schedule(nextDelay);
		}
	}

	const value = readable<T | null>(null, (set) => {
		if (!browser) return;
		setValue = set;
		schedule(0);
		return () => {
			setValue = null;
			clearTimer();
			inFlight = false;
			runAgain = false;
		};
	});

	return {
		value,
		pollNow(): void {
			if (!setValue) return;
			schedule(0);
		},
		requestFast(): () => void {
			fastLeases += 1;
			// Tighten the cadence right away — the lease holder is a page
			// that wants fresh data now, not in up-to-idleMs.
			if (setValue && !inFlight) schedule(0);
			let released = false;
			return () => {
				if (released) return;
				released = true;
				fastLeases = Math.max(0, fastLeases - 1);
			};
		}
	};
}
