// Scroll-driven arrival for the continuation's beats.
//
// The film pins one sticky stage and scrubs it (scrub.ts). The two roads that
// follow it are ordinary document flow — you keep scrolling and beats come at
// you — so they need the other half of the same grammar: each beat's own 0..1
// progress, published as `--local` so the film's reveal formulas
// (`opacity: clamp(0, calc((var(--local) - var(--i) * 0.1) * 5), 1)`) work here
// verbatim. No second engine: the film's scrubber is untouched.
//
// Same house rules as scrub.ts — the arithmetic is pure and unit-tested away
// from the DOM, and the listener is passive and coalesced through rAF.

import { clamp } from './scrub';

/**
 * A beat's own progress from geometry alone.
 *
 * `top` is the beat's getBoundingClientRect().top. It reads 0 while the beat's
 * top edge is still at (or below) the viewport's bottom, and 1 once that edge
 * has risen to `settle` of the viewport height — so a beat finishes arriving
 * comfortably before it is centred, and is fully built by the time it is the
 * thing you are reading. A degenerate viewport reports 1 rather than NaN: the
 * document renders whole rather than blank.
 */
export function beatLocal(top: number, viewportHeight: number, settle = 0.44): number {
	if (viewportHeight <= 0) return 1;
	const enter = viewportHeight;
	const done = viewportHeight * clamp(settle, 0, 0.98);
	return clamp((enter - top) / (enter - done), 0, 1);
}

/**
 * Svelte action for a road: one passive listener for all of its beats.
 *
 * Put it on the road's root. Every `[data-beat]` inside gets `--local` written
 * on it each frame. Passing `false` detaches and wipes the inline value, so
 * the CSS default of `--local: 1` stands and every beat renders finished —
 * the accessibility contract the film keeps too, and correct when the system
 * preference is flipped mid-session rather than only read at mount.
 */
export function revealBeats(
	node: HTMLElement,
	enabled = true
): { update(next: boolean): void; destroy(): void } {
	let raf = 0;
	let ticking = false;
	let attached = false;

	// THE SCROLL ROOT, same contract as `scrub`: whatever actually scrolls
	// these beats. On the page that is the window; inside a road's modal it is
	// the dialog's own scroller. This matters twice over — scroll events do
	// not bubble from elements, so a window listener never fires for a beat
	// in a dialog, AND the beat's top has to be read against the scroller's
	// box rather than the document's. Missed, the two roads simply stop
	// moving: every beat holds whatever `--local` it was born with.
	const root: HTMLElement | null = node.closest<HTMLElement>('[data-scrub-root]');
	const target: EventTarget = root ?? window;

	const update = (): void => {
		ticking = false;
		const vh = root ? root.clientHeight : window.innerHeight;
		const rootTop = root ? root.getBoundingClientRect().top : 0;
		for (const beat of node.querySelectorAll<HTMLElement>('[data-beat]')) {
			const top = beat.getBoundingClientRect().top - rootTop;
			beat.style.setProperty('--local', beatLocal(top, vh).toFixed(3));
		}
	};

	const onScroll = (): void => {
		if (!ticking) {
			ticking = true;
			raf = requestAnimationFrame(update);
		}
	};

	const attach = (): void => {
		if (attached) return;
		attached = true;
		target.addEventListener('scroll', onScroll, { passive: true });
		window.addEventListener('resize', onScroll, { passive: true });
		update();
	};

	const detach = (): void => {
		if (!attached) return;
		attached = false;
		target.removeEventListener('scroll', onScroll);
		window.removeEventListener('resize', onScroll);
		if (raf) cancelAnimationFrame(raf);
		for (const beat of node.querySelectorAll<HTMLElement>('[data-beat]')) {
			beat.style.removeProperty('--local');
		}
	};

	if (enabled) attach();

	return {
		update(next: boolean) {
			if (next) attach();
			else detach();
		},
		destroy() {
			detach();
		}
	};
}
