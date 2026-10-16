import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const client = vi.hoisted(() => ({
	fetchWebPushConfig: vi.fn(),
	readWebPushState: vi.fn(),
	enableWebPush: vi.fn(),
	disableWebPush: vi.fn()
}));
vi.mock('$lib/webPush/client', () => client);

import BrowserNotificationsPanel from './BrowserNotificationsPanel.svelte';

const CONFIG = { enabled: true, vapid_public_key: 'AQID' };

beforeEach(() => {
	for (const fn of Object.values(client)) fn.mockReset();
	client.fetchWebPushConfig.mockResolvedValue(CONFIG);
});

afterEach(() => cleanup());

describe('BrowserNotificationsPanel', () => {
	it('turns browser notifications on from the Off state', async () => {
		client.readWebPushState.mockResolvedValue('off');
		client.enableWebPush.mockResolvedValue('on');
		render(BrowserNotificationsPanel);

		const button = await screen.findByRole('button', { name: 'Turn on browser notifications' });
		await userEvent.click(button);

		expect(client.enableWebPush).toHaveBeenCalledWith(CONFIG);
		await waitFor(() => expect(screen.getByTestId('browser-notifications-state')).toHaveTextContent('On'));
		expect(screen.getByRole('button', { name: 'Turn off' })).toBeInTheDocument();
	});

	it('turns them off', async () => {
		client.readWebPushState.mockResolvedValue('on');
		client.disableWebPush.mockResolvedValue('off');
		render(BrowserNotificationsPanel);

		await userEvent.click(await screen.findByRole('button', { name: 'Turn off' }));

		expect(client.disableWebPush).toHaveBeenCalled();
		await waitFor(() => expect(screen.getByTestId('browser-notifications-state')).toHaveTextContent('Off'));
	});

	it('explains a server without web push and offers no button', async () => {
		client.fetchWebPushConfig.mockResolvedValue({ enabled: false, vapid_public_key: null });
		client.readWebPushState.mockResolvedValue('server-disabled');
		render(BrowserNotificationsPanel);

		expect(await screen.findByText(/not configured on this Magician server/)).toBeInTheDocument();
		expect(screen.queryByRole('button')).toBeNull();
	});

	it('explains blocked permission', async () => {
		client.readWebPushState.mockResolvedValue('blocked');
		render(BrowserNotificationsPanel);
		expect(await screen.findByText(/blocked for this site/)).toBeInTheDocument();
		expect(screen.queryByRole('button')).toBeNull();
	});

	it('shows an enable failure', async () => {
		client.readWebPushState.mockResolvedValue('off');
		client.enableWebPush.mockRejectedValue(new Error('Could not register this browser (500)'));
		render(BrowserNotificationsPanel);
		await userEvent.click(await screen.findByRole('button', { name: 'Turn on browser notifications' }));
		expect(await screen.findByRole('alert')).toHaveTextContent('Could not register this browser (500)');
	});
});
