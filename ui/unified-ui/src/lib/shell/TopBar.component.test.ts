import { cleanup, render, screen, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { installBrowserTestPolyfills, installFetchMock, jsonResponse } from '../../test/browser';
import { hudWindow } from './hudWindow';
import TopBar from './TopBar.svelte';

vi.mock('$app/stores', async () => {
	const { readable } = await import('svelte/store');
	return { page: readable({ url: new URL('http://localhost/chat') }) };
});

vi.mock('$app/navigation', () => ({ goto: vi.fn() }));

beforeEach(() => {
	vi.clearAllMocks();
	hudWindow.set({ available: false, ready: false, expanded: false });
	installBrowserTestPolyfills();
	// Every backend read the bar's stores make answers empty; the nav itself
	// is static markup and must not depend on any of them.
	installFetchMock([
		{ method: 'GET', match: '/api/', handle: () => jsonResponse({}) },
		{ method: 'POST', match: '/api/', handle: () => jsonResponse({}) }
	]);
});

afterEach(() => {
	cleanup();
});

describe('TopBar primary navigation', () => {
	it('links the brand to /chat as the default page', () => {
		render(TopBar);
		const brandLink = screen.getByRole('link', { name: /magican/i });
		expect(brandLink).toHaveAttribute('href', '/chat');
	});

	it('carries Chat first, then Work, Observe, and Today last without VibeDev', () => {
		render(TopBar);

		const nav = screen.getByRole('navigation', { name: 'Primary' });
		const links = within(nav).getAllByRole('link');
		const labels = links.map((link) => link.textContent?.trim());
		expect(labels.slice(0, 4)).toEqual(['Chat', 'Work', 'Observe', 'Today']);
		expect(links[0]).toHaveClass('tab--chat');
		expect(nav.querySelector('a[href="/work"]')).not.toBeNull();
		expect(nav.querySelector('a[href="/vibe"]')).toBeNull();
		expect(labels).not.toContain('VibeDev');
		expect(labels).not.toContain('Tasks');
		expect(labels).not.toContain('Crew');
		expect(labels).not.toContain('Relentless');
	});

	it('places the native Compact action last, with an icon and accessible name', () => {
		hudWindow.set({ available: true, ready: true, expanded: true });
		const { container } = render(TopBar);
		const button = screen.getByRole('button', { name: 'Return to compact' });
		expect(button.textContent?.trim()).toBe('');
		expect(button.querySelector('svg')).not.toBeNull();
		expect(container.querySelector('.top-right')?.lastElementChild).toBe(button);
	});

	it('leaves only search and attention in the top right', () => {
		const { container } = render(TopBar);
		const topRight = container.querySelector('.top-right');
		expect(topRight).not.toBeNull();
		expect(topRight?.querySelector('.cmdk')).not.toBeNull();
		expect(topRight?.querySelector('.icon-btn--attention')).not.toBeNull();
		expect(topRight?.querySelector('.topbar__history')).toBeNull();
		expect(topRight?.querySelector('.theme-switcher')).toBeNull();
	});
});
