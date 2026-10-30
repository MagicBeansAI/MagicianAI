import { afterEach, describe, expect, it, vi } from 'vitest';
import { anchorPopup } from './anchorPopup';

afterEach(() => {
	vi.unstubAllGlobals();
	document.body.replaceChildren();
});

describe('composer popup anchoring', () => {
	it('follows its trigger when the composer grows and stops observing on close', () => {
		let resized = () => {};
		const disconnect = vi.fn();
		vi.stubGlobal('ResizeObserver', class {
			constructor(callback: () => void) { resized = callback; }
			observe = vi.fn();
			disconnect = disconnect;
		});
		const trigger = document.createElement('button');
		const popup = document.createElement('div');
		document.body.append(trigger, popup);
		let top = 600;
		vi.spyOn(trigger, 'getBoundingClientRect').mockImplementation(() => ({ top, right: 900 } as DOMRect));
		vi.spyOn(popup, 'getBoundingClientRect').mockReturnValue({ width: 352 } as DOMRect);
		const action = anchorPopup(popup, () => trigger);
		expect(popup.style.right).toBe(`${window.innerWidth - 900}px`);
		expect(popup.style.bottom).toBe(`${window.innerHeight - 600 + 6}px`);
		top = 500;
		resized();
		expect(popup.style.bottom).toBe(`${window.innerHeight - 500 + 6}px`);
		expect(popup.style.maxHeight).toBe('486px');
		action.destroy();
		expect(disconnect).toHaveBeenCalledOnce();
		top = 400;
		window.dispatchEvent(new Event('resize'));
		expect(popup.style.bottom).toBe(`${window.innerHeight - 500 + 6}px`);
	});

	it('keeps the popup inside the viewport near either edge', () => {
		const trigger = document.createElement('button');
		const popup = document.createElement('div');
		let right = 20;
		vi.spyOn(trigger, 'getBoundingClientRect').mockImplementation(() => ({ top: 300, right } as DOMRect));
		vi.spyOn(popup, 'getBoundingClientRect').mockReturnValue({ width: 352 } as DOMRect);
		const action = anchorPopup(popup, () => trigger);
		expect(popup.style.right).toBe(`${window.innerWidth - 352 - 8}px`);
		right = window.innerWidth + 100;
		window.dispatchEvent(new Event('resize'));
		expect(popup.style.right).toBe('8px');
		action.destroy();
	});

	it('opens header popups below the trigger and flips above near the bottom', () => {
		const trigger = document.createElement('button');
		const popup = document.createElement('div');
		let top = 40;
		let left = 8;
		vi.spyOn(trigger, 'getBoundingClientRect').mockImplementation(() => ({ top, bottom: top + 32, left, right: left + 60 } as DOMRect));
		vi.spyOn(popup, 'getBoundingClientRect').mockReturnValue({ width: 352 } as DOMRect);
		const action = anchorPopup(popup, { getAnchor: () => trigger, placement: 'bottom', align: 'start' });
		expect(popup.style.position).toBe('fixed');
		expect(popup.style.top).toBe('78px');
		expect(popup.style.bottom).toBe('auto');
		expect(popup.style.right).toBe(`${window.innerWidth - 352 - 8}px`);
		top = window.innerHeight - 50;
		left = window.innerWidth - 30;
		window.dispatchEvent(new Event('resize'));
		expect(popup.style.top).toBe('auto');
		expect(popup.style.bottom).toBe('56px');
		expect(popup.style.right).toBe('8px');
		action.destroy();
	});
});
