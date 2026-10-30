// One 1 s clock for every live "how long has this step been running" counter.
//
// The chat activity card, the turn progress bubble, the chat-next stage and the
// task panel's Run timeline all show a running row's elapsed time. Each used to
// show nothing until the step settled, so a 40-second model call read as a
// frozen row. They now read `secondTick`, one store whose single interval runs
// only while something is subscribed: a host holds a subscription through
// `liveClock(...).sync(anyRunning)` only while it has a running row, and drops
// it on destroy, so a settled or idle surface costs no timer at all.

import { readable, type Readable } from 'svelte/store';

/** How often a live elapsed counter advances. */
export const LIVE_TICK_MS = 1000;

/**
 * `Date.now()`, refreshed every `LIVE_TICK_MS` while subscribed. The interval
 * starts with the first subscriber and stops with the last, so every running row
 * on screen ticks off one timer, in step.
 */
export const secondTick: Readable<number> = readable(Date.now(), (set) => {
	set(Date.now());
	const id = setInterval(() => set(Date.now()), LIVE_TICK_MS);
	return () => clearInterval(id);
});

export interface LiveClock {
	/** Hold the tick while `active` (some row is running); release it otherwise. */
	sync(active: boolean): void;
	/** Release the tick for good — call from `onDestroy`. */
	stop(): void;
}

/**
 * A host's handle on `secondTick`: subscribed only while `sync(true)`, and
 * `onTick` receives each new `now`. Calling `sync` with the same value twice is a
 * no-op, so a host can call it from a reactive statement on every update.
 */
export function liveClock(onTick: (now: number) => void): LiveClock {
	let release: (() => void) | null = null;
	return {
		sync(active: boolean) {
			if (active && release === null) {
				release = secondTick.subscribe(onTick);
			} else if (!active && release !== null) {
				release();
				release = null;
			}
		},
		stop() {
			release?.();
			release = null;
		}
	};
}

/** `m:ss`, or `h:mm:ss` past an hour. */
export function formatElapsed(ms: number): string {
	const total = Math.max(0, Math.floor(ms / 1000));
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = String(total % 60).padStart(2, '0');
	return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`;
}

/**
 * A running row's elapsed time as `m:ss` (`h:mm:ss` past an hour) — the format
 * the live task card's own run timer already uses — or `null` when the row has
 * no usable start. A start slightly ahead of this client's clock (server skew)
 * reads `0:00`, never a negative.
 */
export function runningElapsed(startedAt: number | null | undefined, now: number): string | null {
	if (typeof startedAt !== 'number' || !Number.isFinite(startedAt) || startedAt <= 0) return null;
	if (!Number.isFinite(now)) return null;
	return formatElapsed(now - startedAt);
}
