import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import LandingHero from './LandingHero.svelte';
import { ROLES } from './roleCycle';

afterEach(cleanup);

describe('LandingHero', () => {
	// The h1 hosts a role that cycles on an un-mocked setInterval, so its
	// textContent is not deterministic at mount time under a real timer —
	// asserting on it would make this suite flaky depending on how much wall
	// time elapses between render and assertion. The aria-label carries the
	// whole, stable proposition regardless of which role happens to be
	// showing, so that is the contract this test pins.
	it('states the whole-person hook once, stably, on the h1', () => {
		render(LandingHero);

		const heading = screen.getByRole('heading', { level: 1 });
		expect(heading).toHaveAttribute(
			'aria-label',
			`You are ${ROLES.join(', ')}. Your AI has only met one of them.`
		);
	});

	it('states the turn line beneath the h1', () => {
		const { container } = render(LandingHero);

		expect(container.querySelector('.lh-turn')?.textContent).toContain(
			'Your AI has only met one of them.'
		);
	});

	it('no longer states the retired category claim or the three promises', () => {
		const { container } = render(LandingHero);

		expect(container.textContent).not.toContain('Superintelligence');
		expect(container.textContent).not.toContain('Knows you');
		expect(container.textContent).not.toContain('Acts for you');
		expect(container.textContent).not.toContain('Belongs to you');
		expect(container.textContent).not.toContain('Magican learns your ways');
	});

	it('keeps only the real ask, reframed off work', () => {
		render(LandingHero);

		expect(screen.queryByRole('link', { name: /See the change/ })).not.toBeInTheDocument();
		expect(screen.queryByRole('link', { name: /Put it to work/ })).not.toBeInTheDocument();
		expect(screen.getByRole('link', { name: /See how/ })).toHaveAttribute(
			'href',
			'#landing-cta'
		);
	});

	// The motif SVG moved from inside the retired `+` glyph to the turn
	// line, which is now its positioned anchor — `.lh-plus` no longer exists
	// at all.
	it('keeps the aurora motif, re-anchored at the turn line', () => {
		const { container } = render(LandingHero);

		expect(container.querySelector('.lh-plus')).not.toBeInTheDocument();
		expect(container.querySelector('.lh-orbit')).not.toBeInTheDocument();
		const origin = container.querySelector('.lh-turn .lh-motif-origin');
		expect(origin).toBeInTheDocument();
		expect(origin?.querySelectorAll('path')).toHaveLength(2);
	});
});
