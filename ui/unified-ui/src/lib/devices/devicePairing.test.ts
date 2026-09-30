import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	beginDeviceEnrollment,
	completedEnrollmentDevice,
	listPairedDevices,
	pairedDeviceSnapshot,
	type PairedDevice,
	unpairDevice
} from './devicePairing';

afterEach(() => vi.unstubAllGlobals());

describe('device pairing API', () => {
	it('asks the server for an owner-scoped iOS enrollment using a server-owned route', async () => {
		const fetch = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ enrollment_id: 'e1', enrollment_uri: 'magican://connect', qr_svg: '<svg/>', expires_at_ms: 9, principal: 'p', workspace: 'w', client_kind: 'ios' }), { status: 200 })
		);
		vi.stubGlobal('fetch', fetch);
		await beginDeviceEnrollment('ios');
		expect(fetch).toHaveBeenCalledWith('/api/magician/v2/devices/enrollment', expect.objectContaining({
			method: 'POST', body: JSON.stringify({ client_kind: 'ios', connection_mode: 'remote' })
		}));
	});

	it('can request the same-Wi-Fi route without supplying an arbitrary address', async () => {
		const fetch = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ enrollment_id: 'e1' }), { status: 200 })
		);
		vi.stubGlobal('fetch', fetch);
		await beginDeviceEnrollment('android', 'same_wifi');
		expect(JSON.parse(String(fetch.mock.calls[0][1]?.body))).toEqual({
			client_kind: 'android', connection_mode: 'same_wifi'
		});
	});

	it('loads the scoped roster and encodes a device id during revocation', async () => {
		const fetch = vi.fn()
			.mockResolvedValueOnce(new Response(JSON.stringify({ principal: 'p', workspace: 'w', devices: [] }), { status: 200 }))
			.mockResolvedValueOnce(new Response(JSON.stringify({ removed: true }), { status: 200 }));
		vi.stubGlobal('fetch', fetch);
		expect((await listPairedDevices()).devices).toEqual([]);
		await unpairDevice('phone/one');
		expect(fetch.mock.calls[1][0]).toBe('/api/magician/v2/devices/phone%2Fone');
	});

	it('turns server capacity into an actionable message', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'too_many_pending_enrollments' }), { status: 429 })
		));
		await expect(beginDeviceEnrollment('android')).rejects.toThrow('Too many pairing codes');
	});

	it('does not invite another one-shot code while durable pairing is unavailable', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'pairing_authority_unavailable' }), { status: 503 })
		));
		await expect(beginDeviceEnrollment('ios')).rejects.toThrow(
			'Device pairing storage is unavailable'
		);
	});

	it('turns a stale running server route into a rebuild instruction', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'not_found' }), { status: 404 })
		));
		await expect(beginDeviceEnrollment('ios')).rejects.toThrow(
			'The running Magician does not expose the current device-enrollment route'
		);
	});

	it('does not mislabel a missing roster response as a missing enrollment route', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ error: 'not_found' }), { status: 404 })
		));
		await expect(listPairedDevices()).rejects.toThrow('Device pairing failed (HTTP 404).');
	});

	it('settles only for a matching roster delta without comparing browser and server clocks', () => {
		const device = (device_id: string, client_kind: 'ios' | 'android', paired_at_ms: number): PairedDevice => ({
			principal: 'owner', workspace: 'default', device_id, label: device_id,
			paired_at_ms, last_seen_ms: null, client_kind,
			capabilities: ['mobile_client'],
			automation_review_generation: 1
		});
		const baseline = pairedDeviceSnapshot([
			device('existing-ios', 'ios', 50_000),
			device('existing-android', 'android', 60_000)
		]);

		expect(completedEnrollmentDevice([
			device('existing-ios', 'ios', 50_000),
			device('new-android', 'android', 1)
		], 'ios', baseline)).toBeUndefined();
		expect(completedEnrollmentDevice([
			device('existing-ios', 'ios', 50_001)
		], 'ios', baseline)?.device_id).toBe('existing-ios');
		expect(completedEnrollmentDevice([
			device('new-ios', 'ios', 1)
		], 'ios', baseline)?.device_id).toBe('new-ios');
	});
});
