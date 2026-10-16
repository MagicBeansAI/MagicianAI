import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import TrustReveal from './TrustReveal.svelte';

afterEach(cleanup);

describe('TrustReveal', () => {
	// The three promises used to open this component as static pillars —
	// "Knows you", "Acts for you", "Belongs to you" — inside a region called
	// "The relationship". The promise ROAD now spends nine scroll-scrubbed
	// stations making exactly those three arguments, each titled in turn,
	// immediately above this. Restating them here asked the visitor to read
	// the conclusion of something they had just watched, so the region went
	// and only the part the road does not cover remains: who it answers to.
	it('does not restate the three promises the road just made', () => {
		render(TrustReveal);

		expect(screen.getByRole('region', { name: 'Trust' })).toBeTruthy();
		expect(screen.queryByRole('region', { name: 'The relationship' })).toBeNull();
		for (const promise of ['Knows you', 'Acts for you', 'Belongs to you']) {
			expect(screen.queryByRole('heading', { name: promise })).toBeNull();
		}
	});

	it('keeps the three mechanisms the road has no room for', () => {
		render(TrustReveal);

		expect(screen.getByRole('heading', { name: 'The vault' })).toBeTruthy();
		expect(screen.getByRole('heading', { name: 'Models you choose' })).toBeTruthy();
		expect(screen.getByRole('heading', { name: 'Your hand on the wheel' })).toBeTruthy();
	});

	it('makes personal power—not restoration—the trust question', () => {
		const { container } = render(TrustReveal);

		expect(container.textContent).toContain('Power this personal must answer to you.');
		expect(container.textContent).not.toContain('Yours again');
	});
});
