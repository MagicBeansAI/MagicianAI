/**
 * Svelte action: invoke a handler when a pointer goes down outside the node.
 *
 * THE shared click-outside primitive for popovers/menus — use this instead of
 * page-level `window.addEventListener('mousedown', ...)` + CSS-selector
 * allowlists. `exclude` skips elements whose clicks should NOT count as
 * outside (typically the menu's trigger, so a trigger click toggles instead
 * of close-then-reopen).
 *
 * Listens in the capture phase on `pointerdown` so it fires even when inner
 * handlers stop propagation, and works for mouse + touch + pen alike.
 */
export interface ClickOutsideOptions {
	handler: (event: PointerEvent) => void;
	exclude?: Array<Element | null | undefined>;
	/** Set false to detach without destroying the action. */
	enabled?: boolean;
}

export function clickOutside(node: HTMLElement, options: ClickOutsideOptions) {
	let current = options;

	function onPointerDown(event: PointerEvent): void {
		if (current.enabled === false) return;
		const target = event.target;
		if (!(target instanceof Node)) return;
		if (node.contains(target)) return;
		if (current.exclude?.some((el) => el != null && el.contains(target))) return;
		current.handler(event);
	}

	document.addEventListener('pointerdown', onPointerDown, true);

	return {
		update(next: ClickOutsideOptions) {
			current = next;
		},
		destroy() {
			document.removeEventListener('pointerdown', onPointerDown, true);
		}
	};
}
