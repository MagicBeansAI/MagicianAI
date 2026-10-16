// The landing movie's scrubber — the house scroll pattern, extracted once.
//
// Every scroll-movie this app has shipped (ScrollPartner, ScrollErrand — both
// retired to legacy/landing-2026-08-04/) re-implemented the same three lines:
// a passive scroll listener coalesced through requestAnimationFrame, page
// progress as `-rect.top / (track.offsetHeight - viewportHeight)`, and a
// BOUNDS array that turns that one number into (scene, local). This module is
// that pattern stated once, with the math split from the DOM so the part that
// can be wrong quietly — the boundary arithmetic — is unit-testable without a
// window.
//
// Shape: `createScrubber(bounds)` returns a readable `state` store and a
// `track` Svelte action. The action owns the listeners and the rAF token; the
// store carries `{ p, scene, local }`. Components never touch scroll events.

import { readonly, writable, type Readable } from 'svelte/store';

export interface ScrubState {
	/** Whole-track progress, 0 at the track's top edge pinned, 1 at its end. */
	p: number;
	/** Index into the bounds intervals — which chapter owns the viewport. */
	scene: number;
	/** Progress WITHIN the owning chapter, 0..1. */
	local: number;
}

export function clamp(v: number, lo: number, hi: number): number {
	return Math.min(hi, Math.max(lo, v));
}

/**
 * Whole-track progress from geometry alone. `top` is the track's
 * getBoundingClientRect().top; the denominator is how far the track can
 * scroll while its sticky stage stays pinned. A degenerate track (shorter
 * than the viewport) reports 0 rather than NaN — the movie simply holds its
 * first frame.
 */
export function trackProgress(top: number, trackHeight: number, viewportHeight: number): number {
	const total = trackHeight - viewportHeight;
	if (total <= 0) return 0;
	return clamp(-top / total, 0, 1);
}

/**
 * Cumulative bounds from chapter weights. Weights are relative durations
 * (a chapter of weight 2 holds the viewport twice as long as weight 1);
 * the result always starts at 0 and ends at exactly 1, so the last chapter
 * cannot lose its tail to floating-point drift.
 */
export function boundsFromWeights(weights: readonly number[]): number[] {
	const total = weights.reduce((sum, w) => sum + w, 0);
	const bounds = [0];
	let acc = 0;
	for (const w of weights) {
		acc += w;
		bounds.push(acc / total);
	}
	bounds[bounds.length - 1] = 1;
	return bounds;
}

/**
 * The pure heart: which scene owns progress `p`, and how far through it.
 *
 * A boundary belongs to the scene it OPENS (p === bounds[n] gives scene n at
 * local 0), except the very end: p === 1 stays in the last scene at local 1
 * rather than indexing past the array. Empty or single-entry bounds mean
 * "one scene, local = p" — the degenerate movie is still a movie.
 */
export function resolveScene(
	bounds: readonly number[],
	p: number
): { scene: number; local: number } {
	const clamped = clamp(p, 0, 1);
	if (bounds.length < 2) return { scene: 0, local: clamped };
	let scene = 0;
	while (scene < bounds.length - 2 && clamped >= bounds[scene + 1]) scene += 1;
	const span = bounds[scene + 1] - bounds[scene];
	const local = span > 0 ? clamp((clamped - bounds[scene]) / span, 0, 1) : 1;
	return { scene, local };
}

export interface Scrubber {
	state: Readable<ScrubState>;
	/** Svelte action for the tall track element. */
	track: (node: HTMLElement) => { destroy(): void };
}

export type ScrubGeometry = 'sticky' | 'viewport';

/**
 * Progress while an ordinary, non-sticky section travels through its scroll
 * root: 0 when its leading edge enters at the bottom, 1 when its trailing
 * edge leaves at the top. This lets short transition beats remain compact
 * instead of manufacturing a viewport-high sticky stage just to get a useful
 * progress value.
 */
export function viewportProgress(top: number, elementHeight: number, viewportHeight: number): number {
	if (elementHeight <= 0 || viewportHeight <= 0) return 0;
	const total = viewportHeight + elementHeight;
	return clamp((viewportHeight - top) / total, 0, 1);
}

export function createScrubber(
	bounds: readonly number[],
	options: { geometry?: ScrubGeometry } = {}
): Scrubber {
	const state = writable<ScrubState>({ p: 0, scene: 0, local: 0 });
	const geometry = options.geometry ?? 'sticky';

	function track(node: HTMLElement): { destroy(): void } {
		let raf = 0;
		let ticking = false;

		// THE SCROLL ROOT IS WHATEVER ACTUALLY SCROLLS THIS TRACK. Usually
		// that is the window; inside a road's modal it is the dialog's own
		// scroll container. The geometry does not change either way — a
		// scroller's client box IS the viewport for anything inside it, and
		// `position: sticky` sticks to it — but SCROLL EVENTS DO NOT BUBBLE
		// from elements, so the listener has to be on the thing that moves.
		const root: HTMLElement | null = node.closest<HTMLElement>('[data-scrub-root]');
		const target: EventTarget = root ?? window;
		const viewportH = (): number => (root ? root.clientHeight : window.innerHeight);

		const update = (): void => {
			ticking = false;
			const rect = node.getBoundingClientRect();
			// The track's top is measured against the ROOT's own box, not the
			// document's: inside a scroller, a viewport-relative top is off by
			// wherever the scroller happens to sit on the page.
			const top = root ? rect.top - root.getBoundingClientRect().top : rect.top;
			const p =
				geometry === 'viewport'
					? viewportProgress(top, node.offsetHeight, viewportH())
					: trackProgress(top, node.offsetHeight, viewportH());
			const { scene, local } = resolveScene(bounds, p);
			state.set({ p, scene, local });
		};

		const onScroll = (): void => {
			// Coalesce through rAF — the house rule: scroll fires faster than
			// frames paint, and computing layout more than once per frame buys
			// nothing but jank.
			if (!ticking) {
				ticking = true;
				raf = requestAnimationFrame(update);
			}
		};

		target.addEventListener('scroll', onScroll, { passive: true });
		window.addEventListener('resize', onScroll, { passive: true });
		update();

		return {
			destroy() {
				target.removeEventListener('scroll', onScroll);
				window.removeEventListener('resize', onScroll);
				if (raf) cancelAnimationFrame(raf);
			}
		};
	}

	return { state: readonly(state), track };
}
