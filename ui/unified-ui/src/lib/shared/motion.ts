/**
 * JS-side companions to the CSS motion grammar (app.css --spring-pop / --ease-settle).
 *
 *   --spring-pop  = things that ARRIVE → use `arrive` transition / backOut
 *   --ease-settle = things that LAND   → use `settleIn`/`settleOut` / cubicOut
 *
 * Everything collapses to duration 0 under prefers-reduced-motion — even when
 * callers pass their own params. Caller params override the defaults (y, easing,
 * delay, …) but never the reduced-motion guard: `guard()` is applied AFTER the
 * params merge so reduced motion always wins.
 *
 * Reduced-motion state comes from the shared `motionEnabled` store in
 * `$lib/motion` (live matchMedia listener, SSR-safe via `$app/environment`)
 * rather than re-reading the media query here.
 */
import { get } from 'svelte/store';
import { cubicOut, backOut } from 'svelte/easing';
import { fly, type FlyParams } from 'svelte/transition';
import { motionEnabled } from '$lib/motion';

/** True when the user prefers reduced motion. Returns false during SSR. */
export const reducedMotion = (): boolean => !get(motionEnabled);

/** Duration for `animate:flip` — 0 when reduced motion. */
export const flipDurationMs = (base = 220): number => (reducedMotion() ? 0 : base);

/** Neutralise a merged transition config under reduced motion (duration AND delay). */
const guard = (params: FlyParams): FlyParams =>
	reducedMotion() ? { ...params, duration: 0, delay: 0 } : params;

/** Rows/cards LANDING into a list (--ease-settle verb). */
export function settleIn(node: Element, params: FlyParams = {}) {
	return fly(node, guard({ y: 8, duration: 240, easing: cubicOut, ...params }));
}

/**
 * Rows LEAVING (dismiss/snooze) — slides toward the action's meaning
 * (--ease-settle verb). Default is rightward; pass `x: -24` (or a custom
 * axis) to match the gesture. Arrivals exit via this too — no `arriveOut`.
 */
export function settleOut(node: Element, params: FlyParams = {}) {
	return fly(node, guard({ x: 24, duration: 180, easing: cubicOut, ...params }));
}

/** Things that ARRIVE with intent (new task card, toast) (--spring-pop verb). */
export function arrive(node: Element, params: FlyParams = {}) {
	return fly(node, guard({ y: 10, duration: 260, easing: backOut, ...params }));
}
