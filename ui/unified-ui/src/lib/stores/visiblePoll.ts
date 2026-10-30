// A page-owned poll with a user-chosen interval that runs only while the
// browser tab is visible, and reports when the next refresh is due so the
// page can show a countdown.
//
// Use `createSharedPoll` when several consumers read one backend resource;
// use this when one page owns the request (its target changes with the
// page) and the viewer picks the cadence. Failures back off with jitter
// (realtime/backoff.ts) but never refresh sooner than the chosen interval.

import { writable, type Readable } from 'svelte/store';
import { browser } from '$app/environment';
import { createBackoff } from '$lib/realtime/backoff';

export const POLL_INTERVAL_CHOICES_MS = [30_000, 60_000, 120_000, 300_000, 900_000] as const;
export const DEFAULT_POLL_INTERVAL_MS = 60_000;

export interface VisiblePoll {
	/** Epoch ms the next refresh is due; `null` while hidden, stopped or fetching. */
	nextAt: Readable<number | null>;
	/** Start (or restart) polling, refreshing immediately. */
	start(): void;
	stop(): void;
	/** Refresh now and restart the interval from this refresh. */
	pollNow(): void;
	setIntervalMs(ms: number): void;
}

export function createVisiblePoll(opts: {
	/** One refresh. Throw on failure (drives the backoff). */
	run: () => Promise<void>;
	intervalMs: number;
	maxBackoffMs?: number;
}): VisiblePoll {
	let intervalMs = opts.intervalMs;
	const backoff = createBackoff({ initialMs: intervalMs, maxMs: opts.maxBackoffMs ?? 900_000 });
	const nextAt = writable<number | null>(null);
	let running = false;
	let inFlight = false;
	/** A refresh was asked for mid-fetch; run again right after, for the new target. */
	let runAgain = false;
	let lastRunAt = 0;
	/** Identifies the newest refresh; an older one finishing never reschedules. */
	let seq = 0;
	let dueAt: number | null = null;
	let timer: ReturnType<typeof setTimeout> | null = null;

	const hidden = () => browser && document.visibilityState === 'hidden';

	function clearTimer(): void {
		if (timer) clearTimeout(timer);
		timer = null;
	}

	function schedule(delayMs: number): void {
		clearTimer();
		dueAt = Date.now() + delayMs;
		if (hidden()) {
			nextAt.set(null);
			return;
		}
		nextAt.set(dueAt);
		timer = setTimeout(() => void tick(), delayMs);
	}

	async function tick(force = false): Promise<void> {
		if (!running) return;
		if (inFlight && !force) {
			runAgain = true;
			return;
		}
		clearTimer();
		if (hidden()) {
			// Paused; `onVisibility` refreshes when the tab comes back.
			dueAt = Date.now();
			nextAt.set(null);
			return;
		}
		const mine = ++seq;
		inFlight = true;
		runAgain = false;
		nextAt.set(null);
		let delay = intervalMs;
		try {
			await opts.run();
			backoff.reset();
		} catch {
			delay = Math.max(intervalMs, backoff.nextMs());
		}
		if (mine !== seq) return;
		inFlight = false;
		lastRunAt = Date.now();
		if (!running) return;
		if (runAgain) {
			runAgain = false;
			void tick();
		} else {
			schedule(delay);
		}
	}

	function onVisibility(): void {
		if (!running || inFlight) return;
		if (hidden()) {
			clearTimer();
			nextAt.set(null);
			return;
		}
		const remaining = (dueAt ?? 0) - Date.now();
		if (remaining <= 0) void tick();
		else schedule(remaining);
	}

	return {
		nextAt: { subscribe: nextAt.subscribe },
		start(): void {
			if (!browser) return;
			if (!running) {
				running = true;
				document.addEventListener('visibilitychange', onVisibility);
			}
			backoff.reset();
			dueAt = Date.now();
			// A new target must not wait for the previous target's request.
			void tick(true);
		},
		stop(): void {
			if (running && browser) document.removeEventListener('visibilitychange', onVisibility);
			running = false;
			runAgain = false;
			inFlight = false;
			seq += 1;
			clearTimer();
			dueAt = null;
			nextAt.set(null);
		},
		pollNow(): void {
			if (!running) return;
			void tick();
		},
		setIntervalMs(ms: number): void {
			if (!Number.isFinite(ms) || ms <= 0 || ms === intervalMs) return;
			intervalMs = ms;
			backoff.reset();
			// Keep the time already waited; a shorter interval may be due now.
			if (running && !inFlight) schedule(Math.max(0, lastRunAt + ms - Date.now()));
		}
	};
}
