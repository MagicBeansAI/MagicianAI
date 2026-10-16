import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import HowCard from './HowCard.svelte';
import { HOW_CLAIMS } from './howTrack';
import { motionEnabled } from '$lib/motion';

// the card plays only on screen: report every observed card as visible
beforeEach(() => {
	vi.stubGlobal(
		'IntersectionObserver',
		class {
			constructor(private readonly callback: (entries: { isIntersecting: boolean }[]) => void) {}
			observe(): void {
				this.callback([{ isIntersecting: true }]);
			}
			disconnect(): void {}
		}
	);
});

afterEach(() => {
	cleanup();
	motionEnabled.set(true);
	vi.unstubAllGlobals();
});

describe('HowCard', () => {
	it("loads nothing until its station is pinned, then plays that station's loop", async () => {
		motionEnabled.set(true);
		const { container, rerender } = render(HowCard, { props: { id: 'relentless', label: 'Relentless', active: false } });
		const video = container.querySelector('video');
		expect(video).toHaveAttribute('poster', '/landing/how/relentless.webp');
		expect(video).not.toHaveAttribute('src');
		await rerender({ id: 'relentless', label: 'Relentless', active: true });
		expect(container.querySelector('video')).toHaveAttribute('src', '/landing/how/relentless.mp4');
	});

	it('shows the poster alone with reduced motion', () => {
		motionEnabled.set(false);
		const { container } = render(HowCard, { props: { id: 'yours', label: 'Yours', active: true } });
		expect(container.querySelector('video')).toBeNull();
		expect(container.querySelector('img')).toHaveAttribute('src', '/landing/how/yours.webp');
	});

	it('has a card for every station', () => {
		expect(HOW_CLAIMS.map((c) => c.id)).toEqual(['present', 'hands', 'learns', 'relentless', 'builds', 'agents', 'yours']);
	});
});
