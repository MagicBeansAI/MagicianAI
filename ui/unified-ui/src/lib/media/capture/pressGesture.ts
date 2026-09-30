/**
 * Tap-vs-hold discrimination for a press-to-talk control.
 *
 * The composer mic is dual-purpose: a quick TAP toggles a recording the way it
 * always has, while a HOLD is push-to-talk — capture runs for exactly as long as
 * the button is held and finishes on release. Both gestures share one button, so
 * something has to decide which one happened.
 *
 * Pure and DOM-free on purpose: the host component owns the pointer events and
 * the recorder, this owns only the decision, so it is unit-testable without
 * mocking getUserMedia/MediaRecorder. Timers are injectable for the same reason.
 *
 * Click suppression: after a hold, the browser still emits a `click` on release.
 * The host must ask `shouldSuppressClick()` in its click handler and bail when it
 * returns true, otherwise the release would also fire the tap-toggle and
 * immediately restart the recording the user just ended.
 */

/**
 * How long the button must be held before it counts as push-to-talk rather than
 * a click. Long enough that an ordinary (even slightly slow) click still reads as
 * a tap, short enough to feel instant when you mean to hold and talk.
 */
export const HOLD_THRESHOLD_MS = 250;

export interface PressGestureCallbacks {
	/** Hold passed the threshold — start capturing. */
	onHoldStart: () => void;
	/** Held capture ended (release, cancel, or pointer left the button). */
	onHoldEnd: () => void;
	/** Press+release inside the threshold — run the existing toggle. */
	onTap: () => void;
}

export interface PressGestureOptions {
	thresholdMs?: number;
	setTimeout?: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
	clearTimeout?: (handle: ReturnType<typeof setTimeout>) => void;
}

export interface PressGesture {
	/** pointerdown */
	down: () => void;
	/** pointerup */
	up: () => void;
	/** pointercancel / pointerleave — treated as a release of an active hold. */
	cancel: () => void;
	/** True while a held capture is running. */
	readonly isHolding: boolean;
	/** One-shot: true for the single click emitted after a hold. */
	shouldSuppressClick: () => boolean;
	/** Drop any armed timer (component teardown). */
	dispose: () => void;
}

export function createPressGesture(
	callbacks: PressGestureCallbacks,
	options: PressGestureOptions = {}
): PressGesture {
	const thresholdMs = options.thresholdMs ?? HOLD_THRESHOLD_MS;
	const arm = options.setTimeout ?? ((fn, ms) => setTimeout(fn, ms));
	const disarm = options.clearTimeout ?? ((handle) => clearTimeout(handle));

	let timer: ReturnType<typeof setTimeout> | null = null;
	let pressed = false;
	let holding = false;
	let suppressClick = false;

	function clearTimer(): void {
		if (timer !== null) {
			disarm(timer);
			timer = null;
		}
	}

	/** Finish an active hold exactly once; returns true if one was running. */
	function endHold(): boolean {
		if (!holding) return false;
		holding = false;
		suppressClick = true;
		callbacks.onHoldEnd();
		return true;
	}

	return {
		down(): void {
			if (pressed) return; // duplicate/re-entrant pointerdown
			pressed = true;
			clearTimer();
			timer = arm(() => {
				timer = null;
				if (!pressed) return;
				holding = true;
				callbacks.onHoldStart();
			}, thresholdMs);
		},
		up(): void {
			if (!pressed) return; // stray release
			pressed = false;
			clearTimer();
			if (endHold()) return;
			callbacks.onTap();
		},
		cancel(): void {
			if (!pressed) return;
			pressed = false;
			clearTimer();
			// A cancelled hold must still stop capture; a cancelled tap is a no-op
			// (the user slid off the button without committing to anything).
			endHold();
		},
		get isHolding(): boolean {
			return holding;
		},
		shouldSuppressClick(): boolean {
			const suppress = suppressClick;
			suppressClick = false;
			return suppress;
		},
		dispose(): void {
			clearTimer();
			pressed = false;
			holding = false;
			suppressClick = false;
		}
	};
}
