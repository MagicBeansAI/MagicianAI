import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { tick } from 'svelte';

import HeroSplit from './HeroSplit.svelte';
import { holdMsFor, VERB_COLORS } from './lifeCycle';

function hexToRgb(hex: string): string {
	const n = hex.replace('#', '');
	const r = parseInt(n.slice(0, 2), 16);
	const g = parseInt(n.slice(2, 4), 16);
	const b = parseInt(n.slice(4, 6), 16);
	return `rgb(${r}, ${g}, ${b})`;
}
import { heroPhase } from './landingPhase';
import { motionEnabled } from '$lib/motion';

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
	vi.useRealTimers();
});

function verbEl(container: HTMLElement): HTMLElement | null {
	const nodes = [...container.querySelectorAll('.hs-verb')];
	return (nodes.at(-1) as HTMLElement | undefined) ?? null;
}

function verbText(container: HTMLElement): string {
	return verbEl(container)?.textContent?.trim() ?? '';
}

describe('HeroSplit', () => {
	it('renders the lowercase magican wordmark as the h1', () => {
		// Lowercase on purpose, and not a typo to "correct" back. The hero h1 is
		// the wordmark, and `43fe3814f6` lowercased it alongside the two other
		// rendered wordmarks — LandingChrome's corner mark and LifeDevice's app
		// name. Prose keeps the capital: the <title>, the manifesto signature,
		// and the "Tell Magican what…" placeholder all still assert `Magican`,
		// and those tests are correct as they stand.
		render(HeroSplit);

		expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('magican');
	});

	it('renders the Superpowers line and the first activity immediately', () => {
		const { container } = render(HeroSplit);

		expect(screen.getByText('Superpowers for Work, Play and')).toBeInTheDocument();
		expect(verbText(container)).toBe('singing');
		expect(screen.getByText('singing')).toBeInTheDocument();
	});

	it('colours the cycling verb from the persona palette', () => {
		const { container } = render(HeroSplit);
		expect(verbEl(container)?.style.color).toBe(hexToRgb(VERB_COLORS[0]));
	});

	it('does not render the retired BrandReveal lockup or the Lottie figure', () => {
		const { container } = render(HeroSplit);
		const text = container.textContent ?? '';

		expect(text).not.toContain('for the');
		expect(text).not.toContain('in you');
		expect(text).not.toContain('Personal Superintelligence');
		expect(container.querySelector('.life-portrait')).toBeNull();
	});

	it('stays on singing under reduced motion after the first hold would have elapsed', async () => {
		vi.useFakeTimers();
		motionEnabled.set(false);
		const { container } = render(HeroSplit);

		expect(verbText(container)).toBe('singing');
		await vi.advanceTimersByTimeAsync(2000);
		await tick();
		expect(verbText(container)).toBe('singing');
	});

	it('cycles to cooking after holdMsFor(0) when motion is on', async () => {
		vi.useFakeTimers();
		motionEnabled.set(true);
		const { container } = render(HeroSplit);

		expect(verbText(container)).toBe('singing');
		await vi.advanceTimersByTimeAsync(holdMsFor(0));
		await tick();
		if (verbText(container) !== 'cooking') {
			await vi.runOnlyPendingTimersAsync();
			await tick();
		}

		expect(verbText(container)).toBe('cooking');
		expect(verbEl(container)?.style.color).toBe(hexToRgb(VERB_COLORS[1]));
	});

	it('publishes heroPhase 0 on mount', () => {
		heroPhase.set(1);
		render(HeroSplit);

		expect(get(heroPhase)).toBe(0);
	});
});
