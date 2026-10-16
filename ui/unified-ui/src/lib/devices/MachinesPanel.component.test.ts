import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import MachinesPanel from './MachinesPanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function stubMachines(body: unknown, status = 200) {
	const fetch = vi.fn(async () => new Response(JSON.stringify(body), { status }));
	vi.stubGlobal('fetch', fetch);
	return fetch;
}

describe('MachinesPanel', () => {
	it('marks this machine and links every other machine to its own UI host', async () => {
		const fetch = stubMachines({
			self: 'macbookpro',
			machines: [
				{
					name: 'macstudio',
					api_origin: 'https://macstudio.example.com',
					ui_origin: 'https://ui-macstudio.example.com',
					is_self: false
				},
				{
					name: 'macbookpro',
					api_origin: 'https://macbookpro.example.com',
					ui_origin: 'https://ui-macbookpro.example.com',
					is_self: true
				}
			]
		});
		render(MachinesPanel);

		expect(await screen.findByText('Machines · this is macbookpro')).toBeTruthy();
		expect(fetch).toHaveBeenCalledWith('/api/magician/v2/remote-machines', expect.anything());
		expect(screen.getByText('This machine')).toBeTruthy();
		const open = screen.getByRole('link', { name: 'Open macstudio' });
		expect(open.getAttribute('href')).toBe('https://ui-macstudio.example.com');
		expect(open.getAttribute('target')).toBe('_blank');
		expect(open.getAttribute('rel')).toContain('noopener');
		expect(screen.queryByRole('link', { name: 'Open macbookpro' })).toBeNull();
	});

	it('explains how to add machines when none are registered', async () => {
		stubMachines({ self: null, machines: [] });
		render(MachinesPanel);

		expect(await screen.findByText(/No machines are registered/)).toBeTruthy();
		expect(screen.getByText('Machines')).toBeTruthy();
	});

	it('says to rebuild when the running server has no machine list yet', async () => {
		stubMachines({}, 404);
		render(MachinesPanel);

		expect(await screen.findByRole('alert')).toBeTruthy();
		expect(screen.getByRole('alert').textContent).toContain('Rebuild and restart');
	});
});
