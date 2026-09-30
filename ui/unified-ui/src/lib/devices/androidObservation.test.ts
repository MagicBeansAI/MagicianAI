import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	ANDROID_OBSERVATION_DESKTOP_LINK,
	androidObservationDesktopLink,
	canOpenLocalAndroidObservation,
	findAndroidObservationDesktop,
	getAndroidAutomationTrustOptions,
	openAndroidObservationApproval,
	openLocalAndroidObservationApproval
} from './androidObservation';

afterEach(() => vi.unstubAllGlobals());

describe('Android observation desktop bridge', () => {
	it('uses the bounded desktop deep link only from a local desktop Magician page', () => {
		expect(canOpenLocalAndroidObservation(new URL('http://localhost:5173/settings'), 'Macintosh')).toBe(true);
		expect(canOpenLocalAndroidObservation(new URL('http://127.0.0.1:3002/settings'), 'Mac OS X')).toBe(true);
		expect(canOpenLocalAndroidObservation(new URL('http://localhost:3002/settings'), 'Windows NT 10.0')).toBe(true);
		expect(canOpenLocalAndroidObservation(new URL('http://[::1]:3002/settings'), 'X11; Linux x86_64')).toBe(true);
		expect(canOpenLocalAndroidObservation(new URL('https://connect.magican.ai/settings'), 'Macintosh')).toBe(false);
		expect(canOpenLocalAndroidObservation(new URL('http://localhost:5173/settings'), 'Linux; Android 16')).toBe(false);
		expect(canOpenLocalAndroidObservation(new URL('http://localhost:5173/settings'), 'iPhone; CPU iPhone OS 19_0 like Mac OS X')).toBe(false);
		expect(ANDROID_OBSERVATION_DESKTOP_LINK).toBe('magican://android-observation');
		expect(androidObservationDesktopLink('play_integrity')).toBe('magican://android-observation/play-integrity');
	});

	it('selects only a live desktop that advertises the bounded setup operation', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ devices: [
			{ device_id: 'linux', client_version: '1', capabilities: [{ capability: 'host.cua', operations: ['click'] }] },
			{ device_id: 'mac', client_version: '2', capabilities: [{ capability: 'host.android-observation', operations: ['open-settings'] }] }
		] }), { status: 200 })));
		expect(await findAndroidObservationDesktop()).toEqual({ deviceId: 'mac', clientVersion: '2' });
	});

	it('opens the trusted desktop approval through one typed Edge operation', async () => {
		const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({
			status: 'succeeded', payload: { opened: true }, error: null
		}), { status: 200 }));
		vi.stubGlobal('fetch', fetch);
		await openAndroidObservationApproval('desktop one', 'owner_pinned_private_build');
		const [url, init] = fetch.mock.calls[0];
		expect(url).toBe('/api/magician/v2/edge/devices/desktop%20one/invoke');
		expect(JSON.parse(String(init.body))).toMatchObject({
			capability: 'host.android-observation', operation: 'open-settings', payload: { trust_mode: 'owner_pinned_private_build' }
		});
	});

	it('shows readiness and missing prerequisites supplied by Magician', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ options: [{
			mode: 'owner_pinned_private_build', label: 'Private', description: 'Pinned', recommended: true,
			ready: false, missing: ['Android trust root (android_attestation_root_sha256)']
		}] }), { status: 200 })));
		expect(await getAndroidAutomationTrustOptions()).toEqual([expect.objectContaining({
			mode: 'owner_pinned_private_build', ready: false
		})]);
	});

	it('opens local approval through the bounded Desktop loopback action', async () => {
		const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ opened: true }), { status: 200 }));
		vi.stubGlobal('fetch', fetch);
		await openLocalAndroidObservationApproval('play_integrity');
		expect(fetch).toHaveBeenCalledWith(
			'http://127.0.0.1:3017/host/ui/android-observation?trust=play_integrity',
			{ method: 'POST' }
		);
	});
});
