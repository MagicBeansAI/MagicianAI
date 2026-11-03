// @vitest-environment jsdom
import { afterEach, describe, expect, it } from 'vitest';

import {
	ignoresGlobalShortcuts,
	isEditableTarget,
	isInExclusiveKeyboardScope
} from './keyboardTargets';

function add<K extends keyof HTMLElementTagNameMap>(
	tag: K,
	parent: HTMLElement = document.body,
	attributes: Record<string, string> = {}
): HTMLElementTagNameMap[K] {
	const element = document.createElement(tag);
	for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, value);
	parent.appendChild(element);
	return element;
}

afterEach(() => {
	document.body.replaceChildren();
});

describe('keyboard targets', () => {
	it('treats text fields and contenteditable as editable', () => {
		const field = add('input');
		const area = add('textarea');
		const rich = add('span', add('div', document.body, { contenteditable: 'true' }));
		const button = add('button');
		expect(isEditableTarget(field)).toBe(true);
		expect(isEditableTarget(area)).toBe(true);
		expect(isEditableTarget(rich)).toBe(true);
		expect(isEditableTarget(button)).toBe(false);
		expect(isEditableTarget(null)).toBe(false);
	});

	it('detects keys from inside an exclusive keyboard scope', () => {
		const dialog = add('aside', document.body, { 'data-keyboard-exclusive': '' });
		const name = add('input', dialog);
		const create = add('button', dialog);
		const outside = add('button');
		expect(isInExclusiveKeyboardScope(create)).toBe(true);
		expect(isInExclusiveKeyboardScope(outside)).toBe(false);
		expect(ignoresGlobalShortcuts({ target: name })).toBe(true);
		expect(ignoresGlobalShortcuts({ target: outside })).toBe(false);
	});
});
