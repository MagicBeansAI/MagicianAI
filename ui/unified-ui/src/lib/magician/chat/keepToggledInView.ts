/**
 * Keep an expanding / collapsing card in view inside the chat scroller.
 *
 * One behaviour for every card and bubble instead of per-component scroll
 * code: a click on a disclosure control (anything with `aria-expanded`,
 * `aria-controls` or `data-toggles-content`) marks the card it belongs to,
 * and while that card keeps resizing — the transition, content that loads
 * after the click (a preview fetch) — the scroller is nudged so the card
 * stays visible:
 *
 * - grown past the bottom edge → scroll down, but never push its top out;
 * - shrunk with its top above the top edge (collapsed from far down a long
 *   preview) → scroll up to its top.
 */

/** Cards a toggle can belong to, nearest first. */
const SCOPE_SELECTOR = [
	'[data-reveal-scope]',
	'.chat-rich-file-block',
	// Task drawer cards: act sections and the Technical details card.
	'.tech-details',
	'.act',
	'.cn-live-task',
	'.chat-action-card-wrap',
	'.chat-bubble',
	'.chat'
].join(', ');

const TOGGLE_SELECTOR = 'button[aria-expanded], button[aria-controls], [data-toggles-content]';

/** How long a card is followed after the click (covers a preview fetch). */
const FOLLOW_MS = 3000;
/** Breathing room kept between the card and the scroller's edges. */
const EDGE_PAD = 12;

/** Pure scroll delta for one card rect against the scroller's rect. */
export function revealDelta(
	card: { top: number; bottom: number },
	view: { top: number; bottom: number },
	pad = EDGE_PAD
): number {
	const viewTop = view.top + pad;
	const viewBottom = view.bottom - pad;
	if (card.top < viewTop) return card.top - viewTop;
	if (card.bottom > viewBottom) {
		// Down only as far as the card's top allows.
		return Math.max(0, Math.min(card.bottom - viewBottom, card.top - viewTop));
	}
	return 0;
}

/**
 * The element that actually scrolls the card: the nearest ancestor with a
 * scrollable overflow and room to scroll, else the page. The transcript
 * container is not always it — in some layouts the page scrolls and the
 * container just grows, which made every nudge a silent no-op.
 */
function scrollerFor(card: Element): { el: Element; rect: () => { top: number; bottom: number } } {
	for (let node = card.parentElement; node; node = node.parentElement) {
		const { overflowY } = getComputedStyle(node);
		if ((overflowY === 'auto' || overflowY === 'scroll') && node.scrollHeight > node.clientHeight + 1) {
			const el = node;
			return { el, rect: () => el.getBoundingClientRect() };
		}
	}
	const page = document.scrollingElement ?? document.documentElement;
	return { el: page, rect: () => ({ top: 0, bottom: window.innerHeight }) };
}

export function keepToggledInView(container: HTMLElement): { destroy(): void } {
	let frame = 0;
	let stopAt = 0;
	let card: Element | null = null;

	function stop() {
		cancelAnimationFrame(frame);
		frame = 0;
		card = null;
		window.removeEventListener('wheel', stop, true);
		window.removeEventListener('touchmove', stop, true);
		window.removeEventListener('keydown', stop, true);
	}

	function adjust(target: Element) {
		if (!target.isConnected) return;
		const scroller = scrollerFor(target);
		const delta = revealDelta(target.getBoundingClientRect(), scroller.rect());
		// jsdom elements have no scrollBy. A real browser does, and the follow
		// still measures; only the nudge is skipped where the method is absent.
		if (delta !== 0 && typeof scroller.el.scrollBy === 'function') {
			scroller.el.scrollBy({ top: delta });
		}
	}

	// Re-measure every frame instead of waiting for resize notifications: a
	// preview grows in steps (loading line, then content) and the first nudge
	// is clamped by the scroll height of that moment, so a one-shot or
	// observer-driven adjust stopped short.
	function tickFollow() {
		if (!card) return;
		try {
			adjust(card);
		} catch {
			// A measurement that throws (jsdom's style proxy can, while a node
			// is going away) ends the follow. The nudge is not worth a loop.
			stop();
			return;
		}
		if (performance.now() < stopAt) frame = requestAnimationFrame(tickFollow);
		else stop();
	}

	function onClick(event: MouseEvent) {
		const target = event.target instanceof Element ? event.target : null;
		const toggle = target?.closest(TOGGLE_SELECTOR);
		if (!toggle || !container.contains(toggle)) return;
		// A popover does not resize its message. Following the whole answer
		// here makes the first info click unexpectedly scroll the transcript.
		if (toggle.hasAttribute('data-popover-trigger')) { stop(); return; }
		const scope = toggle.closest(SCOPE_SELECTOR);
		if (!scope) return;

		stop();
		card = scope;
		stopAt = performance.now() + FOLLOW_MS;
		// The user taking over the scroll ends the follow at once.
		window.addEventListener('wheel', stop, true);
		window.addEventListener('touchmove', stop, true);
		window.addEventListener('keydown', stop, true);
		frame = requestAnimationFrame(tickFollow);
	}

	// Capture phase: resolve the card before the toggle's own handler runs.
	// "Show preview" replaces itself with "Hide preview" in that handler, so by
	// the bubble phase the clicked button is detached and has no card.
	container.addEventListener('click', onClick, true);
	return {
		destroy() {
			container.removeEventListener('click', onClick, true);
			stop();
		}
	};
}
