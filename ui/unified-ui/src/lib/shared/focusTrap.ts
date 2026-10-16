/**
 * Svelte action: trap Tab/Shift+Tab focus inside a modal-like node.
 *
 * THE shared focus-trap primitive for dialogs/sheets — mount it on the
 * dialog element (typically inside an `{#if open}` block so the action's
 * lifecycle mirrors the modal's):
 *
 *   <div role="dialog" tabindex="-1" use:focusTrap>
 *
 * Behavior:
 * - On activation, remembers the previously-focused element (the invoker)
 *   and moves focus to the first focusable descendant (falling back to the
 *   node itself, which is why the host should carry `tabindex="-1"`).
 * - Tab on the last focusable wraps to the first; Shift+Tab on the first
 *   wraps to the last. No other keys are intercepted — Escape-close and
 *   arrow-key handling stay with the host.
 * - On deactivation/destroy, returns focus to `returnFocusTo` (if given)
 *   or the remembered invoker, so keyboard users land back where they
 *   opened the modal from.
 */
export interface FocusTrapOptions {
	/** Trap engaged? Defaults to true (mount-with-the-modal usage). */
	active?: boolean;
	/**
	 * Element to refocus when the trap disengages. Defaults to whatever
	 * had focus at activation time (the invoker).
	 */
	returnFocusTo?: HTMLElement | null;
}

const FOCUSABLE_SELECTOR = [
	'a[href]',
	'button:not(:disabled)',
	'input:not(:disabled)',
	'select:not(:disabled)',
	'textarea:not(:disabled)',
	'[tabindex]:not([tabindex="-1"])'
].join(', ');

/** Visible, enabled focus candidates inside `node`, in DOM order. */
function focusableIn(node: HTMLElement): HTMLElement[] {
	return Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
		// getClientRects() is empty for display:none / detached elements —
		// a cheap visibility gate without style recomputation per element.
		// tabindex="-1" excludes programmatically-focusable-only elements
		// (e.g. scrim buttons) from the trap's tab stops.
		(el) => el.getClientRects().length > 0 && el.tabIndex !== -1
	);
}

export function focusTrap(node: HTMLElement, options: FocusTrapOptions = {}) {
	let current: FocusTrapOptions = options;
	let engaged = false;
	let previouslyFocused: HTMLElement | null = null;
	let initialFocusFrame = 0;

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key !== 'Tab') return;
		const focusable = focusableIn(node);
		if (focusable.length === 0) {
			// Nothing tabbable inside — keep focus parked on the host.
			event.preventDefault();
			node.focus();
			return;
		}
		const first = focusable[0];
		const last = focusable[focusable.length - 1];
		const active = document.activeElement as HTMLElement | null;
		const insideIndex = active ? focusable.indexOf(active) : -1;
		if (event.shiftKey) {
			if (insideIndex <= 0) {
				event.preventDefault();
				last.focus();
			}
		} else if (insideIndex === -1 || insideIndex === focusable.length - 1) {
			event.preventDefault();
			first.focus();
		}
	}

	function engage(): void {
		if (engaged) return;
		engaged = true;
		previouslyFocused =
			document.activeElement instanceof HTMLElement ? document.activeElement : null;
		node.addEventListener('keydown', handleKeydown);
		// Defer initial focus one frame so child components mounting after
		// the action (async modal bodies) don't race the focus move.
		initialFocusFrame = requestAnimationFrame(() => {
			const target = focusableIn(node)[0] ?? node;
			target.focus();
		});
	}

	function disengage(): void {
		if (!engaged) return;
		engaged = false;
		cancelAnimationFrame(initialFocusFrame);
		node.removeEventListener('keydown', handleKeydown);
		const restore = current.returnFocusTo ?? previouslyFocused;
		previouslyFocused = null;
		if (restore && restore.isConnected) restore.focus();
	}

	if (current.active !== false) engage();

	return {
		update(next: FocusTrapOptions) {
			current = next;
			if (current.active !== false) engage();
			else disengage();
		},
		destroy() {
			disengage();
		}
	};
}
