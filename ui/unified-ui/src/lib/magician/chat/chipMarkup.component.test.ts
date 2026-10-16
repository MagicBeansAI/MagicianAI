import { describe, expect, it } from 'vitest';

import { serializeChipDom } from './chipMarkup';

/**
 * DOM-backed serializer cases (jsdom — the unit project runs in `node`).
 *
 * The composer's placeholder is driven by `showPlaceholder = !value.length`, so
 * anything that makes a visually-empty editor serialize to a non-empty string
 * silently kills the placeholder.
 */
type Part = string | 'br' | { block: Part[] };

/** Build an editor's children explicitly — no innerHTML. */
function appendParts(parent: HTMLElement, parts: Part[]): void {
	for (const part of parts) {
		if (part === 'br') {
			parent.appendChild(document.createElement('br'));
		} else if (typeof part === 'string') {
			parent.appendChild(document.createTextNode(part));
		} else {
			const block = document.createElement('div');
			appendParts(block, part.block);
			parent.appendChild(block);
		}
	}
}

function editor(...parts: Part[]): HTMLElement {
	const root = document.createElement('div');
	appendParts(root, parts);
	return root;
}

describe('serializeChipDom line breaks', () => {
	it('treats a cleared editor as empty so the placeholder returns', () => {
		// Regression: typing then deleting everything leaves the browser's
		// trailing "bogus" <br>. Serializing it as "\n" made `value` truthy, so
		// the placeholder never came back after the first edit.
		expect(serializeChipDom(editor('br'))).toBe('');
	});

	it('drops only the trailing break after real text', () => {
		expect(serializeChipDom(editor('hello', 'br'))).toBe('hello');
	});

	it('keeps a genuine trailing newline, which needs two breaks to render', () => {
		expect(serializeChipDom(editor('hello', 'br', 'br'))).toBe('hello\n');
	});

	it('keeps interior newlines untouched', () => {
		expect(serializeChipDom(editor('one', 'br', 'two'))).toBe('one\ntwo');
		expect(serializeChipDom(editor('one', 'br', 'two', 'br'))).toBe('one\ntwo');
	});

	it('leaves text-only content alone', () => {
		expect(serializeChipDom(editor('hello'))).toBe('hello');
		expect(serializeChipDom(editor())).toBe('');
	});

	it('unwraps the bogus break nested in the last block wrapper', () => {
		// Chrome wraps Enter-newlines in <div>, so the bogus break can sit
		// inside the final block rather than as a direct child of the editor.
		expect(serializeChipDom(editor('one', { block: ['two', 'br'] }))).toBe('one\ntwo');
	});
});
