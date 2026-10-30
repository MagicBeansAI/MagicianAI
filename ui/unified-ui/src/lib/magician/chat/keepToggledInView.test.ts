// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';

import { keepToggledInView, revealDelta } from './keepToggledInView';

const view = { top: 0, bottom: 600 };

describe('revealDelta', () => {
	it('leaves a card that fits alone', () => {
		expect(revealDelta({ top: 100, bottom: 400 }, view)).toBe(0);
	});

	it('scrolls down to show a card grown past the bottom', () => {
		expect(revealDelta({ top: 300, bottom: 800 }, view)).toBe(212);
	});

	it('never pushes the top of a tall card out of view', () => {
		expect(revealDelta({ top: 100, bottom: 1400 }, view)).toBe(88);
	});

	it('scrolls up to the top of a card collapsed above the view', () => {
		expect(revealDelta({ top: -250, bottom: 40 }, view)).toBe(-262);
	});
});

function nextFrame(): Promise<void> {
	return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

describe('keepToggledInView', () => {
	it('does not scroll a long answer when its verification popover opens', async () => {
		const container = document.createElement('div');
		container.style.overflowY = 'auto';
		Object.defineProperty(container, 'scrollHeight', { value: 2000 });
		Object.defineProperty(container, 'clientHeight', { value: 600 });
		container.getBoundingClientRect = () => ({ top: 0, bottom: 600 }) as DOMRect;
		const card = document.createElement('div');
		card.className = 'chat';
		card.getBoundingClientRect = () => ({ top: 300, bottom: 1400 }) as DOMRect;
		const toggle = document.createElement('button');
		toggle.setAttribute('aria-expanded', 'false');
		toggle.setAttribute('data-popover-trigger', '');
		card.append(toggle); container.append(card); document.body.append(container);
		const scrollBy = vi.fn();
		container.scrollBy = scrollBy as typeof container.scrollBy;
		const action = keepToggledInView(container);
		toggle.click();
		await nextFrame();
		expect(scrollBy).not.toHaveBeenCalled();
		action.destroy(); container.remove();
	});

	it('nudges the scroller when a disclosure inside a card is clicked', async () => {
		const container = document.createElement('div');
		const card = document.createElement('div');
		card.className = 'chat-rich-file-block';
		const toggle = document.createElement('button');
		toggle.setAttribute('data-toggles-content', '');
		toggle.textContent = 'Show preview';
		card.appendChild(toggle);
		const plain = document.createElement('button');
		plain.textContent = 'Plain';
		container.append(card, plain);
		document.body.appendChild(container);

		container.getBoundingClientRect = () => ({ top: 0, bottom: 600 }) as DOMRect;
		container.style.overflowY = 'auto';
		Object.defineProperty(container, 'scrollHeight', { value: 2000, configurable: true });
		Object.defineProperty(container, 'clientHeight', { value: 600, configurable: true });
		card.getBoundingClientRect = () => ({ top: 300, bottom: 800 }) as DOMRect;
		const scrollBy = vi.fn();
		container.scrollBy = scrollBy as typeof container.scrollBy;

		const action = keepToggledInView(container);
		toggle.click();
		await nextFrame();
		expect(scrollBy).toHaveBeenCalledWith({ top: 212 });

		// A fresh helper: a plain button is not a disclosure, so no follow.
		action.destroy();
		scrollBy.mockClear();
		const fresh = keepToggledInView(container);
		plain.click();
		await nextFrame();
		expect(scrollBy).not.toHaveBeenCalled();

		fresh.destroy();
		container.remove();
	});

	it('falls back to the page when no ancestor scrolls', async () => {
		const container = document.createElement('div');
		const card = document.createElement('div');
		card.className = 'chat-rich-file-block';
		const toggle = document.createElement('button');
		toggle.setAttribute('aria-expanded', 'false');
		card.appendChild(toggle);
		container.appendChild(card);
		document.body.appendChild(container);
		Object.defineProperty(window, 'innerHeight', { value: 600, configurable: true });
		card.getBoundingClientRect = () => ({ top: 300, bottom: 800 }) as DOMRect;
		const page = document.scrollingElement ?? document.documentElement;
		const scrollBy = vi.fn();
		page.scrollBy = scrollBy as typeof page.scrollBy;

		const action = keepToggledInView(container);
		toggle.click();
		await nextFrame();
		expect(scrollBy).toHaveBeenCalledWith({ top: 212 });
		action.destroy();
		container.remove();
	});

	it('keeps adjusting as the card grows in steps after the click', async () => {
		const container = document.createElement('div');
		const card = document.createElement('div');
		card.className = 'chat-rich-file-block';
		const toggle = document.createElement('button');
		toggle.setAttribute('data-toggles-content', '');
		card.appendChild(toggle);
		container.appendChild(card);
		document.body.appendChild(container);
		container.style.overflowY = 'auto';
		Object.defineProperty(container, 'scrollHeight', { value: 2000, configurable: true });
		Object.defineProperty(container, 'clientHeight', { value: 600, configurable: true });
		container.getBoundingClientRect = () => ({ top: 0, bottom: 600 }) as DOMRect;
		// Loading line first (fits), real content a frame later (overflows).
		let rect = { top: 400, bottom: 560 };
		card.getBoundingClientRect = () => rect as DOMRect;
		const scrollBy = vi.fn();
		container.scrollBy = scrollBy as typeof container.scrollBy;

		const action = keepToggledInView(container);
		toggle.click();
		await nextFrame();
		await nextFrame();
		expect(scrollBy).not.toHaveBeenCalled();
		rect = { top: 400, bottom: 900 };
		await nextFrame();
		await nextFrame();
		expect(scrollBy).toHaveBeenCalledWith({ top: 312 });
		action.destroy();
		container.remove();
	});

	it('stops following when the user scrolls', async () => {
		const container = document.createElement('div');
		const card = document.createElement('div');
		card.className = 'chat-rich-file-block';
		const toggle = document.createElement('button');
		toggle.setAttribute('aria-expanded', 'false');
		card.appendChild(toggle);
		container.appendChild(card);
		document.body.appendChild(container);
		container.style.overflowY = 'auto';
		Object.defineProperty(container, 'scrollHeight', { value: 2000, configurable: true });
		Object.defineProperty(container, 'clientHeight', { value: 600, configurable: true });
		container.getBoundingClientRect = () => ({ top: 0, bottom: 600 }) as DOMRect;
		let rect = { top: 400, bottom: 560 };
		card.getBoundingClientRect = () => rect as DOMRect;
		const scrollBy = vi.fn();
		container.scrollBy = scrollBy as typeof container.scrollBy;

		const action = keepToggledInView(container);
		toggle.click();
		await nextFrame();
		window.dispatchEvent(new WheelEvent('wheel'));
		rect = { top: 400, bottom: 900 };
		await nextFrame();
		await nextFrame();
		expect(scrollBy).not.toHaveBeenCalled();
		action.destroy();
		container.remove();
	});

	it('follows the card even when the toggle replaces itself on click', async () => {
		const container = document.createElement('div');
		const card = document.createElement('div');
		card.className = 'chat-rich-file-block';
		const toggle = document.createElement('button');
		toggle.setAttribute('data-toggles-content', '');
		// Like "Show preview" → "Hide preview": the clicked button leaves the DOM
		// in its own handler, before the click bubbles to the container.
		toggle.addEventListener('click', () => {
			const next = document.createElement('button');
			next.setAttribute('data-toggles-content', '');
			toggle.replaceWith(next);
		});
		card.appendChild(toggle);
		container.appendChild(card);
		document.body.appendChild(container);
		container.style.overflowY = 'auto';
		Object.defineProperty(container, 'scrollHeight', { value: 2000, configurable: true });
		Object.defineProperty(container, 'clientHeight', { value: 600, configurable: true });
		container.getBoundingClientRect = () => ({ top: 0, bottom: 600 }) as DOMRect;
		card.getBoundingClientRect = () => ({ top: 300, bottom: 800 }) as DOMRect;
		const scrollBy = vi.fn();
		container.scrollBy = scrollBy as typeof container.scrollBy;

		const action = keepToggledInView(container);
		toggle.click();
		await nextFrame();
		expect(scrollBy).toHaveBeenCalledWith({ top: 212 });
		action.destroy();
		container.remove();
	});
});
