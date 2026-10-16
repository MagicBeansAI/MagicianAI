import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import CriticalDeliveryPanel from './CriticalDeliveryPanel.svelte';
import { parseCriticalDeliveryEnvelope, parseCriticalDeliveryStatus } from '$lib/stores/criticalDeliveryStore';

const envelope = {
	settings_path: '/notes/magician-config.yaml',
	settings: {
		enabled_channels: ['telegram'],
		policy: 'simultaneous',
		staged_fallback_secs: 45,
		push_enabled: true,
		quiet_hours: null
	},
	channels: [{ channel_type: 'telegram', owner_addresses: ['…01'], has_owner: true }],
	available_channels: ['kapso'],
	public_origin_configured: true,
	warnings: []
};

const status = {
	deliveries: [
		{
			id: 'd1',
			correlation_id: 'req-1',
			kind: 'request',
			destination: 'telegram:…01',
			channel_type: 'telegram',
			state: 'provider_accepted',
			attempts: 1,
			requested_at_ms: 1_790_078_400_000,
			enqueued_at_ms: 1_790_078_400_050,
			accepted_at_ms: 1_790_078_401_000,
			updated_at_ms: 1_790_078_401_000,
			reason: null,
			registrations: null
		}
	],
	latency: {
		request_to_enqueue: { samples: 1, p50_ms: 50, p95_ms: 50 },
		enqueue_to_acceptance: { samples: 1, p50_ms: 950, p95_ms: 950 }
	},
	channels_last_claimed_ms: { telegram: 1_790_078_400_500 }
};

afterEach(() => cleanup());

describe('CriticalDeliveryPanel', () => {
	it('renders the enabled channels with masked addresses, status and never sends on load', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/settings/critical-delivery', handle: () => jsonResponse(envelope) },
			{ method: 'GET', match: '/hitl/deliveries', handle: () => jsonResponse(status) }
		]);
		render(CriticalDeliveryPanel);
		expect(await screen.findByText('telegram')).toBeTruthy();
		expect(screen.getByText('…01')).toBeTruthy();
		expect(screen.getByRole('checkbox', { name: 'Enable kapso' })).toBeTruthy();
		expect(await screen.findByText('provider_accepted')).toBeTruthy();
		expect(screen.getByText(/p50 950 ms/)).toBeTruthy();
		expect(calls.every((call) => call.method === 'GET')).toBe(true);
	});

	it('saves the edited section without sending and only the test button posts a test', async () => {
		let saved: unknown = null;
		let tested = 0;
		installFetchMock([
			{ method: 'GET', match: '/settings/critical-delivery', handle: () => jsonResponse(envelope) },
			{ method: 'GET', match: '/hitl/deliveries', handle: () => jsonResponse(status) },
			{
				method: 'PUT',
				match: '/settings/critical-delivery',
				handle: (call) => {
					saved = JSON.parse(String(call.init?.body));
					return jsonResponse({
						...envelope,
						settings: { ...envelope.settings, enabled_channels: ['telegram', 'kapso'], policy: 'staged' },
						reload_applied: true
					});
				}
			},
			{
				method: 'POST',
				match: '/settings/critical-delivery/test',
				handle: () => {
					tested += 1;
					return jsonResponse({ correlation_id: 'test-1', destinations: 2, deliveries: [] }, { status: 202 });
				}
			}
		]);
		render(CriticalDeliveryPanel);
		const user = userEvent.setup();
		await user.click(await screen.findByRole('checkbox', { name: 'Enable kapso' }));
		await user.click(screen.getByRole('radio', { name: /Alert the first/ }));
		await user.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(saved).not.toBeNull());
		const body = saved as { critical_delivery: { enabled_channels: string[]; policy: string; quiet_hours?: unknown } };
		expect(body.critical_delivery.enabled_channels).toEqual(['telegram', 'kapso']);
		expect(body.critical_delivery.policy).toBe('staged');
		expect(body.critical_delivery.quiet_hours).toBeUndefined();
		expect(tested).toBe(0);
		await user.click(screen.getByRole('button', { name: 'Send test alert' }));
		await waitFor(() => expect(tested).toBe(1));
	});

	it('parses envelopes and status tolerantly', () => {
		expect(parseCriticalDeliveryEnvelope(null)).toBeNull();
		const parsed = parseCriticalDeliveryEnvelope({ settings_path: '/x', settings: { policy: 'weird' } });
		expect(parsed?.settings.policy).toBe('simultaneous');
		expect(parsed?.settings.staged_fallback_secs).toBe(45);
		expect(parseCriticalDeliveryStatus({ deliveries: [{ id: 'a' }], latency: {} })?.deliveries).toEqual([]);
	});
});
