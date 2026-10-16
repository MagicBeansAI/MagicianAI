import { act, cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import HeroSplit from './HeroSplit.svelte';
import { motionEnabled } from '$lib/motion';
import { HERO_EXAMPLE_INTERVAL_MS } from './heroExamples';

vi.mock('lottie-web', () => ({ default: { loadAnimation: vi.fn() } }));

function enableCycling(): void {
	vi.useFakeTimers();
	vi.stubGlobal('IntersectionObserver', undefined);
	vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false })));
	motionEnabled.set(true);
}

async function tick(ms = HERO_EXAMPLE_INTERVAL_MS): Promise<void> {
	await act(() => vi.advanceTimersByTime(ms));
}

function ask(): string {
	return document.querySelector('.hs-ask')?.textContent ?? '';
}

beforeEach(() => motionEnabled.set(false));
afterEach(() => {
	cleanup();
	motionEnabled.set(true);
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
	vi.useRealTimers();
});

describe('Bright Sidekick hero', () => {
	it('shows the concise brand hierarchy immediately with a static mascot', () => {
		const { container } = render(HeroSplit);
		expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('magican');
		expect(screen.getByText('More You')).toBeInTheDocument();
		expect(screen.getByText('Your personal superpowers')).toBeInTheDocument();
		expect(container.querySelector('img')).toHaveAttribute('src', '/brand/mascot-hi-light.svg');
	});

	it('links directly to the provided app or public access destination', async () => {
		const { rerender } = render(HeroSplit, {
			actionLabel: 'Try Magican', actionHref: '/chat'
		});
		expect(screen.getByRole('link', { name: 'Try Magican' })).toHaveAttribute('href', '/chat');
		await rerender({ actionLabel: 'Request setup', actionHref: 'mailto:reach.magican@gmail.com' });
		expect(screen.getByRole('link', { name: 'Request setup' })).toHaveAttribute('href', 'mailto:reach.magican@gmail.com');
	});

	it('changes example copy without submitting work or fetching under reduced motion', async () => {
		const fetch = vi.fn();
		vi.stubGlobal('fetch', fetch);
		render(HeroSplit);
		await fireEvent.click(screen.getByRole('button', { name: 'Play' }));
		expect(screen.getByText('“Help me learn this guitar piece.”')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Play' })).toHaveAttribute('aria-pressed', 'true');
		expect(screen.getByRole('button', { name: 'Work' })).toHaveAttribute('aria-pressed', 'false');
		await fireEvent.click(screen.getByRole('button', { name: 'Side quests' }));
		expect(screen.getByText('A follow-up, ready for your review.')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Next example' }));
		expect(ask()).toContain('Catch me up on my inbox. What actually needs me?');
		expect(screen.queryByRole('button', { name: 'Pause examples' })).not.toBeInTheDocument();
		expect(fetch).not.toHaveBeenCalled();
	});

	it('keeps one access CTA and the dark static fallback usable without animation', () => {
		const { container } = render(HeroSplit, { dark: true });
		expect(screen.queryByRole('link', { name: 'See it in action' })).not.toBeInTheDocument();
		expect(screen.getByRole('navigation', { name: 'Get started with Magican' }).querySelectorAll('a')).toHaveLength(1);
		expect(screen.getByRole('link', { name: 'Explore Magican' })).toHaveAttribute('href', '#landing-proof');
		expect(screen.getByRole('link', { name: /Watch the film/ })).toHaveAttribute('href', '#film');
		expect(container.querySelector('img')).toHaveAttribute('src', '/brand/mascot-hi-dark.svg');
	});

	it('cycles across all categories and 36 unique examples before wrapping', async () => {
		enableCycling();
		render(HeroSplit);
		const first = ask();
		await tick(HERO_EXAMPLE_INTERVAL_MS - 1);
		expect(ask()).toBe(first);
		await tick(1);
		expect(ask()).toContain('Help me learn this guitar piece.');
		expect(screen.getByRole('button', { name: 'Play' })).toHaveAttribute('aria-pressed', 'true');
		await tick();
		expect(ask()).toContain('Find my refund email');
		const seen = new Set([first, '“Help me learn this guitar piece.”', ask()]);
		for (let index = 3; index < 36; index++) {
			await tick();
			seen.add(ask());
		}
		expect(seen.size).toBe(36);
		await tick();
		expect(ask()).toBe(first);
		expect(screen.getByRole('status').textContent?.trim()).toBe('');
	});

	it('can pause, browse backwards, and resume from the current example', async () => {
		enableCycling();
		render(HeroSplit);
		await fireEvent.click(screen.getByRole('button', { name: 'Pause examples' }));
		const first = ask();
		await tick(HERO_EXAMPLE_INTERVAL_MS * 3);
		expect(ask()).toBe(first);
		await fireEvent.click(screen.getByRole('button', { name: 'Previous example' }));
		expect(ask()).toContain('find a time for dinner');
		expect(screen.getByRole('status')).toHaveTextContent('Side quests example.');
		await fireEvent.click(screen.getByRole('button', { name: 'Resume examples' }));
		await tick();
		expect(ask()).toBe(first);
	});

	it('restarts reading time after selecting a category and cleans up its timer', async () => {
		enableCycling();
		const { unmount } = render(HeroSplit);
		await tick(HERO_EXAMPLE_INTERVAL_MS - 1_000);
		await fireEvent.click(screen.getByRole('button', { name: 'Play' }));
		await tick(1_000);
		expect(ask()).toContain('Help me learn this guitar piece.');
		await tick(HERO_EXAMPLE_INTERVAL_MS - 1_000);
		expect(ask()).toContain('Find my refund email');
		unmount();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('holds while hovered, focused or hidden, then resumes with a full interval', async () => {
		enableCycling();
		const { container } = render(HeroSplit);
		const strip = container.querySelector('.hs-example')!;
		const first = ask();
		await fireEvent.pointerEnter(strip, { pointerType: 'mouse' });
		await tick();
		expect(ask()).toBe(first);
		await fireEvent.pointerLeave(strip, { pointerType: 'mouse' });
		const work = screen.getByRole('button', { name: 'Work' });
		// JSDOM does not model the browser's keyboard/pointer focus heuristic.
		const matches = work.matches.bind(work);
		vi.spyOn(work, 'matches').mockImplementation((selector) =>
			selector === ':focus-visible' || matches(selector)
		);
		await act(() => work.focus());
		await tick();
		expect(ask()).toBe(first);
		await act(() => work.blur());
		const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
		await fireEvent(document, new Event('visibilitychange'));
		await tick();
		expect(ask()).toBe(first);
		hidden.mockReturnValue(false);
		await fireEvent(document, new Event('visibilitychange'));
		await tick();
		expect(ask()).toContain('Help me learn this guitar piece.');
	});

	it('runs only when the example strip is visible and respects motion preference changes', async () => {
		enableCycling();
		let observeStrip: (visible: boolean) => void = () => {};
		vi.stubGlobal('IntersectionObserver', class {
			constructor(private callback: IntersectionObserverCallback) {}
			observe(target: Element): void {
				if (target.classList.contains('hs-example')) {
					observeStrip = (visible) => this.callback([
						{ isIntersecting: visible, intersectionRatio: visible ? 1 : 0 } as IntersectionObserverEntry
					], this as unknown as IntersectionObserver);
				}
			}
			disconnect(): void {}
		});
		render(HeroSplit);
		const first = ask();
		await tick();
		expect(ask()).toBe(first);
		await act(() => observeStrip(true));
		await tick();
		expect(ask()).toContain('Help me learn this guitar piece.');
		await act(() => observeStrip(false));
		await tick();
		expect(ask()).toContain('Help me learn this guitar piece.');
		await act(() => observeStrip(true));
		await act(() => motionEnabled.set(false));
		await tick();
		expect(ask()).toContain('Help me learn this guitar piece.');
		await act(() => motionEnabled.set(true));
		await tick();
		expect(ask()).toContain('Find my refund email');
	});
});
