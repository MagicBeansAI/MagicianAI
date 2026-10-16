// Motion primitives — single source for all spring physics + transition
// presets across the app. Importing from one module keeps the feel of
// every page consistent: a button squish on / behaves the same as a
// button squish on /chat. Three named presets cover ~all UI motion.
//
// Usage:
//
//   import { spring } from '$lib/motion';
//   const lift = spring.snappy(0);   // a tweened/spring store you can drive
//
//   import { fadeUp, popIn, sheetIn } from '$lib/motion';
//   <div transition:fadeUp> ... </div>
//
// All presets respect prefers-reduced-motion via the `getMotionEnabled()`
// helper — components can guard their `transition:` directives with it.
//
// For list/arrival work, prefer the verb-grammar transitions in
// `$lib/shared/motion.ts` (settleIn/settleOut/arrive) — they carry a
// built-in reduced-motion guard, so call sites need no manual guarding.

import { spring as svelteSpring } from 'svelte/motion';
import { cubicOut, cubicIn, quintOut, quintInOut } from 'svelte/easing';
import type { TransitionConfig } from 'svelte/transition';

// ── Spring presets ───────────────────────────────────────────────────
//
// Three flavours, picked from interaction style:
//
// snappy  — buttons, toggles, chips, tabs. Quick settle, no overshoot.
// smooth  — modals, sheets, drawers. Eased glide, no bounce.
// bouncy  — celebrations, success states, idle drift. Visible overshoot.
//
// Wrap `spring()` so callers can `spring.snappy(initial)` without
// re-typing tuning every time.

export const spring = {
	snappy: <T>(initial: T) => svelteSpring(initial, { stiffness: 0.30, damping: 0.80 }),
	smooth: <T>(initial: T) => svelteSpring(initial, { stiffness: 0.15, damping: 0.70 }),
	bouncy: <T>(initial: T) => svelteSpring(initial, { stiffness: 0.40, damping: 0.50 })
};

// ── Reduced-motion guard ────────────────────────────────────────────
//
// Read once on mount, watch the media query so the runtime adapts if
// the user toggles the system preference. Components import the value
// of `motionEnabled` (a writable store) to decide whether to play
// transitions at all.

import { writable } from 'svelte/store';
import { browser } from '$app/environment';

export const motionEnabled = writable(true);

if (browser) {
	const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
	const sync = () => motionEnabled.set(!mq.matches);
	sync();
	mq.addEventListener('change', sync);
}

// ── Transition presets ──────────────────────────────────────────────
//
// Drop-in svelte transition functions. Each returns a TransitionConfig
// so components use them as `<div transition:fadeUp>`. The cap-on
// duration keeps things snappy on entry (200-280ms band).

export function fadeUp(node: Element, params: { y?: number; duration?: number; delay?: number } = {}): TransitionConfig {
	const { y = 12, duration = 240, delay = 0 } = params;
	return {
		delay,
		duration,
		easing: cubicOut,
		css: (t, u) => `
			opacity: ${t};
			transform: translateY(${u * y}px);
		`
	};
}

export function fadeDown(node: Element, params: { y?: number; duration?: number; delay?: number } = {}): TransitionConfig {
	const { y = 12, duration = 240, delay = 0 } = params;
	return {
		delay,
		duration,
		easing: cubicOut,
		css: (t, u) => `
			opacity: ${t};
			transform: translateY(${-u * y}px);
		`
	};
}

// Modal / sheet entrance — scale-up from 0.96 with fade.
export function popIn(node: Element, params: { duration?: number; from?: number } = {}): TransitionConfig {
	const { duration = 220, from = 0.96 } = params;
	return {
		duration,
		easing: cubicOut,
		css: (t, u) => `
			opacity: ${t};
			transform: scale(${from + (1 - from) * t});
		`
	};
}

// Modal exit — fade + slight scale-down (faster than entrance).
export function popOut(node: Element, params: { duration?: number; to?: number } = {}): TransitionConfig {
	const { duration = 160, to = 0.97 } = params;
	return {
		duration,
		easing: cubicIn,
		css: (t, u) => `
			opacity: ${t};
			transform: scale(${to + (1 - to) * t});
		`
	};
}

// Right-edge sheet (history drawer, side panel).
export function sheetIn(node: Element, params: { x?: number; duration?: number } = {}): TransitionConfig {
	const { x = 24, duration = 280 } = params;
	return {
		duration,
		easing: quintOut,
		css: (t, u) => `
			opacity: ${t};
			transform: translateX(${u * x}px);
		`
	};
}

// Skeleton placeholder shimmer — used by Skeleton component as a
// sweeping highlight gradient. Exposed here so any component can build
// a custom skeleton with the same animation.
export const SKELETON_SHIMMER_DURATION_MS = 1400;

// Standardised easings re-exported so callers don't need to dual-import.
export { cubicOut, cubicIn, quintOut, quintInOut };
