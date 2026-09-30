import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import DevicePairingPanel from './DevicePairingPanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('DevicePairingPanel', () => {
	it('does not show the global configuration warning when the recommended private build is ready', async () => {
		const fetch = vi.fn(async (url: string) => {
			if (url.endsWith('/devices/apps-automation/trust-options')) {
				return new Response(JSON.stringify({
					options: [
						{
							mode: 'owner_pinned_private_build',
							label: 'Private / self-hosted build',
							description: 'Uses reviewed local pins.',
							recommended: true,
							ready: true,
							missing: []
						},
						{
							mode: 'play_integrity',
							label: 'Google Play release',
							description: 'Adds Google Play Integrity.',
							recommended: false,
							ready: false,
							missing: ['Google Cloud project']
						}
					]
				}), { status: 200 });
			}
			if (url.endsWith('/edge/devices')) {
				return new Response(JSON.stringify({ devices: [] }), { status: 200 });
			}
			return new Response(JSON.stringify({
				principal: 'owner', workspace: 'default', devices: [], pairing_available: true
			}), { status: 200 });
		});
		vi.stubGlobal('fetch', fetch);

		render(DevicePairingPanel);

		expect(await screen.findByText(/Private \/ self-hosted build/)).toBeInTheDocument();
		expect(await screen.findByText('Needs: Google Cloud project')).toBeInTheDocument();
		expect(screen.queryByText(/Missing values belong under/)).not.toBeInTheDocument();
	});

	it('distinguishes unavailable pairing storage from an empty roster and recovers on refresh', async () => {
		let available = false;
		const fetch = vi.fn(async () => new Response(JSON.stringify({
			principal: 'owner', workspace: 'default', devices: [], pairing_available: available
		}), { status: 200 }));
		vi.stubGlobal('fetch', fetch);
		render(DevicePairingPanel);
		expect(await screen.findByRole('alert')).toHaveTextContent('Device pairing storage is unavailable');
		expect(screen.queryByText('No mobile device is connected yet.')).not.toBeInTheDocument();
		for (const name of ['Connect iPhone', 'Connect Android']) {
			expect(screen.getByRole('button', { name })).toBeDisabled();
		}
		available = true;
		await fireEvent.click(screen.getByRole('button', { name: 'Refresh' }));
		expect(await screen.findByText('No mobile device is connected yet.')).toBeInTheDocument();
		expect(screen.queryByRole('alert')).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Connect Android' })).toBeEnabled();
	});

	it('shows the current iPhone without probing retired Android owner routes', async () => {
		const fetch = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({
				principal: 'owner',
				workspace: 'default',
				devices: [{
					principal: 'owner',
					workspace: 'default',
					device_id: 'ios-current',
					label: 'Current iPhone',
					paired_at_ms: 100,
					last_seen_ms: 200,
					client_kind: 'ios',
					capabilities: ['mobile_client'],
					automation_review_generation: 0
				}]
			}), { status: 200, headers: { 'Content-Type': 'application/json' } })
		);
		vi.stubGlobal('fetch', fetch);

		render(DevicePairingPanel);

		expect(await screen.findByText('Current iPhone')).toBeInTheDocument();
		expect(screen.queryByText('No mobile device is connected yet.')).not.toBeInTheDocument();
		expect(screen.queryByRole('alert')).not.toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Connect iPhone' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Connect Android' })).toBeInTheDocument();
		expect(screen.getByText(/Manage Android screen observation here/)).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Set up or manage' })).toBeDisabled();

		// devices, the device policy (verification-code permissions), edge status,
		// Android observation — never a retired owner route.
		await waitFor(() => expect(fetch).toHaveBeenCalledTimes(4));
		expect(fetch).toHaveBeenCalledWith('/api/magician/v2/devices', {
			headers: { Accept: 'application/json' }
		});
		expect(fetch.mock.calls.some(([url]) => String(url).includes('/devices/policy'))).toBe(true);
		expect(fetch.mock.calls.some(([url]) => String(url).includes('/devices/apps-automation/trust-options'))).toBe(true);
		expect(fetch.mock.calls.some(([url]) => String(url).includes('/enrollment/exchange'))).toBe(false);
	});

	// Secure HITL P6: an Android phone can be permitted as a verification-code
	// source; the grant is separate from pairing and written immediately.
	it('lets the owner permit an Android phone as a verification-code source', async () => {
		let permitted: unknown = null;
		const fetch = vi.fn(async (url: string, init?: RequestInit) => {
			const path = String(url);
			if (path.endsWith('/devices/policy/verification-codes')) {
				permitted = JSON.parse(String(init?.body));
				return new Response(JSON.stringify({ verification_code_devices: ['pixel-1'] }), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			if (path.endsWith('/devices/policy')) {
				return new Response(JSON.stringify({ screenshot_policy: 'allow', verification_code_devices: [] }), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			if (path.endsWith('/api/magician/v2/devices')) {
				return new Response(JSON.stringify({
					principal: 'owner',
					workspace: 'default',
					devices: [{
						principal: 'owner', workspace: 'default', device_id: 'pixel-1', label: 'Pixel',
						paired_at_ms: 100, last_seen_ms: 200, client_kind: 'android',
						capabilities: ['mobile_client'], automation_review_generation: 0
					}]
				}), { status: 200, headers: { 'Content-Type': 'application/json' } });
			}
			return new Response(JSON.stringify({}), { status: 200, headers: { 'Content-Type': 'application/json' } });
		});
		vi.stubGlobal('fetch', fetch);
		render(DevicePairingPanel);
		const toggle = await screen.findByRole('checkbox', { name: 'Use Pixel for verification codes' });
		expect(toggle).not.toBeChecked();
		await userEvent.setup().click(toggle);
		await waitFor(() => expect(permitted).toEqual({ device_id: 'pixel-1', permitted: true }));
		await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Use Pixel for verification codes' })).toBeChecked());
	});

	it.each([
		['ios', 'iPhone'],
		['android', 'Android']
	] as const)('creates and renews a %s companion code through ordinary enrollment', async (clientKind, label) => {
		const fetch = vi.fn(async (url: string, init?: RequestInit) => {
			if (init?.method === 'POST') {
				return new Response(JSON.stringify({
					enrollment_id: 'test-enrollment',
					enrollment_uri: 'magican://connect',
					qr_svg: '<svg></svg>',
					expires_at_ms: 0,
					principal: 'owner', workspace: 'default', client_kind: clientKind,
					connection_mode: 'same_wifi', origin: 'http://192.168.1.20:3002'
				}), { status: 200 });
			}
			return new Response(JSON.stringify(init?.method === 'DELETE'
				? { cancelled: true }
				: {
					principal: 'owner', workspace: 'default', devices: [],
					connection_options: {
						same_wifi: 'http://192.168.1.20:3002',
						remote: 'https://connect.magican.ai'
					}
				}), { status: 200 });
		});
		vi.stubGlobal('fetch', fetch);
		render(DevicePairingPanel);
		await screen.findByText('No mobile device is connected yet.');
		await fireEvent.click(screen.getByRole('button', { name: `Connect ${label}` }));
		expect(screen.getByText('Where will this phone use Magican?')).toBeInTheDocument();
		expect(screen.getByText(/localhost.*phone itself/)).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: /Same Wi-Fi · this computer/ }));
		expect(await screen.findByLabelText(`${label} connection QR code`)).toBeInTheDocument();
		expect(screen.getByText('http://192.168.1.20:3002')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Create a new code' }));
		await waitFor(() => expect(fetch.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(2));
		for (const [url, init] of fetch.mock.calls.filter(([, init]) => init?.method === 'POST')) {
			expect(url).toBe('/api/magician/v2/devices/enrollment');
			expect(JSON.parse(String(init?.body))).toEqual({
				client_kind: clientKind,
				connection_mode: 'same_wifi'
			});
		}
		expect(fetch.mock.calls.some(([url]) => url.includes('/enrollment/exchange'))).toBe(false);
	});

	it('creates a browser-to-desktop Edge link without exposing a QR workflow', async () => {
		const enrollmentUri = 'magican://connect?base=https%3A%2F%2Fconnect.magican.ai&kind=desktop&id=e-1&secret=s-1';
		const fetch = vi.fn(async (_url: string, init?: RequestInit) => {
			if (init?.method === 'POST') {
				return new Response(JSON.stringify({
					enrollment_id: 'desktop-enrollment', enrollment_uri: enrollmentUri,
					qr_svg: '<svg></svg>', expires_at_ms: Date.now() + 300_000,
					principal: 'owner', workspace: 'default', client_kind: 'desktop',
					connection_mode: 'remote', origin: 'https://connect.magican.ai'
				}), { status: 200 });
			}
			if (String(_url).endsWith('/devices/apps-automation/trust-options')) {
				return new Response(JSON.stringify({ options: [] }), { status: 200 });
			}
			if (String(_url).endsWith('/edge/devices')) {
				return new Response(JSON.stringify({ devices: [] }), { status: 200 });
			}
			return new Response(JSON.stringify({
				principal: 'owner', workspace: 'default', devices: [], pairing_available: true,
				connection_options: { same_wifi: null, remote: 'https://connect.magican.ai' }
			}), { status: 200 });
		});
		vi.stubGlobal('fetch', fetch);

		render(DevicePairingPanel);
		await fireEvent.click(await screen.findByRole('button', { name: 'Connect Desktop Edge' }));

		const openLink = await screen.findByRole('link', { name: 'Open Magican Desktop' });
		expect(openLink).toHaveAttribute('href', enrollmentUri);
		expect(screen.getByText('Authorize the installed Magican Desktop')).toBeInTheDocument();
		expect(screen.queryByLabelText(/connection QR code/)).not.toBeInTheDocument();
		expect(JSON.parse(String(fetch.mock.calls.find(([, init]) => init?.method === 'POST')?.[1]?.body))).toEqual({
			client_kind: 'desktop', connection_mode: 'remote'
		});
	});
});
