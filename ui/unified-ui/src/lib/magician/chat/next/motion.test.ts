// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';

import { morph, readDuration, revealWords, rise } from './motion';

function stubReducedMotion(reduce: boolean) {
	vi.spyOn(window, 'matchMedia').mockImplementation(
		(query: string) =>
			({
				matches: reduce && query.includes('prefers-reduced-motion'),
				media: query,
				onchange: null,
				addListener: () => {},
				removeListener: () => {},
				addEventListener: () => {},
				removeEventListener: () => {},
				dispatchEvent: () => false
			}) as MediaQueryList
	);
}

function stubToken(values: Record<string, string>) {
	const real = window.getComputedStyle.bind(window);
	vi.spyOn(window, 'getComputedStyle').mockImplementation((el: Element) => {
		const style = real(el);
		return {
			...style,
			getPropertyValue: (name: string) => values[name] ?? ''
		} as CSSStyleDeclaration;
	});
}

const vtName = (el: HTMLElement) => el.style.getPropertyValue('view-transition-name');

afterEach(() => {
	vi.restoreAllMocks();
	document.body.innerHTML = '';
	delete (document as { startViewTransition?: unknown }).startViewTransition;
});

describe('readDuration', () => {
	it('parses ms and s values', () => {
		stubReducedMotion(false);
		stubToken({ '--dur-base': ' 280ms', '--dur-slow': '0.5s' });
		expect(readDuration('--dur-base')).toBe(280);
		expect(readDuration('--dur-slow')).toBe(500);
	});

	it('falls back to a default when the token is unreadable', () => {
		stubReducedMotion(false);
		stubToken({});
		expect(readDuration('--dur-quick')).toBe(160);
	});

	it('returns 0 under reduced motion', () => {
		stubReducedMotion(true);
		stubToken({ '--dur-base': '280ms' });
		expect(readDuration('--dur-base')).toBe(0);
	});
});

describe('rise', () => {
	it('has zero duration under reduced motion', () => {
		stubReducedMotion(true);
		expect(rise(document.createElement('div'))).toEqual({ duration: 0 });
	});

	it('fades from 0 to 1', () => {
		stubReducedMotion(false);
		stubToken({ '--dur-base': '280ms' });
		const config = rise(document.createElement('div'), { delay: 40 });
		expect(config.duration).toBe(280);
		expect(config.delay).toBe(40);
		expect(config.css?.(0, 1)).toContain('opacity: 0');
		expect(config.css?.(1, 0)).toContain('opacity: 1');
	});
});

describe('revealWords', () => {
	it('wraps words in groups of three and skips code', () => {
		stubReducedMotion(false);
		stubToken({ '--dur-quick': '160ms' });
		const node = document.createElement('div');
		node.innerHTML = '<p>one two three four five</p><code>keep me whole</code>';
		revealWords(node);
		const words = node.querySelectorAll('.cn-word');
		expect(words).toHaveLength(2);
		expect(words[0].textContent?.trim()).toBe('one two three');
		expect(words[1].textContent?.trim()).toBe('four five');
		expect((words[1] as HTMLElement).style.animationDelay).toBe('40ms');
		expect(node.querySelector('code')?.innerHTML).toBe('keep me whole');
		expect(node.textContent).toBe('one two three four fivekeep me whole');
	});

	it('is a no-op when disabled', () => {
		stubReducedMotion(false);
		stubToken({ '--dur-quick': '160ms' });
		const node = document.createElement('div');
		node.innerHTML = '<p>one two three four five</p>';
		revealWords(node, { enabled: false });
		expect(node.innerHTML).toBe('<p>one two three four five</p>');
	});

	it('is a no-op under reduced motion', () => {
		stubReducedMotion(true);
		const node = document.createElement('div');
		node.innerHTML = '<p>one two three four five</p>';
		revealWords(node);
		expect(node.querySelectorAll('.cn-word')).toHaveLength(0);
	});
});

describe('morph', () => {
	it('just runs the update without view transitions', async () => {
		stubReducedMotion(false);
		const update = vi.fn();
		await morph(document.createElement('div'), () => null, update);
		expect(update).toHaveBeenCalledTimes(1);
	});

	it('names source then target inside a view transition and clears both', async () => {
		stubReducedMotion(false);
		const source = document.createElement('div');
		const target = document.createElement('div');
		document.body.append(source);
		let sourceNameAtStart = '';
		let targetNameAfterCallback = '';
		const startViewTransition = vi.fn((cb: () => Promise<void>) => {
			sourceNameAtStart = vtName(source);
			const done = cb().then(() => {
				targetNameAfterCallback = vtName(target);
			});
			return { finished: done, updateCallbackDone: done, ready: done };
		});
		(document as unknown as { startViewTransition: unknown }).startViewTransition =
			startViewTransition;
		const update = vi.fn(() => {
			document.body.append(target);
		});

		await morph(source, () => target, update);

		expect(startViewTransition).toHaveBeenCalledTimes(1);
		expect(update).toHaveBeenCalledTimes(1);
		expect(sourceNameAtStart).toMatch(/^cn-morph-\d+$/);
		expect(targetNameAfterCallback).toBe(sourceNameAtStart);
		expect(vtName(source)).toBe('');
		expect(vtName(target)).toBe('');
	});

	it('falls back to a plain update under reduced motion', async () => {
		stubReducedMotion(true);
		const startViewTransition = vi.fn();
		(document as unknown as { startViewTransition: unknown }).startViewTransition =
			startViewTransition;
		const update = vi.fn();
		await morph(document.createElement('div'), () => null, update);
		expect(startViewTransition).not.toHaveBeenCalled();
		expect(update).toHaveBeenCalledTimes(1);
	});

	it('falls back when a transition is already running', async () => {
		stubReducedMotion(false);
		(document as unknown as { startViewTransition: unknown }).startViewTransition = () => {
			throw new DOMException('busy', 'InvalidStateError');
		};
		const source = document.createElement('div');
		const update = vi.fn();
		await morph(source, () => null, update);
		expect(update).toHaveBeenCalledTimes(1);
		expect(vtName(source)).toBe('');
	});
});
