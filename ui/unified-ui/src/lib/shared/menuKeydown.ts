/**
 * Shared keyboard mechanics for `role="menu"` popups (task-card overflow
 * menu, export menu, Today snooze menu) and — via `itemSelector` —
 * listbox-flavored popups whose items are `role="option"` (chat profile
 * picker), so every popup behaves identically:
 *
 * - ArrowDown/ArrowUp move focus between item buttons with
 *   wrap-around; Home/End jump to the first/last item.
 * - Escape closes the menu and asks the caller to refocus the trigger
 *   (`close(true)`).
 * - Arrow keys originating inside form inputs are left alone so native
 *   value-stepping (date fields etc.) keeps working; Escape still closes.
 * - Tab intentionally does NOT close the menu — a deliberate, consistent
 *   choice across all menus: focus moves on naturally, and Escape or the
 *   shared outside-pointerdown (`clickOutside`) remain the close paths.
 */

export interface MenuKeydownOptions {
	/**
	 * Returns the menu container queried for enabled `role="menuitem"`
	 * elements. May return null while the menu is closed/unmounted.
	 */
	getMenuEl: () => HTMLElement | null;
	/**
	 * Close the menu. `refocusTrigger === true` asks the caller to move
	 * focus back to the trigger element (the Escape path).
	 */
	close: (refocusTrigger: boolean) => void;
	/**
	 * Selector for the focusable items inside the popup. Defaults to
	 * `role="menuitem"` buttons; listbox popups pass
	 * `'[role="option"]:not(:disabled)'` to get identical mechanics with
	 * option semantics.
	 */
	itemSelector?: string;
}

const DEFAULT_ITEM_SELECTOR = '[role="menuitem"]:not(:disabled)';

/** Enabled item elements inside `menuEl` (per `itemSelector`), in DOM order. */
export function menuFocusableItems(
	menuEl: HTMLElement | null,
	itemSelector: string = DEFAULT_ITEM_SELECTOR
): HTMLElement[] {
	if (!menuEl) return [];
	return Array.from(menuEl.querySelectorAll<HTMLElement>(itemSelector));
}

/** Build the shared `keydown` handler for a `role="menu"` popup. */
export function createMenuKeydown(options: MenuKeydownOptions): (event: KeyboardEvent) => void {
	return (event: KeyboardEvent): void => {
		if (event.key === 'Escape') {
			event.preventDefault();
			event.stopPropagation();
			options.close(true);
			return;
		}
		if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
		// Arrow keys inside inputs keep their native behavior (value stepping).
		if (event.target instanceof HTMLInputElement) return;
		const items = menuFocusableItems(options.getMenuEl(), options.itemSelector);
		if (items.length === 0) return;
		event.preventDefault();
		const active = document.activeElement as HTMLElement | null;
		const index = active ? items.indexOf(active) : -1;
		let next = 0;
		if (event.key === 'ArrowDown') next = index < 0 ? 0 : (index + 1) % items.length;
		else if (event.key === 'ArrowUp')
			next = index < 0 ? items.length - 1 : (index - 1 + items.length) % items.length;
		else if (event.key === 'End') next = items.length - 1;
		items[next]?.focus();
	};
}
