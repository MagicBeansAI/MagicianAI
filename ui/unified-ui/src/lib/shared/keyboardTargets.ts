/**
 * Shared checks for global keyboard handlers.
 *
 * - `isEditableTarget(target)`: the key is going into a text field
 *   (input, textarea, select, contenteditable). Plain-key shortcuts must not
 *   fire there.
 * - `isInExclusiveKeyboardScope(target)`: the key comes from inside a surface
 *   that owns every key while it has focus — a modal text prompt such as the
 *   "New thread" name dialog. Mark the surface's root with
 *   `data-keyboard-exclusive`; global handlers, including modifier chords
 *   and the Escape coordinator, then leave its keys alone.
 * - `ignoresGlobalShortcuts(event)`: either of the above.
 */

export const EXCLUSIVE_KEYBOARD_SCOPE_ATTRIBUTE = 'data-keyboard-exclusive';

function asElement(target: EventTarget | null | undefined): Element | null {
	return typeof Element !== 'undefined' && target instanceof Element ? target : null;
}

export function isEditableTarget(target: EventTarget | null | undefined): boolean {
	const element = asElement(target);
	if (!element) return false;
	if (element instanceof HTMLElement && element.isContentEditable) return true;
	return Boolean(
		element.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]')
	);
}

export function isInExclusiveKeyboardScope(target: EventTarget | null | undefined): boolean {
	return Boolean(asElement(target)?.closest(`[${EXCLUSIVE_KEYBOARD_SCOPE_ATTRIBUTE}]`));
}

export function ignoresGlobalShortcuts(event: Pick<KeyboardEvent, 'target'>): boolean {
	return isEditableTarget(event.target) || isInExclusiveKeyboardScope(event.target);
}
