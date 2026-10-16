import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import LandingChrome from './LandingChrome.svelte';

afterEach(cleanup);

describe('LandingChrome', () => {
	it('keeps the brand, manifesto and access destinations available', () => {
		render(LandingChrome, { actionLabel: 'Try Magican', actionHref: '/chat' });
		expect(screen.getByRole('link', { name: 'Magican home' })).toHaveAttribute('href', '#landing-hero');
		expect(screen.queryByRole('link', { name: 'In action' })).not.toBeInTheDocument();
		expect(screen.getByRole('link', { name: 'How it works' })).toHaveAttribute('href', 'https://explain.magican.ai');
		expect(screen.getByRole('link', { name: 'Magican on GitHub (opens in a new tab)' })).toHaveAttribute('href', 'https://github.com/MagicBeansAI/MagicianAI');
		expect(screen.getByRole('link', { name: 'Manifesto' })).toHaveAttribute('href', '/manifesto');
		expect(screen.getByRole('link', { name: 'Try Magican' })).toHaveAttribute('href', '/chat');
	});

	it('requests a local appearance change and reflects the supplied state', async () => {
		const change = vi.fn();
		const { rerender } = render(LandingChrome, { events: { appearance: change } });
		const night = screen.getByRole('button', { name: 'Switch landing to night' });
		expect(night.textContent?.trim()).toBe('');
		expect(night.querySelector('svg')).toHaveAttribute('aria-hidden', 'true');
		await fireEvent.click(night);
		expect(change).toHaveBeenCalledOnce();
		await rerender({ dark: true });
		expect(screen.getByRole('button', { name: 'Switch landing to day' })).toHaveAttribute('aria-pressed', 'true');
	});
});
