import { describe, expect, it, vi } from 'vitest';
import { createPressGesture, HOLD_THRESHOLD_MS } from './pressGesture';

/** Deterministic timer stand-ins so no test depends on wall-clock timing. */
function fakeTimers() {
	let pending: { id: number; fn: () => void } | null = null;
	let nextId = 1;
	return {
		setTimeout: (fn: () => void) => {
			pending = { id: nextId++, fn };
			return pending.id as unknown as ReturnType<typeof setTimeout>;
		},
		clearTimeout: () => {
			pending = null;
		},
		/** Fire the armed hold timer, as the real clock would at the threshold. */
		elapse: () => {
			const fn = pending?.fn;
			pending = null;
			fn?.();
		},
		get armed() {
			return pending !== null;
		}
	};
}

function setup() {
	const onHoldStart = vi.fn();
	const onHoldEnd = vi.fn();
	const onTap = vi.fn();
	const timers = fakeTimers();
	const gesture = createPressGesture(
		{ onHoldStart, onHoldEnd, onTap },
		{ setTimeout: timers.setTimeout, clearTimeout: timers.clearTimeout }
	);
	return { gesture, timers, onHoldStart, onHoldEnd, onTap };
}

describe('createPressGesture', () => {
	it('treats a quick press/release as a tap, never a hold', () => {
		const { gesture, onHoldStart, onHoldEnd, onTap } = setup();
		gesture.down();
		gesture.up();
		expect(onTap).toHaveBeenCalledOnce();
		expect(onHoldStart).not.toHaveBeenCalled();
		expect(onHoldEnd).not.toHaveBeenCalled();
	});

	it('starts push-to-talk once the hold threshold elapses', () => {
		const { gesture, timers, onHoldStart, onTap } = setup();
		gesture.down();
		timers.elapse();
		expect(onHoldStart).toHaveBeenCalledOnce();
		expect(onTap).not.toHaveBeenCalled();
		expect(gesture.isHolding).toBe(true);
	});

	it('ends push-to-talk on release and does NOT also fire a tap', () => {
		const { gesture, timers, onHoldEnd, onTap } = setup();
		gesture.down();
		timers.elapse();
		gesture.up();
		expect(onHoldEnd).toHaveBeenCalledOnce();
		expect(onTap).not.toHaveBeenCalled();
		expect(gesture.isHolding).toBe(false);
	});

	it('suppresses the click that follows a hold, but not the one after a tap', () => {
		const { gesture, timers } = setup();
		gesture.down();
		timers.elapse();
		gesture.up();
		// The browser still emits click after pointerup; the host must drop it.
		expect(gesture.shouldSuppressClick()).toBe(true);
		// Suppression is one-shot.
		expect(gesture.shouldSuppressClick()).toBe(false);

		gesture.down();
		gesture.up();
		expect(gesture.shouldSuppressClick()).toBe(false);
	});

	it('stops an in-flight hold when the pointer is cancelled or leaves', () => {
		const { gesture, timers, onHoldEnd, onTap } = setup();
		gesture.down();
		timers.elapse();
		gesture.cancel();
		expect(onHoldEnd).toHaveBeenCalledOnce();
		expect(onTap).not.toHaveBeenCalled();
		expect(gesture.isHolding).toBe(false);
	});

	it('cancelling before the threshold fires nothing at all', () => {
		const { gesture, timers, onHoldStart, onHoldEnd, onTap } = setup();
		gesture.down();
		gesture.cancel();
		expect(timers.armed).toBe(false);
		expect(onHoldStart).not.toHaveBeenCalled();
		expect(onHoldEnd).not.toHaveBeenCalled();
		expect(onTap).not.toHaveBeenCalled();
	});

	it('ignores a repeated down and a stray up so callbacks never double-fire', () => {
		const { gesture, timers, onHoldStart, onHoldEnd, onTap } = setup();
		gesture.up(); // stray release with no press
		expect(onTap).not.toHaveBeenCalled();
		expect(onHoldEnd).not.toHaveBeenCalled();

		gesture.down();
		gesture.down(); // duplicate press (e.g. re-entrant pointer events)
		timers.elapse();
		expect(onHoldStart).toHaveBeenCalledOnce();
		gesture.up();
		gesture.up();
		expect(onHoldEnd).toHaveBeenCalledOnce();
	});

	it('exposes a hold threshold long enough to not hijack an ordinary click', () => {
		expect(HOLD_THRESHOLD_MS).toBeGreaterThanOrEqual(200);
	});
});
